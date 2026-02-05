//! WebDAV Storage Provider
//!
//! Implementation of the StorageProvider trait for WebDAV protocol.
//! Compatible with Nextcloud, ownCloud, Synology, QNAP, and other WebDAV servers.
//!
//! WebDAV extends HTTP with methods like PROPFIND, MKCOL, MOVE, COPY, and DELETE
//! to provide full file system operations over HTTP/HTTPS.

use async_trait::async_trait;
use reqwest::{Client, Method, StatusCode};
use std::collections::HashMap;

use super::{
    StorageProvider, ProviderError, ProviderType, RemoteEntry, WebDavConfig,
};

/// Custom HTTP methods for WebDAV
mod webdav_methods {
    use reqwest::Method;
    
    pub fn propfind() -> Method {
        Method::from_bytes(b"PROPFIND").unwrap()
    }
    
    pub fn mkcol() -> Method {
        Method::from_bytes(b"MKCOL").unwrap()
    }
    
    #[allow(dead_code)]
    pub fn copy() -> Method {
        Method::from_bytes(b"COPY").unwrap()
    }
    
    pub fn move_method() -> Method {
        Method::from_bytes(b"MOVE").unwrap()
    }

    pub fn lock() -> Method {
        Method::from_bytes(b"LOCK").unwrap()
    }

    pub fn unlock() -> Method {
        Method::from_bytes(b"UNLOCK").unwrap()
    }
}

/// WebDAV Storage Provider
pub struct WebDavProvider {
    config: WebDavConfig,
    client: Client,
    current_path: String,
    connected: bool,
}

impl WebDavProvider {
    /// Create a new WebDAV provider with the given configuration
    pub fn new(config: WebDavConfig) -> Self {
        let client = Client::builder()
            .danger_accept_invalid_certs(false) // Set to true for self-signed certs if needed
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("Failed to create HTTP client");
        
        Self {
            config,
            client,
            current_path: "/".to_string(),
            connected: false,
        }
    }
    
    /// Build full URL for a path
    fn build_url(&self, path: &str) -> String {
        let base = self.config.url.trim_end_matches('/');
        let path = path.trim_start_matches('/');
        
        if path.is_empty() {
            base.to_string()
        } else {
            format!("{}/{}", base, path)
        }
    }
    
    /// Make an authenticated request
    async fn request(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        self.client
            .request(method, self.build_url(path))
            .basic_auth(&self.config.username, Some(&self.config.password))
    }
    
