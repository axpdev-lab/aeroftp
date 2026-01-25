//! OneDrive Storage Provider
//!
//! Implements StorageProvider for Microsoft OneDrive using the Microsoft Graph API.
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

/// Microsoft Graph API base URL
const GRAPH_API_BASE: &str = "https://graph.microsoft.com/v1.0";

/// OneDrive item metadata (fields needed for API response deserialization)
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct DriveItem {
    id: String,
    name: String,
    #[serde(default)]
    size: u64,
    last_modified_date_time: Option<String>,
    #[serde(default)]
    folder: Option<FolderFacet>,
    #[serde(default)]
    file: Option<FileFacet>,
    #[serde(default)]
    parent_reference: Option<ParentReference>,
    #[serde(rename = "@microsoft.graph.downloadUrl")]
    download_url: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct FolderFacet {
    child_count: Option<i32>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct FileFacet {
    mime_type: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct ParentReference {
    drive_id: Option<String>,
    id: Option<String>,
    path: Option<String>,
}

/// List children response
#[derive(Debug, Deserialize)]
struct ChildrenResponse {
    value: Vec<DriveItem>,
    #[serde(rename = "@odata.nextLink")]
    next_link: Option<String>,
}

/// OneDrive provider configuration
#[derive(Debug, Clone)]
pub struct OneDriveConfig {
    pub client_id: String,
    pub client_secret: String,
}

impl OneDriveConfig {
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

/// OneDrive Storage Provider
pub struct OneDriveProvider {
    config: OneDriveConfig,
    oauth_manager: OAuth2Manager,
    client: reqwest::Client,
    connected: bool,
    current_path: String,
    current_item_id: String,
    /// Cache: path -> item_id
    path_cache: HashMap<String, String>,
}

impl OneDriveProvider {
    pub fn new(config: OneDriveConfig) -> Self {
        Self {
            config,
            oauth_manager: OAuth2Manager::new(),
            client: reqwest::Client::new(),
            connected: false,
            current_path: "/".to_string(),
            current_item_id: "root".to_string(),
            path_cache: HashMap::new(),
        }
    }

    /// Get OAuth config
    fn oauth_config(&self) -> OAuthConfig {
        OAuthConfig::onedrive(&self.config.client_id, &self.config.client_secret)
    }

    /// Get authorization header
    async fn auth_header(&self) -> Result<HeaderValue, ProviderError> {
        let token = self.oauth_manager.get_valid_token(&self.oauth_config()).await?;
        HeaderValue::from_str(&format!("Bearer {}", token))
            .map_err(|e| ProviderError::Other(format!("Invalid token: {}", e)))
    }

    /// Check if authenticated
    pub fn is_authenticated(&self) -> bool {
        self.oauth_manager.has_tokens(OAuthProvider::OneDrive)
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

    /// Build path for Graph API
    fn api_path(&self, path: &str) -> String {
        let clean = path.trim_matches('/');
        if clean.is_empty() {
            format!("{}/me/drive/root", GRAPH_API_BASE)
        } else {
            format!("{}/me/drive/root:/{}", GRAPH_API_BASE, clean)
        }
    }

    /// Build path for item ID
    fn api_item(&self, item_id: &str) -> String {
        if item_id == "root" {
            format!("{}/me/drive/root", GRAPH_API_BASE)
        } else {
            format!("{}/me/drive/items/{}", GRAPH_API_BASE, item_id)
        }
    }

    /// Convert DriveItem to RemoteEntry
    fn to_remote_entry(&self, item: &DriveItem, parent_path: &str) -> RemoteEntry {
        let is_dir = item.folder.is_some();
        let path = if parent_path == "/" {
            format!("/{}", item.name)
        } else {
            format!("{}/{}", parent_path.trim_end_matches('/'), item.name)
        };

        let mut metadata = HashMap::new();
        metadata.insert("id".to_string(), item.id.clone());
        if let Some(ref url) = item.download_url {
            metadata.insert("downloadUrl".to_string(), url.clone());
        }

        RemoteEntry {
            name: item.name.clone(),
            path,
            is_dir,
            size: item.size,
            modified: item.last_modified_date_time.clone(),
            permissions: None,
            owner: None,
            group: None,
            is_symlink: false,
            link_target: None,
            mime_type: item.file.as_ref().and_then(|f| f.mime_type.clone()),
            metadata,
        }
    }

    /// Get item by path
    async fn get_item(&self, path: &str) -> Result<DriveItem, ProviderError> {
        let url = self.api_path(path);
        
        let response = self.client
            .get(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if response.status().as_u16() == 404 {
            return Err(ProviderError::NotFound(path.to_string()));
        }

        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!("API error: {}", text)));
        }

        response.json().await
            .map_err(|e| ProviderError::Other(format!("Parse error: {}", e)))
    }

    /// Get item by ID
    async fn get_item_by_id(&self, item_id: &str) -> Result<DriveItem, ProviderError> {
        let url = self.api_item(item_id);
        
        let response = self.client
            .get(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            return Err(ProviderError::NotFound(item_id.to_string()));
        }

        response.json().await
            .map_err(|e| ProviderError::Other(format!("Parse error: {}", e)))
    }

    /// List children with pagination
    async fn list_children(&self, item_id: &str) -> Result<Vec<DriveItem>, ProviderError> {
        let mut all_items = Vec::new();
        let mut url = format!("{}/children", self.api_item(item_id));

        loop {
            let response = self.client
                .get(&url)
                .header(AUTHORIZATION, self.auth_header().await?)
                .send()
                .await
                .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

            if !response.status().is_success() {
                let text = response.text().await.unwrap_or_default();
                return Err(ProviderError::Other(format!("List error: {}", text)));
            }

            let result: ChildrenResponse = response.json().await
                .map_err(|e| ProviderError::Other(format!("Parse error: {}", e)))?;

            all_items.extend(result.value);

            match result.next_link {
                Some(next) => url = next,
                None => break,
            }
        }

        Ok(all_items)
    }

    /// Resolve path to item ID
    async fn resolve_path(&mut self, path: &str) -> Result<String, ProviderError> {
        let path = path.trim_matches('/');
        
        if path.is_empty() {
            return Ok("root".to_string());
        }

        // Check cache
        if let Some(id) = self.path_cache.get(path) {
            return Ok(id.clone());
        }

        let item = self.get_item(path).await?;
        self.path_cache.insert(path.to_string(), item.id.clone());
        
        Ok(item.id)
    }
}

#[async_trait]
impl StorageProvider for OneDriveProvider {
    fn provider_type(&self) -> ProviderType {
        ProviderType::OneDrive
    }

    fn display_name(&self) -> String {
        "OneDrive".to_string()
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        if !self.is_authenticated() {
            return Err(ProviderError::AuthenticationFailed(
                "Not authenticated. Call start_auth() first.".to_string()
            ));
        }

        // Validate by getting drive info
        let url = format!("{}/me/drive", GRAPH_API_BASE);
        
        let response = self.client
            .get(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            return Err(ProviderError::AuthenticationFailed("Invalid token".to_string()));
        }

        self.connected = true;
        self.current_path = "/".to_string();
        self.current_item_id = "root".to_string();
        
        info!("Connected to OneDrive");
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.connected = false;
        self.path_cache.clear();
        info!("Disconnected from OneDrive");
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let (item_id, parent_path) = if path == "." || path.is_empty() {
            (self.current_item_id.clone(), self.current_path.clone())
        } else if path.starts_with('/') {
            let id = self.resolve_path(path).await?;
            (id, path.to_string())
        } else {
            let full_path = format!("{}/{}", self.current_path.trim_end_matches('/'), path);
            let id = self.resolve_path(&full_path).await?;
            (id, full_path)
        };

        let items = self.list_children(&item_id).await?;
        Ok(items.iter().map(|i| self.to_remote_entry(i, &parent_path)).collect())
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
            if parts.is_empty() {
                "/".to_string()
            } else {
                format!("/{}", parts.join("/"))
            }
        } else {
            format!("{}/{}", self.current_path.trim_end_matches('/'), path)
        };

        let item_id = self.resolve_path(&new_path).await?;
        
        // Verify it's a folder
        let item = self.get_item_by_id(&item_id).await?;
        if item.folder.is_none() {
            return Err(ProviderError::InvalidPath(format!("{} is not a folder", path)));
        }

        self.current_item_id = item_id;
        self.current_path = if new_path == "/" { "/".to_string() } else { new_path };
        
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
        let path = if remote_path.starts_with('/') {
            remote_path.to_string()
        } else {
            format!("{}/{}", self.current_path.trim_end_matches('/'), remote_path)
        };

        let item = self.get_item(&path).await?;
        
        // Get download URL
        let download_url = if let Some(url) = item.download_url {
            url
        } else {
            // Request content
            let url = format!("{}:/content", self.api_path(&path));
            
            let response = self.client
                .get(&url)
                .header(AUTHORIZATION, self.auth_header().await?)
                .send()
                .await
                .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

            // Follow redirect to get actual download URL
            if response.status().is_redirection() {
                response.headers()
                    .get("Location")
                    .and_then(|h| h.to_str().ok())
                    .map(String::from)
                    .ok_or_else(|| ProviderError::Other("No download URL".to_string()))?
            } else {
                return Err(ProviderError::Other("Cannot get download URL".to_string()));
            }
        };

        // Download content
        let response = self.client
            .get(&download_url)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        let bytes = response.bytes().await
            .map_err(|e| ProviderError::Other(format!("Read error: {}", e)))?;

        let _: () = tokio::fs::write(local_path, &bytes).await
            .map_err(|e| ProviderError::Other(format!("Write error: {}", e)))?;

        info!("Downloaded {} to {}", remote_path, local_path);
        Ok(())
    }

    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        let path = if remote_path.starts_with('/') {
            remote_path.to_string()
        } else {
            format!("{}/{}", self.current_path.trim_end_matches('/'), remote_path)
        };

        let url = format!("{}:/content", self.api_path(&path));
        
        let response = self.client
            .get(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        // OneDrive returns redirect, follow it
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

        let path = if remote_path.starts_with('/') {
            remote_path.to_string()
        } else {
            format!("{}/{}", self.current_path.trim_end_matches('/'), remote_path)
        };

        let url = format!("{}:/content", self.api_path(&path));
        
        let response = self.client
            .put(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header(CONTENT_TYPE, "application/octet-stream")
            .body(content)
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
        let (parent_path, folder_name) = if path.starts_with('/') {
            let p = path.trim_matches('/');
            if let Some(pos) = p.rfind('/') {
                (format!("/{}", &p[..pos]), &p[pos + 1..])
            } else {
                ("/".to_string(), p)
            }
        } else {
            (self.current_path.clone(), path)
        };

        let parent_id = self.resolve_path(&parent_path).await?;
        
        let body = serde_json::json!({
            "name": folder_name,
            "folder": {},
            "@microsoft.graph.conflictBehavior": "fail"
        });

        let url = format!("{}/children", self.api_item(&parent_id));
        
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
            return Err(ProviderError::Other(format!("mkdir failed: {}", text)));
        }

        info!("Created folder: {}", path);
        Ok(())
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        let full_path = if path.starts_with('/') {
            path.to_string()
        } else {
            format!("{}/{}", self.current_path.trim_end_matches('/'), path)
        };

        let item_id = self.resolve_path(&full_path).await?;
        let url = self.api_item(&item_id);
        
        let response = self.client
            .delete(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        // 204 No Content is success, 404 is also acceptable
        if !response.status().is_success() && response.status().as_u16() != 404 {
            return Err(ProviderError::Other(format!("Delete failed: {}", response.status())));
        }

        // Clear from cache
        self.path_cache.remove(&full_path.trim_matches('/').to_string());
        
        info!("Deleted: {}", path);
        Ok(())
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        self.delete(path).await
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        // OneDrive delete removes folders with contents
        self.delete(path).await
    }

    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let from_path = if from.starts_with('/') {
            from.to_string()
        } else {
            format!("{}/{}", self.current_path.trim_end_matches('/'), from)
        };

        let item_id = self.resolve_path(&from_path).await?;
        let new_name = to.rsplit('/').next().unwrap_or(to);
        
        let body = serde_json::json!({
            "name": new_name
        });

        let url = self.api_item(&item_id);
        
        let response = self.client
            .patch(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string())
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
        let full_path = if path.starts_with('/') {
            path.to_string()
        } else {
            format!("{}/{}", self.current_path.trim_end_matches('/'), path)
        };

        let item = self.get_item(&full_path).await?;
        
        let parent = full_path.rsplit_once('/').map(|(p, _)| p).unwrap_or("/");
        Ok(self.to_remote_entry(&item, parent))
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
        Ok("Microsoft OneDrive (Graph API v1.0)".to_string())
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
            path.to_string()
        } else {
            format!("{}/{}", self.current_path.trim_end_matches('/'), path)
        };

        // Resolve path to item ID
        let item_id = self.resolve_path(&full_path).await?;

        // Create a sharing link using Graph API
        // POST /me/drive/items/{item-id}/createLink
        let url = format!("{}/createLink", self.api_item(&item_id));
        
        let body = serde_json::json!({
            "type": "view",
            "scope": "anonymous"
        });

        let response = self.client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        let status = response.status();
        
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!("Failed to create share link: {} - {}", status, text)));
        }

        #[derive(Deserialize)]
        struct Link {
            #[serde(rename = "webUrl")]
            web_url: Option<String>,
        }
        #[derive(Deserialize)]
        struct CreateLinkResponse {
            link: Link,
        }

        let result: CreateLinkResponse = response.json().await
            .map_err(|e| ProviderError::Other(format!("Failed to parse response: {}", e)))?;

        let url = result.link.web_url
            .ok_or_else(|| ProviderError::Other("No share URL in response".to_string()))?;

        info!("Created share link for {}: {}", path, url);
        Ok(url)
    }
}
