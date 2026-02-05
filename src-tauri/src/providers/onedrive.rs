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
    StorageProvider, ProviderType, ProviderError, RemoteEntry, ProviderConfig, StorageInfo,
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
    /// Authenticated user email
    account_email: Option<String>,
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
            account_email: None,
        }
    }

    /// Get OAuth config
    fn oauth_config(&self) -> OAuthConfig {
        OAuthConfig::onedrive(&self.config.client_id, &self.config.client_secret)
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

    fn account_email(&self) -> Option<String> {
        self.account_email.clone()
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

        // Parse owner email from drive info
        if let Ok(body) = response.json::<serde_json::Value>().await {
            if let Some(email) = body["owner"]["user"]["email"].as_str() {
                self.account_email = Some(email.to_string());
            }
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

    fn supports_server_copy(&self) -> bool {
        true
    }

    async fn server_copy(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let from_path = if from.starts_with('/') {
            from.to_string()
        } else {
            format!("{}/{}", self.current_path.trim_end_matches('/'), from)
        };

        let from_id = self.resolve_path(&from_path).await?;

        // Resolve destination parent and name
        let to_path = to.trim_matches('/');
        let (to_parent, to_name) = if let Some(pos) = to_path.rfind('/') {
            (&to_path[..pos], &to_path[pos + 1..])
        } else {
            ("", to_path)
        };

        let to_parent_path = if to_parent.is_empty() {
            self.current_path.clone()
        } else if to_parent.starts_with('/') {
            to_parent.to_string()
        } else {
            format!("{}/{}", self.current_path.trim_end_matches('/'), to_parent)
        };

        let to_parent_id = self.resolve_path(&to_parent_path).await?;

        let body = serde_json::json!({
            "parentReference": { "id": to_parent_id },
            "name": to_name
        });

        let url = format!("{}/copy", self.api_item(&from_id));

        let response = self.client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        // OneDrive copy returns 202 Accepted (async operation)
        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!("Copy failed: {}", text)));
        }

        info!("Copied {} to {}", from, to);
        Ok(())
    }

    fn supports_find(&self) -> bool {
        true
    }

    async fn find(&mut self, path: &str, pattern: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let item_id = if path == "/" || path.is_empty() {
            "root".to_string()
        } else {
            self.resolve_path(path).await?
        };

        let url = format!(
            "{}/search(q='{}')",
            self.api_item(&item_id),
            urlencoding::encode(pattern)
        );

        let mut all_entries = Vec::new();
        let mut current_url = url;

        loop {
            let response = self.client
                .get(&current_url)
                .header(AUTHORIZATION, self.auth_header().await?)
                .send()
                .await
                .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

            if !response.status().is_success() {
                let text = response.text().await.unwrap_or_default();
                return Err(ProviderError::Other(format!("Search failed: {}", text)));
            }

            let result: ChildrenResponse = response.json().await
                .map_err(|e| ProviderError::Other(format!("Parse error: {}", e)))?;

            for item in &result.value {
                all_entries.push(self.to_remote_entry(item, path));
            }

            match result.next_link {
                Some(next) if all_entries.len() < 500 => current_url = next,
                _ => break,
            }
        }

        Ok(all_entries)
    }

    async fn storage_info(&mut self) -> Result<StorageInfo, ProviderError> {
        let url = format!("{}/me/drive", GRAPH_API_BASE);

        let response = self.client
            .get(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!("Drive info failed: {}", text)));
        }

        #[derive(Deserialize)]
        struct Quota {
            total: Option<u64>,
            used: Option<u64>,
            remaining: Option<u64>,
        }
        #[derive(Deserialize)]
        struct DriveInfo {
            quota: Option<Quota>,
        }

        let info: DriveInfo = response.json().await
            .map_err(|e| ProviderError::Other(format!("Parse error: {}", e)))?;

        let quota = info.quota.ok_or_else(|| ProviderError::Other("No quota info".to_string()))?;
        let total = quota.total.unwrap_or(0);
        let used = quota.used.unwrap_or(0);

        Ok(StorageInfo {
            used,
            total,
            free: quota.remaining.unwrap_or_else(|| total.saturating_sub(used)),
        })
    }

    fn supports_versions(&self) -> bool {
        true
    }

    async fn list_versions(&mut self, path: &str) -> Result<Vec<super::FileVersion>, ProviderError> {
        let item_id = self.resolve_path(path).await?;
        let url = format!("{}/versions", self.api_item(&item_id));

        let response = self.client
            .get(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!("List versions failed: {}", text)));
        }

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Identity {
            display_name: Option<String>,
        }
        #[derive(Deserialize)]
        struct User {
            user: Option<Identity>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Version {
            id: String,
            last_modified_date_time: Option<String>,
            size: Option<u64>,
            last_modified_by: Option<User>,
        }
        #[derive(Deserialize)]
        struct VersionList {
            value: Vec<Version>,
        }

        let list: VersionList = response.json().await
            .map_err(|e| ProviderError::Other(format!("Parse error: {}", e)))?;

        Ok(list.value.iter().map(|v| super::FileVersion {
            id: v.id.clone(),
            modified: v.last_modified_date_time.clone(),
            size: v.size.unwrap_or(0),
            modified_by: v.last_modified_by.as_ref()
                .and_then(|u| u.user.as_ref())
                .and_then(|i| i.display_name.clone()),
        }).collect())
    }

    async fn download_version(
        &mut self,
        path: &str,
        version_id: &str,
        local_path: &str,
    ) -> Result<(), ProviderError> {
        let item_id = self.resolve_path(path).await?;
        let url = format!("{}/versions/{}/content", self.api_item(&item_id), version_id);

        let response = self.client
            .get(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::TransferFailed(format!("Download version failed: {}", text)));
        }

        let bytes = response.bytes().await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        tokio::fs::write(local_path, &bytes).await
            .map_err(|e| ProviderError::IoError(e))?;

        Ok(())
    }

    async fn restore_version(&mut self, path: &str, version_id: &str) -> Result<(), ProviderError> {
        let item_id = self.resolve_path(path).await?;
        let url = format!("{}/versions/{}/restoreVersion", self.api_item(&item_id), version_id);

        let response = self.client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() && response.status().as_u16() != 204 {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!("Restore version failed: {}", text)));
        }

        info!("Restored {} to version {}", path, version_id);
        Ok(())
    }

    fn supports_thumbnails(&self) -> bool {
        true
    }

    async fn get_thumbnail(&mut self, path: &str) -> Result<String, ProviderError> {
        let item_id = self.resolve_path(path).await?;
        let url = format!("{}/thumbnails/0/medium/content", self.api_item(&item_id));

        let response = self.client
            .get(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            return Err(ProviderError::NotFound("No thumbnail available".to_string()));
        }

        let bytes = response.bytes().await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        Ok(format!("data:image/jpeg;base64,{}", base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes)))
    }

    fn supports_permissions(&self) -> bool {
        true
    }

    async fn list_permissions(&mut self, path: &str) -> Result<Vec<super::SharePermission>, ProviderError> {
        let item_id = self.resolve_path(path).await?;
        let url = format!("{}/permissions", self.api_item(&item_id));

        let response = self.client
            .get(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!("List permissions failed: {}", text)));
        }

        #[derive(Deserialize)]
        struct GrantedTo {
            user: Option<GrantedUser>,
        }
        #[derive(Deserialize)]
        struct GrantedUser {
            email: Option<String>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        #[allow(dead_code)]
        struct Permission {
            id: Option<String>,
            roles: Option<Vec<String>>,
            granted_to_v2: Option<GrantedTo>,
        }
        #[derive(Deserialize)]
        struct PermList {
            value: Vec<Permission>,
        }

        let list: PermList = response.json().await
            .map_err(|e| ProviderError::Other(format!("Parse error: {}", e)))?;

        Ok(list.value.iter().map(|p| {
            let role = p.roles.as_ref()
                .and_then(|r| r.first().cloned())
                .unwrap_or_else(|| "read".to_string());
            let target = p.granted_to_v2.as_ref()
                .and_then(|g| g.user.as_ref())
                .and_then(|u| u.email.clone())
                .unwrap_or_default();
            super::SharePermission {
                role,
                target_type: if target.is_empty() { "anyone".to_string() } else { "user".to_string() },
                target,
            }
        }).collect())
    }

    async fn add_permission(
        &mut self,
        path: &str,
        permission: &super::SharePermission,
    ) -> Result<(), ProviderError> {
        let item_id = self.resolve_path(path).await?;
        let url = format!("{}/invite", self.api_item(&item_id));

        let body = serde_json::json!({
            "recipients": [{
                "email": permission.target
            }],
            "roles": [permission.role],
            "requireSignIn": true
        });

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
            return Err(ProviderError::Other(format!("Add permission failed: {}", text)));
        }

        Ok(())
    }

    fn supports_resume(&self) -> bool {
        true
    }

    async fn resume_upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        _offset: u64,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        // OneDrive resumable upload via upload session
        let data = tokio::fs::read(local_path).await
            .map_err(|e| ProviderError::IoError(e))?;
        let total_size = data.len() as u64;

        let path_str = remote_path.trim_matches('/');
        let url = format!(
            "{}/me/drive/root:/{}:/createUploadSession",
            GRAPH_API_BASE, urlencoding::encode(path_str)
        );

        let body = serde_json::json!({
            "item": {
                "@microsoft.graph.conflictBehavior": "replace"
            }
        });

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
            return Err(ProviderError::TransferFailed(format!("Create upload session failed: {}", text)));
        }

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct UploadSession {
            upload_url: String,
        }

        let session: UploadSession = response.json().await
            .map_err(|e| ProviderError::Other(format!("Parse error: {}", e)))?;

        // Upload in 10MB chunks
        let chunk_size = 10 * 1024 * 1024usize;
        let mut offset = 0usize;

        while offset < data.len() {
            let end = std::cmp::min(offset + chunk_size, data.len());
            let chunk = &data[offset..end];

            let content_range = format!("bytes {}-{}/{}", offset, end - 1, total_size);

            let resp = self.client
                .put(&session.upload_url)
                .header("Content-Range", &content_range)
                .header("Content-Length", chunk.len().to_string())
                .body(chunk.to_vec())
                .send()
                .await
                .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

            let status = resp.status();
            if !status.is_success() && status.as_u16() != 202 {
                let text = resp.text().await.unwrap_or_default();
                return Err(ProviderError::TransferFailed(format!("Upload chunk failed ({}): {}", status, text)));
            }

            offset = end;
            if let Some(ref progress) = on_progress {
                progress(offset as u64, total_size);
            }
        }

        info!("Resumable upload completed: {}", remote_path);
        Ok(())
    }
}