    /// Parse PROPFIND XML response into RemoteEntry list
    fn parse_propfind_response(&self, xml: &str, base_path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let mut entries = Vec::new();
        
        // Simple XML parsing for WebDAV multistatus response
        // In production, consider using quick-xml for proper parsing
        
        // Find all response elements with various namespace prefixes:
        // <d:response>, <D:response>, <DAV:response>, <response>, <lp1:response>, etc.
        let response_pattern = regex::Regex::new(r"(?s)<(?:[a-zA-Z0-9_]+:)?response[^>]*>(.*?)</(?:[a-zA-Z0-9_]+:)?response>")
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;
        
        tracing::info!("[WebDAV] Parsing XML with base_path: {}, url: {}", base_path, self.config.url);

        let match_count = response_pattern.captures_iter(xml).count();
        tracing::info!("[WebDAV] Found {} response elements in XML", match_count);
        
        for cap in response_pattern.captures_iter(xml) {
            if let Some(response_content) = cap.get(1) {
                let content = response_content.as_str();
                tracing::info!("[WebDAV] Processing response element, content length: {}", content.len());
                
                // Extract href
                let href = self.extract_tag_content(content, "href");
                tracing::info!("[WebDAV] Extracted href: {:?}", href);
                if href.is_none() {
                    tracing::warn!("[WebDAV] No href found in response element");
                    continue;
                }
                let href = href.unwrap();
                
                // Skip the base path itself (first response is usually the directory itself)
                let decoded_href = urlencoding::decode(&href).unwrap_or_else(|_| href.clone().into());
                let clean_path = decoded_href.trim_end_matches('/');
                let base_clean = base_path.trim_end_matches('/');
                let url_clean = self.config.url.trim_end_matches('/');
                
                tracing::info!("[WebDAV] clean_path: {}, base_clean: {}, url_clean: {}", clean_path, base_clean, url_clean);
                
                // Check if this is the directory itself (not a child entry)
                // Handle various formats: full URL, absolute path, or relative path
                // Extract URL path component (e.g., "/dav" from "https://host/dav/")
                let url_path_clean = url_clean
                    .find("://")
                    .and_then(|i| url_clean[i + 3..].find('/').map(|j| i + 3 + j))
                    .map(|i| url_clean[i..].trim_end_matches('/'))
                    .unwrap_or("");

                let is_self_reference = clean_path == base_clean
                    || clean_path == url_clean
                    || (!base_clean.is_empty() && clean_path.ends_with(base_clean))
                    || (!base_clean.is_empty() && clean_path.ends_with(&format!("/{}", base_clean.trim_start_matches('/'))))
                    || (!base_clean.is_empty() && base_clean != "/" && url_clean.ends_with(clean_path))
                    || (!url_path_clean.is_empty() && clean_path == url_path_clean);
                
                if is_self_reference {
                    tracing::debug!("[WebDAV] Skipping self-reference: {}", clean_path);
                    continue;
                }
                
                // Check if it's a collection (directory) - handle various namespace formats
                // Self-closing: <d:collection/>, <D:collection />, <collection/>
                // Open+close: <d:collection></d:collection>, <D:collection></D:collection>
                // DriveHQ uses <a:iscollection>1</a:iscollection>
                // Check if resourcetype contains "collection" keyword (any format):
                // Koofr: <D:collection xmlns:D="DAV:"/>  (has attributes before />)
                // Nextcloud: <d:collection/>
                // Generic: <collection/>, <D:collection></D:collection>
                // Strategy: find <resourcetype>...</resourcetype> block, check for "collection" inside
                let content_lower = content.to_lowercase();
                // Find <resourcetype>...</resourcetype> block and check for "collection" inside
                // Closing tag may have namespace: </d:resourcetype>, </D:resourcetype>, </resourcetype>
                let has_collection_in_resourcetype = {
                    if let Some(rt_start) = content_lower.find("resourcetype>") {
                        let after_rt = rt_start + "resourcetype>".len();
                        // Find closing </...resourcetype> - search for next "resourcetype>" after opening
                        if let Some(rt_end_rel) = content_lower[after_rt..].find("resourcetype>") {
                            let rt_content = &content_lower[after_rt..after_rt + rt_end_rel];
                            rt_content.contains("collection")
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                };
                let is_dir_by_iscollection = content_lower.contains("iscollection>1</");
                let href_ends_slash = href.ends_with('/');
                let is_dir = has_collection_in_resourcetype || is_dir_by_iscollection || href_ends_slash;
                
                // Extract content length
                let size: u64 = self.extract_tag_content(content, "getcontentlength")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                
                // Extract last modified
                let modified = self.extract_tag_content(content, "getlastmodified");
                
                // Extract name from href
                let name = decoded_href
                    .trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .unwrap_or("")
                    .to_string();
                
                if name.is_empty() || name == "." || name == ".." {
                    continue;
                }
                
                // Build path relative to current directory
                let path = if self.current_path.ends_with('/') {
                    format!("{}{}", self.current_path, name)
                } else {
                    format!("{}/{}", self.current_path, name)
                };
                
                // Extract content type for MIME
                let mime_type = self.extract_tag_content(content, "getcontenttype");
                
                // Extract etag
                let etag = self.extract_tag_content(content, "getetag");
                
                let mut metadata = HashMap::new();
                if let Some(etag) = etag {
                    metadata.insert("etag".to_string(), etag);
                }
                
                entries.push(RemoteEntry {
                    name,
                    path,
                    is_dir,
                    size,
                    modified,
                    permissions: None,
                    owner: None,
                    group: None,
                    is_symlink: false,
                    link_target: None,
                    mime_type,
                    metadata,
                });
            }
        }
        
        Ok(entries)
    }
    
    /// Extract content from an XML tag (handles various namespace prefixes)
    fn extract_tag_content(&self, xml: &str, tag: &str) -> Option<String> {
        // Try various namespace prefixes used by different WebDAV servers
        // DriveHQ uses 'a:', Nextcloud uses 'd:', some use 'D:', etc.
        let patterns = [
            // Generic pattern: any single-letter or word prefix (handles a:, d:, D:, DAV:, lp1:, etc.)
            format!(r"<[a-zA-Z][a-zA-Z0-9]*:{}[^>]*>([^<]*)</[a-zA-Z][a-zA-Z0-9]*:{}>", tag, tag),
            // No prefix
            format!(r"<{}[^>]*>([^<]*)</{}>", tag, tag),
            // CDATA content (DriveHQ uses CDATA for displayname)
            format!(r"<[a-zA-Z][a-zA-Z0-9]*:{}[^>]*><!\[CDATA\[(.*?)\]\]></[a-zA-Z][a-zA-Z0-9]*:{}>", tag, tag),
            format!(r"<{}[^>]*><!\[CDATA\[(.*?)\]\]></{}>", tag, tag),
        ];
        
        for pattern in patterns {
            if let Ok(re) = regex::Regex::new(&pattern) {
                if let Some(cap) = re.captures(xml) {
                    if let Some(content) = cap.get(1) {
                        let text = content.as_str().trim().to_string();
                        if !text.is_empty() {
                            return Some(text);
                        }
                    }
                }
            }
        }
        
        None
    }
}

#[async_trait]
impl StorageProvider for WebDavProvider {
    fn provider_type(&self) -> ProviderType {
        ProviderType::WebDav
    }
    
    fn display_name(&self) -> String {
        format!("{}@{}", self.config.username, 
            self.config.url
                .replace("https://", "")
                .replace("http://", "")
                .split('/')
                .next()
                .unwrap_or(&self.config.url)
        )
    }
    
    async fn connect(&mut self) -> Result<(), ProviderError> {
        // Test connection with a PROPFIND on the root
        let response = self.request(webdav_methods::propfind(), "/").await
            .header("Depth", "0")
            .header("Content-Type", "application/xml")
            .body(r#"<?xml version="1.0" encoding="utf-8"?>
                <d:propfind xmlns:d="DAV:">
                    <d:prop>
                        <d:resourcetype/>
                    </d:prop>
                </d:propfind>"#)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;
        
        match response.status() {
            StatusCode::OK | StatusCode::MULTI_STATUS => {
                self.connected = true;
                
                // Navigate to initial path if specified
                if let Some(ref initial_path) = self.config.initial_path {
                    if !initial_path.is_empty() {
                        self.current_path = initial_path.clone();
                    }
                }
                
                Ok(())
            }
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                Err(ProviderError::AuthenticationFailed("Invalid credentials".to_string()))
            }
            status => {
                Err(ProviderError::ConnectionFailed(format!("Server returned status: {}", status)))
            }
        }
    }
    
    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.connected = false;
        Ok(())
    }
    
    fn is_connected(&self) -> bool {
        self.connected
    }
    
    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        
        let list_path = if path.is_empty() || path == "." {
            self.current_path.clone()
        } else {
            path.to_string()
        };
        
        tracing::info!("[WebDAV] Listing path: {}", list_path);
        
        let response = self.request(webdav_methods::propfind(), &list_path).await
            .header("Depth", "1")
            .header("Content-Type", "application/xml")
            .body(r#"<?xml version="1.0" encoding="utf-8"?>
                <d:propfind xmlns:d="DAV:">
                    <d:prop>
                        <d:resourcetype/>
                        <d:getcontentlength/>
                        <d:getlastmodified/>
                        <d:getcontenttype/>
                        <d:getetag/>
                        <d:displayname/>
                    </d:prop>
                </d:propfind>"#)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
        
        let status = response.status();
        tracing::info!("[WebDAV] List response status: {}", status);
        
        match status {
            StatusCode::OK | StatusCode::MULTI_STATUS => {
                let xml = response.text().await
                    .map_err(|e| ProviderError::ParseError(e.to_string()))?;
                
                tracing::info!("[WebDAV] Response XML length: {} bytes", xml.len());
                // Log full XML for debugging WebDAV issues
                tracing::info!("[WebDAV] Full XML response:\n{}", xml);
                
                let entries = self.parse_propfind_response(&xml, &list_path)?;
                tracing::info!("[WebDAV] Parsed {} entries", entries.len());
                Ok(entries)
            }
            StatusCode::NOT_FOUND => {
                tracing::warn!("[WebDAV] Path not found: {}", list_path);
                Err(ProviderError::NotFound(list_path))
            }
            StatusCode::UNAUTHORIZED => {
                self.connected = false;
                tracing::error!("[WebDAV] Unauthorized - session expired");
                Err(ProviderError::AuthenticationFailed("Session expired".to_string()))
            }
            status => {
                tracing::error!("[WebDAV] Server error: {}", status);
                Err(ProviderError::ServerError(format!("Server returned status: {}", status)))
            }
        }
    }
    
    async fn pwd(&mut self) -> Result<String, ProviderError> {
        Ok(self.current_path.clone())
    }
    
    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        
        // Verify the path exists and is a directory
        let response = self.request(webdav_methods::propfind(), path).await
            .header("Depth", "0")
            .header("Content-Type", "application/xml")
            .body(r#"<?xml version="1.0" encoding="utf-8"?>
                <d:propfind xmlns:d="DAV:">
                    <d:prop>
                        <d:resourcetype/>
                    </d:prop>
                </d:propfind>"#)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
        
        match response.status() {
            StatusCode::OK | StatusCode::MULTI_STATUS => {
                let xml = response.text().await
                    .map_err(|e| ProviderError::ParseError(e.to_string()))?;
                
                // Check for collection - search for "collection" inside resourcetype block
                let xml_lower = xml.to_lowercase();
                let is_collection = if let Some(rt_start) = xml_lower.find("resourcetype>") {
                    let after_rt = rt_start + "resourcetype>".len();
                    if let Some(rt_end_rel) = xml_lower[after_rt..].find("resourcetype>") {
                        xml_lower[after_rt..after_rt + rt_end_rel].contains("collection")
                    } else { false }
                } else { false } || xml_lower.contains("iscollection>1</");
                
                if is_collection {
                    self.current_path = path.to_string();
                    Ok(())
                } else {
                    Err(ProviderError::InvalidPath(format!("{} is not a directory", path)))
                }
            }
            StatusCode::NOT_FOUND => {
                Err(ProviderError::NotFound(path.to_string()))
            }
            status => {
                Err(ProviderError::ServerError(format!("Server returned status: {}", status)))
            }
        }
    }
    
    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        let parent = std::path::Path::new(&self.current_path)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "/".to_string());
        
