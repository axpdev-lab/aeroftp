//! Box Cloud Storage Provider
//!
//! Implements StorageProvider for Box using the Box API v2.
//! Uses OAuth2 PKCE for authentication.

use async_trait::async_trait;
use reqwest::header::{HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::Deserialize;
use std::collections::HashMap;
use tracing::info;

use super::{
    StorageProvider, ProviderType, ProviderError, RemoteEntry, StorageInfo, FileVersion,
    sanitize_api_error,
    oauth2::{OAuth2Manager, OAuthConfig},
};
use super::types::BoxConfig;

/// Box API endpoints
const API_BASE: &str = "https://api.box.com/2.0";
const UPLOAD_BASE: &str = "https://upload.box.com/api/2.0";

/// Box folder item
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct BoxItem {
    #[serde(rename = "type")]
    item_type: String,
    id: String,
    name: String,
    size: Option<u64>,
    modified_at: Option<String>,
    created_at: Option<String>,
}

/// Box folder items response
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct BoxItemCollection {
    entries: Vec<BoxItem>,
    total_count: u64,
    #[allow(dead_code)]
    offset: u64,
    #[allow(dead_code)]
    limit: u64,
}

/// Box user info (for quota)
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct BoxUser {
    space_amount: u64,
    space_used: u64,
}

/// Box shared link response
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct BoxSharedLink {
    url: String,
}

/// Box file/folder with shared link
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct BoxItemWithLink {
    shared_link: Option<BoxSharedLink>,
}

/// Box file version entry
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct BoxFileVersion {
    id: String,
    #[serde(rename = "type")]
    version_type: String,
    size: Option<u64>,
    modified_at: Option<String>,
    modified_by: Option<BoxVersionUser>,
}

/// Box version user
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct BoxVersionUser {
    name: Option<String>,
}

/// Box versions collection
#[derive(Debug, Deserialize)]
struct BoxVersionCollection {
    entries: Vec<BoxFileVersion>,
}

/// Box search result
#[derive(Debug, Deserialize)]
struct BoxSearchResult {
    entries: Vec<BoxItem>,
}

/// Box Storage Provider
pub struct BoxProvider {
    config: BoxConfig,
    oauth_manager: OAuth2Manager,
    client: reqwest::Client,
    connected: bool,
    current_path: String,
    current_folder_id: String,
    /// Cache: path -> folder ID
    id_cache: HashMap<String, String>,
    /// Authenticated user email
    account_email: Option<String>,
}

impl BoxProvider {
    pub fn new(config: BoxConfig) -> Self {
        Self {
            config,
            oauth_manager: OAuth2Manager::new(),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
            connected: false,
            current_path: "/".to_string(),
            current_folder_id: "0".to_string(),
            account_email: None,
            id_cache: {
                let mut m = HashMap::new();
                m.insert("/".to_string(), "0".to_string());
                m
            },
        }
    }

    /// Get access token from OAuth manager (returns SecretString for memory zeroization)
    async fn get_token(&self) -> Result<secrecy::SecretString, ProviderError> {
        let config = OAuthConfig::box_cloud(&self.config.client_id, &self.config.client_secret);
        self.oauth_manager.get_valid_token(&config).await
            .map_err(|e| ProviderError::AuthenticationFailed(format!("Box token error: {}", e)))
    }

    /// Get authorization header value
    fn bearer_header(token: &secrecy::SecretString) -> HeaderValue {
        use secrecy::ExposeSecret;
        HeaderValue::from_str(&format!("Bearer {}", token.expose_secret())).unwrap()
    }

    /// Resolve a path to a folder ID, using cache or API calls
    async fn resolve_folder_id(&mut self, path: &str) -> Result<String, ProviderError> {
        let normalized = Self::normalize_path(path);

        if let Some(id) = self.id_cache.get(&normalized) {
            return Ok(id.clone());
        }

        // Walk the path from root
        let parts: Vec<&str> = normalized.split('/').filter(|s| !s.is_empty()).collect();
        let mut current_id = "0".to_string();
        let mut built_path = String::new();

        for part in parts {
            built_path = format!("{}/{}", built_path, part);

            if let Some(id) = self.id_cache.get(&built_path) {
                current_id = id.clone();
                continue;
            }

            // List folder to find child
            let token = self.get_token().await?;
            let url = format!("{}/folders/{}/items?fields=name,type,id&limit=1000", API_BASE, current_id);
            let resp = self.client.get(&url)
                .header(AUTHORIZATION, Self::bearer_header(&token))
                .send().await
                .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

            if !resp.status().is_success() {
                return Err(ProviderError::NotFound(format!("Path not found: {}", path)));
            }

            let items: BoxItemCollection = resp.json().await
                .map_err(|e| ProviderError::ParseError(e.to_string()))?;

            let found = items.entries.iter()
                .find(|item| item.name == part && item.item_type == "folder");

            match found {
                Some(folder) => {
                    current_id = folder.id.clone();
                    self.id_cache.insert(built_path.clone(), current_id.clone());
                }
                None => return Err(ProviderError::NotFound(format!("Folder not found: {}", part))),
            }
        }

        Ok(current_id)
    }

    /// Resolve a file path to its ID
    async fn resolve_file_id(&mut self, path: &str) -> Result<String, ProviderError> {
        let normalized = Self::normalize_path(path);
        let (parent_path, file_name) = match normalized.rfind('/') {
            Some(pos) if pos > 0 => (&normalized[..pos], &normalized[pos + 1..]),
            _ => ("/", normalized.trim_start_matches('/')),
        };

        let parent_id = self.resolve_folder_id(parent_path).await?;
        let token = self.get_token().await?;

        let url = format!("{}/folders/{}/items?fields=name,type,id&limit=1000", API_BASE, parent_id);
        let resp = self.client.get(&url)
            .header(AUTHORIZATION, Self::bearer_header(&token))
            .send().await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        let items: BoxItemCollection = resp.json().await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        items.entries.iter()
            .find(|item| item.name == file_name)
            .map(|item| item.id.clone())
            .ok_or_else(|| ProviderError::NotFound(format!("File not found: {}", file_name)))
    }

    fn normalize_path(path: &str) -> String {
        let p = if path.starts_with('/') { path.to_string() } else { format!("/{}", path) };
        if p.len() > 1 { p.trim_end_matches('/').to_string() } else { p }
    }

    /// Upload data in chunks via a Box upload session, then commit
    async fn chunked_upload_session(
        &self,
        session_resp: reqwest::Response,
        local_path: &str,
        total_size: u64,
        on_progress: Option<std::sync::Arc<std::sync::Mutex<Box<dyn Fn(u64, u64) + Send>>>>,
    ) -> Result<(), ProviderError> {
        use sha1::{Sha1, Digest};
        use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
        use tokio::io::AsyncReadExt;

        #[derive(Deserialize)]
        struct UploadSession {
            id: String,
            session_endpoints: Option<SessionEndpoints>,
            part_size: u64,
        }
        #[derive(Deserialize)]
        struct SessionEndpoints {
            upload_part: Option<String>,
            commit: Option<String>,
        }

        let session: UploadSession = session_resp.json().await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        let chunk_size = session.part_size as usize;
        let upload_part_url = session.session_endpoints.as_ref()
            .and_then(|e| e.upload_part.clone())
            .unwrap_or_else(|| format!("{}/files/upload_sessions/{}", UPLOAD_BASE, session.id));
        let commit_url = session.session_endpoints.as_ref()
            .and_then(|e| e.commit.clone())
            .unwrap_or_else(|| format!("{}/files/upload_sessions/{}/commit", UPLOAD_BASE, session.id));

        // Stream from file handle instead of buffered &[u8]
        let mut file = tokio::fs::File::open(local_path).await
            .map_err(|e| ProviderError::IoError(e))?;
        let mut parts: Vec<serde_json::Value> = Vec::new();
        let mut offset: u64 = 0;
        let mut whole_sha1 = Sha1::new();

        while offset < total_size {
            let remaining = (total_size - offset) as usize;
            let this_chunk = std::cmp::min(chunk_size, remaining);
            let mut chunk = vec![0u8; this_chunk];
            file.read_exact(&mut chunk).await
                .map_err(|e| ProviderError::IoError(e))?;

            let chunk_sha1 = {
                let mut h = Sha1::new();
                h.update(&chunk);
                BASE64.encode(h.finalize())
            };
            whole_sha1.update(&chunk);

            let token = self.get_token().await?;
            let end = offset + this_chunk as u64;
            let content_range = format!("bytes {}-{}/{}", offset, end - 1, total_size);

            let resp = self.client.put(&upload_part_url)
                .header(AUTHORIZATION, Self::bearer_header(&token))
                .header(CONTENT_TYPE, "application/octet-stream")
                .header("Content-Range", &content_range)
                .header("Digest", format!("sha={}", chunk_sha1))
                .body(chunk)
                .send().await
                .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

            if !resp.status().is_success() {
                let t = resp.text().await.unwrap_or_default();
                return Err(ProviderError::TransferFailed(format!("Chunk upload failed: {}", sanitize_api_error(&t))));
            }

            let part_resp: serde_json::Value = resp.json().await
                .map_err(|e| ProviderError::ParseError(e.to_string()))?;

            if let Some(part) = part_resp.get("part") {
                parts.push(part.clone());
            }

            offset = end;
            if let Some(ref cb) = on_progress {
                if let Ok(f) = cb.lock() {
                    f(offset, total_size);
                }
            }
        }

        // Commit
        let file_sha1 = BASE64.encode(whole_sha1.finalize());
        let token = self.get_token().await?;
        let commit_body = serde_json::json!({"parts": parts});

        let resp = self.client.post(&commit_url)
            .header(AUTHORIZATION, Self::bearer_header(&token))
            .header(CONTENT_TYPE, "application/json")
            .header("Digest", format!("sha={}", file_sha1))
            .json(&commit_body)
            .send().await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() && resp.status().as_u16() != 201 {
            let t = resp.text().await.unwrap_or_default();
            return Err(ProviderError::TransferFailed(format!("Commit failed: {}", sanitize_api_error(&t))));
        }

        Ok(())
    }
}

#[async_trait]
impl StorageProvider for BoxProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }

    fn provider_type(&self) -> ProviderType { ProviderType::Box }

    fn display_name(&self) -> String { "Box".to_string() }

    fn account_email(&self) -> Option<String> { self.account_email.clone() }

    fn is_connected(&self) -> bool { self.connected }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        // Verify token works
        let token = self.get_token().await?;

        let resp = self.client.get(&format!("{}/users/me", API_BASE))
            .header(AUTHORIZATION, Self::bearer_header(&token))
            .send().await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(ProviderError::AuthenticationFailed("Box authentication failed".to_string()));
        }

        // Parse email from user info
        if let Ok(body) = resp.json::<serde_json::Value>().await {
            if let Some(login) = body["login"].as_str() {
                self.account_email = Some(login.to_string());
            }
        }

        self.connected = true;
        self.current_path = "/".to_string();
        self.current_folder_id = "0".to_string();
        info!("Connected to Box");
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.connected = false;
        self.id_cache.clear();
        self.id_cache.insert("/".to_string(), "0".to_string());
        Ok(())
    }

    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let folder_id = if path == "." || path.is_empty() {
            self.current_folder_id.clone()
        } else {
            self.resolve_folder_id(path).await?
        };

        let base_path = if path == "." || path.is_empty() {
            self.current_path.clone()
        } else {
            Self::normalize_path(path)
        };

        // Paginated listing: offset-based, 1000 items per page
        let mut all_items: Vec<BoxItem> = Vec::new();
        let mut offset: u64 = 0;
        const PAGE_LIMIT: u64 = 1000;

        loop {
            let token = self.get_token().await?;
            let url = format!(
                "{}/folders/{}/items?fields=name,type,id,size,modified_at&limit={}&offset={}",
                API_BASE, folder_id, PAGE_LIMIT, offset
            );

            let resp = self.client.get(&url)
                .header(AUTHORIZATION, Self::bearer_header(&token))
                .send().await
                .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                return Err(ProviderError::ServerError(format!("Box API error {}: {}", status, sanitize_api_error(&body))));
            }

            let page: BoxItemCollection = resp.json().await
                .map_err(|e| ProviderError::ParseError(e.to_string()))?;

            let page_count = page.entries.len() as u64;
            all_items.extend(page.entries);

            // Stop if we've fetched all items or no more entries returned
            if all_items.len() as u64 >= page.total_count || page_count == 0 {
                break;
            }
            offset += page_count;
        }

        let entries = all_items.into_iter().map(|item| {
            let is_dir = item.item_type == "folder";
            let entry_path = if base_path == "/" {
                format!("/{}", item.name)
            } else {
                format!("{}/{}", base_path, item.name)
            };

            RemoteEntry {
                name: item.name,
                path: entry_path,
                is_dir,
                size: item.size.unwrap_or(0),
                modified: item.modified_at,
                permissions: None,
                owner: None,
                group: None,
                is_symlink: false,
                link_target: None,
                mime_type: None,
                metadata: {
                    let mut m = HashMap::new();
                    m.insert("id".to_string(), item.id);
                    m
                },
            }
        }).collect::<Vec<_>>();

        // Update folder ID cache from results
        for entry in &entries {
            if entry.is_dir {
                if let Some(id) = entry.metadata.get("id") {
                    self.id_cache.insert(entry.path.clone(), id.clone());
                }
            }
        }

        Ok(entries)
    }

    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        let new_path = if path.starts_with('/') {
            Self::normalize_path(path)
        } else {
            let base = if self.current_path == "/" { String::new() } else { self.current_path.clone() };
            Self::normalize_path(&format!("{}/{}", base, path))
        };

        let folder_id = self.resolve_folder_id(&new_path).await?;
        self.current_path = new_path;
        self.current_folder_id = folder_id;
        Ok(())
    }

    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        if self.current_path == "/" {
            return Ok(());
        }
        let parent = match self.current_path.rfind('/') {
            Some(0) => "/".to_string(),
            Some(pos) => self.current_path[..pos].to_string(),
            None => "/".to_string(),
        };
        self.cd(&parent).await
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> {
        Ok(self.current_path.clone())
    }

    async fn download(&mut self, remote_path: &str, local_path: &str, _progress: Option<Box<dyn Fn(u64, u64) + Send>>) -> Result<(), ProviderError> {
        use futures_util::StreamExt;
        use tokio::io::AsyncWriteExt;

        let file_id = self.resolve_file_id(remote_path).await?;
        let token = self.get_token().await?;

        let url = format!("{}/files/{}/content", API_BASE, file_id);
        let resp = self.client.get(&url)
            .header(AUTHORIZATION, Self::bearer_header(&token))
            .send().await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(ProviderError::TransferFailed(format!("Download failed: {}", resp.status())));
        }

        let mut stream = resp.bytes_stream();
        let mut file = tokio::fs::File::create(local_path).await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
            file.write_all(&chunk).await
                .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
        }

        Ok(())
    }

    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        let file_id = self.resolve_file_id(remote_path).await?;
        let token = self.get_token().await?;

        let url = format!("{}/files/{}/content", API_BASE, file_id);
        let resp = self.client.get(&url)
            .header(AUTHORIZATION, Self::bearer_header(&token))
            .send().await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(ProviderError::TransferFailed(format!("Download failed: {}", resp.status())));
        }

        resp.bytes().await
            .map(|b| b.to_vec())
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))
    }

    async fn upload(&mut self, local_path: &str, remote_path: &str, on_progress: Option<Box<dyn Fn(u64, u64) + Send>>) -> Result<(), ProviderError> {
        let normalized = Self::normalize_path(remote_path);
        let (parent_path, file_name) = match normalized.rfind('/') {
            Some(pos) if pos > 0 => (&normalized[..pos], &normalized[pos + 1..]),
            _ => ("/", normalized.trim_start_matches('/')),
        };

        let parent_id = self.resolve_folder_id(parent_path).await?;
        let total_size = tokio::fs::metadata(local_path).await
            .map_err(|e| ProviderError::IoError(e))?.len();

        const CHUNKED_THRESHOLD: u64 = 50 * 1024 * 1024; // 50MB

        if total_size > CHUNKED_THRESHOLD {
            // Chunked upload session for large files — stream from file handle
            let progress: Option<std::sync::Arc<std::sync::Mutex<Box<dyn Fn(u64, u64) + Send>>>> =
                on_progress.map(|cb| std::sync::Arc::new(std::sync::Mutex::new(cb)));
            let token = self.get_token().await?;

            // Step 1: Create upload session
            let session_body = serde_json::json!({
                "file_name": file_name,
                "folder_id": parent_id,
                "file_size": total_size
            });

            let session_url = format!("{}/files/upload_sessions", UPLOAD_BASE);
            let session_resp = self.client.post(&session_url)
                .header(AUTHORIZATION, Self::bearer_header(&token))
                .header(CONTENT_TYPE, "application/json")
                .body(session_body.to_string())
                .send().await
                .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

            if !session_resp.status().is_success() {
                let text = session_resp.text().await.unwrap_or_default();
                // If conflict, try upload new version session
                if text.contains("item_name_in_use") {
                    let file_id = self.resolve_file_id(remote_path).await?;
                    let token2 = self.get_token().await?;
                    let ver_body = serde_json::json!({"file_size": total_size});
                    let ver_url = format!("{}/files/{}/upload_sessions", UPLOAD_BASE, file_id);
                    let ver_resp = self.client.post(&ver_url)
                        .header(AUTHORIZATION, Self::bearer_header(&token2))
                        .header(CONTENT_TYPE, "application/json")
                        .body(ver_body.to_string())
                        .send().await
                        .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

                    if !ver_resp.status().is_success() {
                        let t = ver_resp.text().await.unwrap_or_default();
                        return Err(ProviderError::TransferFailed(format!("Upload session failed: {}", sanitize_api_error(&t))));
                    }

                    return self.chunked_upload_session(ver_resp, local_path, total_size, progress.clone()).await;
                }
                return Err(ProviderError::TransferFailed(format!("Upload session failed: {}", sanitize_api_error(&text))));
            }

            return self.chunked_upload_session(session_resp, local_path, total_size, progress).await;
        }

        // Simple multipart upload for small files (<=50MB, OK to buffer)
        let data = tokio::fs::read(local_path).await
            .map_err(|e| ProviderError::IoError(e))?;
        let token = self.get_token().await?;
        let attributes = serde_json::json!({
            "name": file_name,
            "parent": {"id": parent_id}
        });

        let form = reqwest::multipart::Form::new()
            .text("attributes", attributes.to_string())
            .part("file", reqwest::multipart::Part::bytes(data).file_name(file_name.to_string()));

        let url = format!("{}/files/content", UPLOAD_BASE);
        let resp = self.client.post(&url)
            .header(AUTHORIZATION, Self::bearer_header(&token))
            .multipart(form)
            .send().await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            if body.contains("item_name_in_use") {
                let file_id = self.resolve_file_id(remote_path).await?;
                let data2 = tokio::fs::read(local_path).await
                    .map_err(|e| ProviderError::IoError(e))?;
                let token2 = self.get_token().await?;

                let form2 = reqwest::multipart::Form::new()
                    .part("file", reqwest::multipart::Part::bytes(data2).file_name(file_name.to_string()));

                let url2 = format!("{}/files/{}/content", UPLOAD_BASE, file_id);
                let resp2 = self.client.post(&url2)
                    .header(AUTHORIZATION, Self::bearer_header(&token2))
                    .multipart(form2)
                    .send().await
                    .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

                if !resp2.status().is_success() {
                    return Err(ProviderError::TransferFailed(format!("Upload version failed: {}", resp2.status())));
                }
            } else {
                return Err(ProviderError::TransferFailed(format!("Upload failed: {}", sanitize_api_error(&body))));
            }
        }

        if let Some(ref cb) = on_progress {
            cb(total_size, total_size);
        }
        Ok(())
    }

    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let normalized = Self::normalize_path(path);
        let (parent_path, folder_name) = match normalized.rfind('/') {
            Some(pos) if pos > 0 => (&normalized[..pos], &normalized[pos + 1..]),
            _ => ("/", normalized.trim_start_matches('/')),
        };

        let parent_id = self.resolve_folder_id(parent_path).await?;
        let token = self.get_token().await?;

        let body = serde_json::json!({
            "name": folder_name,
            "parent": {"id": parent_id}
        });

        let resp = self.client.post(&format!("{}/folders", API_BASE))
            .header(AUTHORIZATION, Self::bearer_header(&token))
            .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
            .json(&body)
            .send().await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!("mkdir failed: {}", sanitize_api_error(&body))));
        }

        Ok(())
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        let file_id = self.resolve_file_id(path).await?;
        let token = self.get_token().await?;

        let resp = self.client.delete(&format!("{}/files/{}", API_BASE, file_id))
            .header(AUTHORIZATION, Self::bearer_header(&token))
            .send().await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() && resp.status().as_u16() != 204 {
            return Err(ProviderError::Other(format!("Delete failed: {}", resp.status())));
        }

        Ok(())
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let folder_id = self.resolve_folder_id(path).await?;
        let token = self.get_token().await?;

        let resp = self.client.delete(&format!("{}/folders/{}?recursive=true", API_BASE, folder_id))
            .header(AUTHORIZATION, Self::bearer_header(&token))
            .send().await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() && resp.status().as_u16() != 204 {
            return Err(ProviderError::Other(format!("rmdir failed: {}", resp.status())));
        }

        // Remove from cache
        let normalized = Self::normalize_path(path);
        self.id_cache.remove(&normalized);

        Ok(())
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        self.rmdir(path).await // Box API handles recursive by default
    }

    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        // Try as file first, then as folder
        let token = self.get_token().await?;
        let new_name = std::path::Path::new(to).file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| to.to_string());

        if let Ok(file_id) = self.resolve_file_id(from).await {
            let body = serde_json::json!({"name": new_name});
            let resp = self.client.put(&format!("{}/files/{}", API_BASE, file_id))
                .header(AUTHORIZATION, Self::bearer_header(&token))
                .json(&body)
                .send().await
                .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

            if !resp.status().is_success() {
                return Err(ProviderError::Other(format!("Rename failed: {}", resp.status())));
            }
        } else {
            let folder_id = self.resolve_folder_id(from).await?;
            let body = serde_json::json!({"name": new_name});
            let resp = self.client.put(&format!("{}/folders/{}", API_BASE, folder_id))
                .header(AUTHORIZATION, Self::bearer_header(&token))
                .json(&body)
                .send().await
                .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

            if !resp.status().is_success() {
                return Err(ProviderError::Other(format!("Rename failed: {}", resp.status())));
            }
        }

        Ok(())
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        // Try file first
        if let Ok(file_id) = self.resolve_file_id(path).await {
            let token = self.get_token().await?;
            let resp = self.client.get(&format!("{}/files/{}?fields=name,type,size,modified_at", API_BASE, file_id))
                .header(AUTHORIZATION, Self::bearer_header(&token))
                .send().await
                .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

            let item: BoxItem = resp.json().await
                .map_err(|e| ProviderError::ParseError(e.to_string()))?;

            return Ok(RemoteEntry {
                name: item.name,
                path: Self::normalize_path(path),
                is_dir: false,
                size: item.size.unwrap_or(0),
                modified: item.modified_at,
                permissions: None, owner: None, group: None,
                is_symlink: false, link_target: None, mime_type: None,
                metadata: Default::default(),
            });
        }

        // Try folder
        let folder_id = self.resolve_folder_id(path).await?;
        let token = self.get_token().await?;
        let resp = self.client.get(&format!("{}/folders/{}?fields=name,type,size,modified_at", API_BASE, folder_id))
            .header(AUTHORIZATION, Self::bearer_header(&token))
            .send().await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        let item: BoxItem = resp.json().await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        Ok(RemoteEntry {
            name: item.name,
            path: Self::normalize_path(path),
            is_dir: true,
            size: 0,
            modified: item.modified_at,
            permissions: None, owner: None, group: None,
            is_symlink: false, link_target: None, mime_type: None,
            metadata: Default::default(),
        })
    }

    async fn exists(&mut self, path: &str) -> Result<bool, ProviderError> {
        match self.stat(path).await {
            Ok(_) => Ok(true),
            Err(ProviderError::NotFound(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    async fn size(&mut self, path: &str) -> Result<u64, ProviderError> {
        let entry = self.stat(path).await?;
        Ok(entry.size)
    }

    async fn keep_alive(&mut self) -> Result<(), ProviderError> { Ok(()) }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        Ok("Box Cloud Storage (api.box.com)".to_string())
    }

    // Storage info
    async fn storage_info(&mut self) -> Result<StorageInfo, ProviderError> {
        let token = self.get_token().await?;
        let resp = self.client.get(&format!("{}/users/me?fields=space_amount,space_used", API_BASE))
            .header(AUTHORIZATION, Self::bearer_header(&token))
            .send().await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        let user: BoxUser = resp.json().await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        Ok(StorageInfo {
            total: user.space_amount,
            used: user.space_used,
            free: user.space_amount.saturating_sub(user.space_used),
        })
    }

    // Share links
    fn supports_share_links(&self) -> bool { true }

    async fn create_share_link(&mut self, path: &str, _expires_in_secs: Option<u64>) -> Result<String, ProviderError> {
        let file_id = self.resolve_file_id(path).await?;
        let token = self.get_token().await?;

        let body = serde_json::json!({
            "shared_link": {"access": "open"}
        });

        let resp = self.client.put(&format!("{}/files/{}?fields=shared_link", API_BASE, file_id))
            .header(AUTHORIZATION, Self::bearer_header(&token))
            .json(&body)
            .send().await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        let item: BoxItemWithLink = resp.json().await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        item.shared_link
            .map(|l| l.url)
            .ok_or_else(|| ProviderError::Other("Failed to create share link".to_string()))
    }

    async fn remove_share_link(&mut self, path: &str) -> Result<(), ProviderError> {
        let file_id = self.resolve_file_id(path).await?;
        let token = self.get_token().await?;

        let body = serde_json::json!({"shared_link": null});
        let _resp = self.client.put(&format!("{}/files/{}", API_BASE, file_id))
            .header(AUTHORIZATION, Self::bearer_header(&token))
            .json(&body)
            .send().await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        Ok(())
    }

    fn supports_server_copy(&self) -> bool { true }

    async fn server_copy(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let token = self.get_token().await?;
        let to_normalized = Self::normalize_path(to);
        let (to_parent, to_name) = match to_normalized.rfind('/') {
            Some(pos) if pos > 0 => (&to_normalized[..pos], &to_normalized[pos + 1..]),
            _ => ("/", to_normalized.trim_start_matches('/')),
        };
        let to_parent_id = self.resolve_folder_id(to_parent).await?;

        // Try file copy first
        if let Ok(file_id) = self.resolve_file_id(from).await {
            let body = serde_json::json!({
                "parent": {"id": to_parent_id},
                "name": to_name
            });
            let resp = self.client.post(&format!("{}/files/{}/copy", API_BASE, file_id))
                .header(AUTHORIZATION, Self::bearer_header(&token))
                .json(&body)
                .send().await
                .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

            if !resp.status().is_success() {
                return Err(ProviderError::Other(format!("Copy failed: {}", resp.status())));
            }
            return Ok(());
        }

        // Try folder copy
        let folder_id = self.resolve_folder_id(from).await?;
        let body = serde_json::json!({
            "parent": {"id": to_parent_id},
            "name": to_name
        });
        let resp = self.client.post(&format!("{}/folders/{}/copy", API_BASE, folder_id))
            .header(AUTHORIZATION, Self::bearer_header(&token))
            .json(&body)
            .send().await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(ProviderError::Other(format!("Copy failed: {}", resp.status())));
        }
        Ok(())
    }

    fn supports_thumbnails(&self) -> bool { true }

    async fn get_thumbnail(&mut self, path: &str) -> Result<String, ProviderError> {
        let file_id = self.resolve_file_id(path).await?;
        let token = self.get_token().await?;

        let url = format!("{}/files/{}/thumbnail.png?min_height=256&min_width=256", API_BASE, file_id);
        let resp = self.client.get(&url)
            .header(AUTHORIZATION, Self::bearer_header(&token))
            .send().await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(ProviderError::NotFound("No thumbnail available".to_string()));
        }

        let bytes = resp.bytes().await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
        Ok(format!("data:image/png;base64,{}", BASE64.encode(&bytes)))
    }

    fn supports_versions(&self) -> bool { true }

    async fn list_versions(&mut self, path: &str) -> Result<Vec<FileVersion>, ProviderError> {
        let file_id = self.resolve_file_id(path).await?;
        let token = self.get_token().await?;

        let url = format!("{}/files/{}/versions", API_BASE, file_id);
        let resp = self.client.get(&url)
            .header(AUTHORIZATION, Self::bearer_header(&token))
            .send().await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(ProviderError::Other(format!("Failed to list versions: {}", resp.status())));
        }

        let versions: BoxVersionCollection = resp.json().await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        Ok(versions.entries.into_iter().map(|v| FileVersion {
            id: v.id,
            modified: v.modified_at,
            size: v.size.unwrap_or(0),
            modified_by: v.modified_by.and_then(|u| u.name),
        }).collect())
    }

    async fn download_version(
        &mut self,
        path: &str,
        version_id: &str,
        local_path: &str,
    ) -> Result<(), ProviderError> {
        let file_id = self.resolve_file_id(path).await?;
        let token = self.get_token().await?;

        let url = format!("{}/files/{}/content?version={}", API_BASE, file_id, version_id);
        let resp = self.client.get(&url)
            .header(AUTHORIZATION, Self::bearer_header(&token))
            .send().await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(ProviderError::TransferFailed(format!("Version download failed: {}", resp.status())));
        }

        let bytes = resp.bytes().await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        tokio::fs::write(local_path, &bytes).await
            .map_err(|e| ProviderError::IoError(e))?;

        Ok(())
    }

    async fn restore_version(&mut self, path: &str, version_id: &str) -> Result<(), ProviderError> {
        let file_id = self.resolve_file_id(path).await?;
        let token = self.get_token().await?;

        let body = serde_json::json!({"id": version_id});
        let resp = self.client.post(&format!("{}/files/{}/versions/current", API_BASE, file_id))
            .header(AUTHORIZATION, Self::bearer_header(&token))
            .json(&body)
            .send().await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(ProviderError::Other(format!("Restore version failed: {}", resp.status())));
        }
        Ok(())
    }

    fn supports_find(&self) -> bool { true }

    async fn find(&mut self, _path: &str, pattern: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let token = self.get_token().await?;

        let url = format!("{}/search?query={}&limit=200", API_BASE, urlencoding::encode(pattern));
        let resp = self.client.get(&url)
            .header(AUTHORIZATION, Self::bearer_header(&token))
            .send().await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(ProviderError::Other(format!("Search failed: {}", resp.status())));
        }

        let results: BoxSearchResult = resp.json().await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        Ok(results.entries.into_iter().map(|item| {
            RemoteEntry {
                name: item.name,
                path: String::new(), // Box search doesn't return full path
                is_dir: item.item_type == "folder",
                size: item.size.unwrap_or(0),
                modified: item.modified_at,
                permissions: None, owner: None, group: None,
                is_symlink: false, link_target: None, mime_type: None,
                metadata: {
                    let mut m = HashMap::new();
                    m.insert("id".to_string(), item.id);
                    m
                },
            }
        }).collect())
    }
}