        self.current_path = if parent.is_empty() { "/".to_string() } else { parent };
        Ok(())
    }
    
    async fn download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        
        let response = self.request(Method::GET, remote_path).await
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
        
        match response.status() {
            StatusCode::OK => {
                let total_size = response.content_length().unwrap_or(0);
                let bytes = response.bytes().await
                    .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
                
                if let Some(progress) = on_progress {
                    progress(bytes.len() as u64, total_size);
                }
                
                tokio::fs::write(local_path, &bytes).await
                    .map_err(|e| ProviderError::IoError(e))?;
                
                Ok(())
            }
            StatusCode::NOT_FOUND => {
                Err(ProviderError::NotFound(remote_path.to_string()))
            }
            status => {
                Err(ProviderError::TransferFailed(format!("Download failed with status: {}", status)))
            }
        }
    }
    
    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        
        let response = self.request(Method::GET, remote_path).await
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
        
        match response.status() {
            StatusCode::OK => {
                let bytes = response.bytes().await
                    .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
                Ok(bytes.to_vec())
            }
            StatusCode::NOT_FOUND => {
                Err(ProviderError::NotFound(remote_path.to_string()))
            }
            status => {
                Err(ProviderError::TransferFailed(format!("Download failed with status: {}", status)))
            }
        }
    }
    
    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        
        let data = tokio::fs::read(local_path).await
            .map_err(|e| ProviderError::IoError(e))?;
        
        let total_size = data.len() as u64;
        
        let response = self.request(Method::PUT, remote_path).await
            .body(data)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
        
        match response.status() {
            StatusCode::OK | StatusCode::CREATED | StatusCode::NO_CONTENT => {
                if let Some(progress) = on_progress {
                    progress(total_size, total_size);
                }
                Ok(())
            }
            StatusCode::CONFLICT => {
                Err(ProviderError::InvalidPath("Parent directory does not exist".to_string()))
            }
            StatusCode::INSUFFICIENT_STORAGE => {
                Err(ProviderError::ServerError("Insufficient storage space".to_string()))
            }
            status => {
                Err(ProviderError::TransferFailed(format!("Upload failed with status: {}", status)))
            }
        }
    }
    
    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        
        let response = self.request(webdav_methods::mkcol(), path).await
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
        
        match response.status() {
            StatusCode::CREATED | StatusCode::OK => Ok(()),
            StatusCode::METHOD_NOT_ALLOWED => {
                Err(ProviderError::AlreadyExists(path.to_string()))
            }
            StatusCode::CONFLICT => {
                Err(ProviderError::InvalidPath("Parent directory does not exist".to_string()))
            }
            StatusCode::FORBIDDEN => {
                Err(ProviderError::PermissionDenied(path.to_string()))
            }
            status => {
                Err(ProviderError::ServerError(format!("MKCOL failed with status: {}", status)))
            }
        }
    }
    
    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        
        let response = self.request(Method::DELETE, path).await
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
        
        match response.status() {
            StatusCode::OK | StatusCode::NO_CONTENT | StatusCode::ACCEPTED => Ok(()),
            StatusCode::NOT_FOUND => {
                Err(ProviderError::NotFound(path.to_string()))
            }
            StatusCode::FORBIDDEN => {
                Err(ProviderError::PermissionDenied(path.to_string()))
            }
            status => {
                Err(ProviderError::ServerError(format!("DELETE failed with status: {}", status)))
            }
        }
    }
    
    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        // WebDAV DELETE works for both files and directories
        self.delete(path).await
    }
    
    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        // WebDAV DELETE automatically deletes recursively
        self.delete(path).await
    }
    
    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        
        let destination = self.build_url(to);
        
        let response = self.request(webdav_methods::move_method(), from).await
            .header("Destination", destination)
            .header("Overwrite", "F") // Don't overwrite existing
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
        
        match response.status() {
            StatusCode::OK | StatusCode::CREATED | StatusCode::NO_CONTENT => Ok(()),
            StatusCode::NOT_FOUND => {
                Err(ProviderError::NotFound(from.to_string()))
            }
            StatusCode::PRECONDITION_FAILED => {
                Err(ProviderError::AlreadyExists(to.to_string()))
            }
            StatusCode::CONFLICT => {
                Err(ProviderError::InvalidPath("Destination parent does not exist".to_string()))
            }
            status => {
                Err(ProviderError::ServerError(format!("MOVE failed with status: {}", status)))
            }
        }
    }
    
    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        
        let response = self.request(webdav_methods::propfind(), path).await
            .header("Depth", "0")
            .header("Content-Type", "application/xml")
            .body(r#"<?xml version="1.0" encoding="utf-8"?>
                <d:propfind xmlns:d="DAV:">
                    <d:prop>
                        <d:resourcetype/>
                        <d:getcontentlength/>
                        <d:getlastmodified/>
                        <d:getcontenttype/>
                        <d:getetag/>
                    </d:prop>
                </d:propfind>"#)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
        
        match response.status() {
            StatusCode::OK | StatusCode::MULTI_STATUS => {
                let xml = response.text().await
                    .map_err(|e| ProviderError::ParseError(e.to_string()))?;
                
                // Check for collection - search for "collection" inside resourcetype block
                let xml_lower = xml.to_lowercase();
                let is_dir = if let Some(rt_start) = xml_lower.find("resourcetype>") {
                    let after_rt = rt_start + "resourcetype>".len();
                    if let Some(rt_end_rel) = xml_lower[after_rt..].find("resourcetype>") {
                        xml_lower[after_rt..after_rt + rt_end_rel].contains("collection")
                    } else { false }
                } else { false } || xml_lower.contains("iscollection>1</");
                let size: u64 = self.extract_tag_content(&xml, "getcontentlength")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                let modified = self.extract_tag_content(&xml, "getlastmodified");
                let mime_type = self.extract_tag_content(&xml, "getcontenttype");
                
                let name = std::path::Path::new(path)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.to_string());
                
                Ok(RemoteEntry {
                    name,
                    path: path.to_string(),
                    is_dir,
                    size,
                    modified,
                    permissions: None,
                    owner: None,
                    group: None,
                    is_symlink: false,
                    link_target: None,
                    mime_type,
                    metadata: Default::default(),
                })
            }
            StatusCode::NOT_FOUND => {
                Err(ProviderError::NotFound(path.to_string()))
            }
            status => {
                Err(ProviderError::ServerError(format!("PROPFIND failed with status: {}", status)))
            }
        }
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
        // WebDAV uses HTTP which is stateless, no keep-alive needed
        // Just verify we can still authenticate
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        
        let response = self.request(Method::OPTIONS, "/").await
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
        
        if response.status() == StatusCode::UNAUTHORIZED {
            self.connected = false;
            return Err(ProviderError::AuthenticationFailed("Session expired".to_string()));
        }
        
        Ok(())
    }
    
    async fn server_info(&mut self) -> Result<String, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        
        let response = self.request(Method::OPTIONS, "/").await
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
        
        let server = response
            .headers()
            .get("server")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("Unknown WebDAV Server");
        
        let dav = response
            .headers()
            .get("dav")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("1");
        
        Ok(format!("WebDAV Server: {} (DAV compliance: {})", server, dav))
    }
    
    fn supports_find(&self) -> bool {
        true
    }

    async fn find(&mut self, path: &str, pattern: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let pattern_lower = pattern.to_lowercase();
        let mut results = Vec::new();
        let mut dirs_to_scan = vec![path.to_string()];

        while let Some(dir) = dirs_to_scan.pop() {
            let entries = match self.list(&dir).await {
                Ok(e) => e,
                Err(_) => continue,
            };

            for entry in entries {
                if entry.is_dir {
                    dirs_to_scan.push(entry.path.clone());
                }

                if entry.name.to_lowercase().contains(&pattern_lower) {
                    results.push(entry);
                    if results.len() >= 500 {
                        return Ok(results);
                    }
                }
            }
        }

        Ok(results)
    }

    async fn storage_info(&mut self) -> Result<super::StorageInfo, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        // RFC 4331: WebDAV quota properties
        let response = self.request(webdav_methods::propfind(), &self.current_path.clone()).await
            .header("Depth", "0")
            .header("Content-Type", "application/xml")
            .body(r#"<?xml version="1.0" encoding="utf-8"?>
                <d:propfind xmlns:d="DAV:">
                    <d:prop>
                        <d:quota-available-bytes/>
                        <d:quota-used-bytes/>
                    </d:prop>
                </d:propfind>"#)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !response.status().is_success() && response.status() != StatusCode::MULTI_STATUS {
            return Err(ProviderError::NotSupported("storage_info".to_string()));
        }

        let xml = response.text().await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        let used = self.extract_tag_content(&xml, "quota-used-bytes")
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        let free = self.extract_tag_content(&xml, "quota-available-bytes")
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        let total = used + free;

        Ok(super::StorageInfo { used, total, free })
    }

    fn supports_locking(&self) -> bool {
        true
    }

    async fn lock_file(&mut self, path: &str, timeout: u64) -> Result<super::LockInfo, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let timeout_header = if timeout == 0 {
            "Infinite".to_string()
        } else {
            format!("Second-{}", timeout)
        };

        let lock_body = r#"<?xml version="1.0" encoding="utf-8"?>
            <d:lockinfo xmlns:d="DAV:">
                <d:lockscope><d:exclusive/></d:lockscope>
                <d:locktype><d:write/></d:locktype>
                <d:owner><d:href>AeroFTP</d:href></d:owner>
            </d:lockinfo>"#;

        let response = self.request(webdav_methods::lock(), path).await
            .header("Depth", "0")
            .header("Timeout", &timeout_header)
            .header("Content-Type", "application/xml")
            .body(lock_body)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!("LOCK failed ({}): {}", status, text)));
        }

        // Extract lock token from Lock-Token header or XML response
        let lock_token = response.headers()
            .get("lock-token")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.trim_matches(|c| c == '<' || c == '>').to_string())
            .unwrap_or_default();

        Ok(super::LockInfo {
            token: lock_token,
            owner: Some("AeroFTP".to_string()),
            timeout,
            exclusive: true,
        })
    }

    async fn unlock_file(&mut self, path: &str, lock_token: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let token_header = format!("<{}>", lock_token);

        let response = self.request(webdav_methods::unlock(), path).await
            .header("Lock-Token", &token_header)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        match response.status() {
            reqwest::StatusCode::OK | reqwest::StatusCode::NO_CONTENT => Ok(()),
            status => {
                let text = response.text().await.unwrap_or_default();
                Err(ProviderError::ServerError(format!("UNLOCK failed ({}): {}", status, text)))
            }
        }
    }

    fn supports_server_copy(&self) -> bool {
        true
    }

    async fn server_copy(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        
        let destination = self.build_url(to);
        
        let response = self.request(webdav_methods::copy(), from).await
            .header("Destination", destination)
            .header("Overwrite", "F")
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
        
        match response.status() {
            StatusCode::OK | StatusCode::CREATED | StatusCode::NO_CONTENT => Ok(()),
            StatusCode::NOT_FOUND => {
                Err(ProviderError::NotFound(from.to_string()))
            }
            StatusCode::PRECONDITION_FAILED => {
                Err(ProviderError::AlreadyExists(to.to_string()))
            }
            status => {
                Err(ProviderError::ServerError(format!("COPY failed with status: {}", status)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_build_url() {
        let provider = WebDavProvider::new(WebDavConfig {
            url: "https://cloud.example.com/remote.php/dav/files/user/".to_string(),
            username: "user".to_string(),
            password: "pass".to_string(),
            initial_path: None,
        });
        
        assert_eq!(
            provider.build_url("/Documents"),
            "https://cloud.example.com/remote.php/dav/files/user/Documents"
        );
    }
    
    #[test]
    fn test_extract_tag_content() {
        let provider = WebDavProvider::new(WebDavConfig {
            url: "https://example.com".to_string(),
            username: "user".to_string(),
            password: "pass".to_string(),
            initial_path: None,
        });
        
        let xml = r#"<d:getcontentlength>12345</d:getcontentlength>"#;
        assert_eq!(provider.extract_tag_content(xml, "getcontentlength"), Some("12345".to_string()));
        
        let xml2 = r#"<D:getcontenttype>text/plain</D:getcontenttype>"#;
        assert_eq!(provider.extract_tag_content(xml2, "getcontenttype"), Some("text/plain".to_string()));
    }
}
