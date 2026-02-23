//! pCloud Storage Provider
//!
//! Implements StorageProvider for pCloud using their REST API.
//! Uses OAuth2 for authentication. Supports US and EU regions.

use async_trait::async_trait;
use serde::Deserialize;
use std::collections::HashMap;
use tracing::info;

use super::{
    StorageProvider, ProviderType, ProviderError, RemoteEntry, StorageInfo, FileVersion,
    sanitize_api_error,
    http_retry::{HttpRetryConfig, send_with_retry},
    oauth2::{OAuth2Manager, OAuthConfig},
};
use super::types::PCloudConfig;

/// pCloud folder metadata
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct PCloudMetadata {
    name: String,
    #[serde(default)]
    isfolder: bool,
    #[serde(default)]
    isfile: Option<bool>,
    #[serde(default)]
    size: u64,
    modified: Option<String>,
    created: Option<String>,
    path: Option<String>,
    folderid: Option<u64>,
    fileid: Option<u64>,
    /// PA-012: MIME type from pCloud API (e.g. "image/jpeg", "application/pdf")
    contenttype: Option<String>,
    #[serde(default)]
    contents: Option<Vec<PCloudMetadata>>,
}

/// pCloud API response wrapper
#[derive(Debug, Deserialize)]
struct PCloudResponse {
    result: u32,
    #[serde(default)]
    error: Option<String>,
    metadata: Option<PCloudMetadata>,
}

/// pCloud file link response
#[derive(Debug, Deserialize)]
struct PCloudFileLink {
    result: u32,
    #[serde(default)]
    error: Option<String>,
    hosts: Option<Vec<String>>,
    path: Option<String>,
}

/// pCloud user info
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct PCloudUserInfo {
    result: u32,
    quota: Option<u64>,
    usedquota: Option<u64>,
}

/// pCloud public link response
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct PCloudPubLink {
    result: u32,
    #[serde(default)]
    error: Option<String>,
    link: Option<String>,
    /// PA-001: linkid returned by getfilepublink, needed for deletepublink
    linkid: Option<u64>,
}

/// PA-001: Response for listpublinks containing all public links
#[derive(Debug, Deserialize)]
struct PCloudPubLinksResponse {
    result: u32,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    publinks: Vec<PCloudPubLinkEntry>,
}

/// PA-001: Individual public link entry from listpublinks
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct PCloudPubLinkEntry {
    linkid: u64,
    #[serde(default)]
    path: Option<String>,
    /// Metadata contains the path for the linked file/folder
    metadata: Option<PCloudPubLinkMeta>,
}

/// PA-001: Minimal metadata within a public link entry
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct PCloudPubLinkMeta {
    path: Option<String>,
    name: Option<String>,
}

/// pCloud revision entry
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct PCloudRevision {
    revisionid: u64,
    #[serde(default)]
    size: u64,
    modified: Option<String>,
}

/// pCloud revisions response
#[derive(Debug, Deserialize)]
struct PCloudRevisions {
    result: u32,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    revisions: Vec<PCloudRevision>,
}

/// pCloud thumb link response
#[derive(Debug, Deserialize)]
struct PCloudThumbLink {
    result: u32,
    #[serde(default)]
    error: Option<String>,
    hosts: Option<Vec<String>>,
    path: Option<String>,
}

/// PA-008: pCloud upload response (different structure from standard PCloudResponse)
#[derive(Debug, Deserialize)]
struct PCloudUploadResponse {
    result: u32,
    #[serde(default)]
    error: Option<String>,
}

/// pCloud Storage Provider
pub struct PCloudProvider {
    config: PCloudConfig,
    oauth_manager: OAuth2Manager,
    client: reqwest::Client,
    connected: bool,
    current_path: String,
    /// Authenticated user email
    account_email: Option<String>,
}

impl PCloudProvider {
    pub fn new(config: PCloudConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            config,
            oauth_manager: OAuth2Manager::new(),
            client,
            connected: false,
            current_path: "/".to_string(),
            account_email: None,
        }
    }

    /// Get Authorization header with Bearer token (token never exposed in URL)
    async fn auth_header(&self) -> Result<String, ProviderError> {
        use secrecy::ExposeSecret;
        let config = OAuthConfig::pcloud(&self.config.client_id, &self.config.client_secret, &self.config.region);
        let secret = self.oauth_manager.get_valid_token(&config).await
            .map_err(|e| ProviderError::AuthenticationFailed(format!("pCloud token error: {}", e)))?;
        Ok(format!("Bearer {}", secret.expose_secret()))
    }

    fn normalize_path(path: &str) -> String {
        let p = if path.starts_with('/') { path.to_string() } else { format!("/{}", path) };
        if p.len() > 1 { p.trim_end_matches('/').to_string() } else { p }
    }

    fn resolve_path(&self, path: &str) -> String {
        if path == "." || path.is_empty() {
            self.current_path.clone()
        } else if path.starts_with('/') {
            Self::normalize_path(path)
        } else {
            let base = if self.current_path == "/" { String::new() } else { self.current_path.clone() };
            Self::normalize_path(&format!("{}/{}", base, path))
        }
    }

    /// GAP-A01/PA-010: Send GET request with retry on 429/5xx via http_retry.rs
    async fn get_with_retry(&self, url: &str, auth: &str) -> Result<reqwest::Response, ProviderError> {
        let request = self.client.get(url)
            .header("Authorization", auth)
            .build()
            .map_err(|e| ProviderError::NetworkError(format!("Failed to build request: {}", e)))?;

        send_with_retry(&self.client, request, &HttpRetryConfig::default())
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))
    }

    /// Recursively collect entries matching a pattern
    fn collect_matching(&self, meta: &PCloudMetadata, base_path: &str, pattern: &str, results: &mut Vec<RemoteEntry>) {
        if let Some(contents) = &meta.contents {
            for item in contents {
                let item_path = if base_path == "/" {
                    format!("/{}", item.name)
                } else {
                    format!("{}/{}", base_path, item.name)
                };

                if item.name.to_lowercase().contains(pattern) {
                    results.push(RemoteEntry {
                        name: item.name.clone(),
                        path: item_path.clone(),
                        is_dir: item.isfolder,
                        size: item.size,
                        modified: item.modified.clone(),
                        permissions: None, owner: None, group: None,
                        is_symlink: false, link_target: None,
                        // PA-012: Populate MIME type from pCloud contenttype field
                        mime_type: item.contenttype.clone(),
                        metadata: Default::default(),
                    });
                }

                if item.isfolder {
                    self.collect_matching(item, &item_path, pattern, results);
                }
            }
        }
    }

    /// Check pCloud API response for errors (PA-014: sanitized messages)
    fn check_response(resp: &PCloudResponse) -> Result<(), ProviderError> {
        if resp.result != 0 {
            let raw_msg = resp.error.clone().unwrap_or_else(|| format!("Error code: {}", resp.result));
            let msg = sanitize_api_error(&raw_msg);
            return Err(match resp.result {
                2009 | 2010 => ProviderError::NotFound(msg),
                2003 | 2028 => ProviderError::PermissionDenied(msg),
                2004 => ProviderError::AlreadyExists(msg),
                _ => ProviderError::ServerError(msg),
            });
        }
        Ok(())
    }
}

#[async_trait]
impl StorageProvider for PCloudProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }

    fn provider_type(&self) -> ProviderType { ProviderType::PCloud }

    fn display_name(&self) -> String {
        format!("pCloud ({})", self.config.region.to_uppercase())
    }

    fn account_email(&self) -> Option<String> { self.account_email.clone() }

    fn is_connected(&self) -> bool { self.connected }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        let url = format!("{}/userinfo", self.config.api_base());
        let auth = self.auth_header().await?;
        let resp = self.get_with_retry(&url, &auth).await?;

        if !resp.status().is_success() {
            return Err(ProviderError::AuthenticationFailed("pCloud authentication failed".to_string()));
        }

        // Parse email from userinfo
        if let Ok(body) = resp.json::<serde_json::Value>().await {
            if let Some(email) = body["email"].as_str() {
                self.account_email = Some(email.to_string());
            }
        }

        self.connected = true;
        self.current_path = "/".to_string();
        info!("Connected to pCloud ({})", self.config.region);
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.connected = false;
        Ok(())
    }

    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let resolved = self.resolve_path(path);
        // PA-009: pCloud listfolder returns ALL items in a single response (no pagination needed).
        // The API does not support offset/limit parameters for listfolder.
        let url = format!("{}/listfolder?path={}",
            self.config.api_base(),
            urlencoding::encode(&resolved));

        let auth = self.auth_header().await?;
        let resp: PCloudResponse = self.get_with_retry(&url, &auth).await?
            .json().await
            .map_err(|e| ProviderError::ParseError(sanitize_api_error(&e.to_string())))?;

        Self::check_response(&resp)?;

        let metadata = resp.metadata
            .ok_or_else(|| ProviderError::ParseError("No metadata in response".to_string()))?;

        let contents = metadata.contents.unwrap_or_default();

        Ok(contents.into_iter().map(|item| {
            let item_path = if resolved == "/" {
                format!("/{}", item.name)
            } else {
                format!("{}/{}", resolved, item.name)
            };

            RemoteEntry {
                name: item.name,
                path: item_path,
                is_dir: item.isfolder,
                size: item.size,
                modified: item.modified,
                permissions: None, owner: None, group: None,
                is_symlink: false, link_target: None,
                // PA-012: Populate MIME type from pCloud contenttype field
                mime_type: item.contenttype,
                metadata: Default::default(),
            }
        }).collect())
    }

    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        let new_path = self.resolve_path(path);

        // Verify folder exists
        let url = format!("{}/listfolder?path={}&nofiles=1",
            self.config.api_base(),
            urlencoding::encode(&new_path));

        let auth = self.auth_header().await?;
        let resp: PCloudResponse = self.get_with_retry(&url, &auth).await?
            .json().await
            .map_err(|e| ProviderError::ParseError(sanitize_api_error(&e.to_string())))?;

        Self::check_response(&resp)?;
        self.current_path = new_path;
        Ok(())
    }

    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        if self.current_path == "/" { return Ok(()); }
        let parent = match self.current_path.rfind('/') {
            Some(0) => "/".to_string(),
            Some(pos) => self.current_path[..pos].to_string(),
            None => "/".to_string(),
        };
        self.current_path = parent;
        Ok(())
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> {
        Ok(self.current_path.clone())
    }

    async fn download(&mut self, remote_path: &str, local_path: &str, on_progress: Option<Box<dyn Fn(u64, u64) + Send>>) -> Result<(), ProviderError> {
        let resolved = self.resolve_path(remote_path);
        let auth = self.auth_header().await?;

        // Step 1: Get download link
        let url = format!("{}/getfilelink?path={}",
            self.config.api_base(), urlencoding::encode(&resolved));

        let link_resp: PCloudFileLink = self.get_with_retry(&url, &auth).await?
            .json().await
            .map_err(|e| ProviderError::ParseError(sanitize_api_error(&e.to_string())))?;

        if link_resp.result != 0 {
            return Err(ProviderError::TransferFailed(
                sanitize_api_error(&link_resp.error.unwrap_or_else(|| "Failed to get download link".to_string()))
            ));
        }

        let host = link_resp.hosts.and_then(|h| h.into_iter().next())
            .ok_or_else(|| ProviderError::TransferFailed("No download host".to_string()))?;
        let path = link_resp.path
            .ok_or_else(|| ProviderError::TransferFailed("No download path".to_string()))?;

        let download_url = format!("https://{}{}", host, path);

        // Step 2: Streaming download (no retry for data stream — only the link request is retried)
        let resp = self.client.get(&download_url).send().await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

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
        let auth = self.auth_header().await?;

        // Step 1: Get download link
        let url = format!("{}/getfilelink?path={}",
            self.config.api_base(), urlencoding::encode(&resolved));

        let link_resp: PCloudFileLink = self.get_with_retry(&url, &auth).await?
            .json().await
            .map_err(|e| ProviderError::ParseError(sanitize_api_error(&e.to_string())))?;

        if link_resp.result != 0 {
            return Err(ProviderError::TransferFailed(
                sanitize_api_error(&link_resp.error.unwrap_or_else(|| "Failed to get download link".to_string()))
            ));
        }

        let host = link_resp.hosts.and_then(|h| h.into_iter().next())
            .ok_or_else(|| ProviderError::TransferFailed("No download host".to_string()))?;
        let path = link_resp.path
            .ok_or_else(|| ProviderError::TransferFailed("No download path".to_string()))?;

        let download_url = format!("https://{}{}", host, path);

        // Step 2: Download
        let resp = self.client.get(&download_url).send().await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        let bytes = resp.bytes().await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        Ok(bytes.to_vec())
    }

    async fn upload(&mut self, local_path: &str, remote_path: &str, progress: Option<Box<dyn Fn(u64, u64) + Send>>) -> Result<(), ProviderError> {
        let resolved = self.resolve_path(remote_path);
        let (dir_path, file_name) = match resolved.rfind('/') {
            Some(pos) if pos > 0 => (&resolved[..pos], &resolved[pos + 1..]),
            _ => ("/", resolved.trim_start_matches('/')),
        };

        let auth = self.auth_header().await?;

        // PA-005: Get file size for progress tracking
        let file_metadata = tokio::fs::metadata(local_path).await
            .map_err(ProviderError::IoError)?;
        let file_size = file_metadata.len();

        // Streaming upload: read file as a stream instead of loading into memory
        let file = tokio::fs::File::open(local_path).await
            .map_err(ProviderError::IoError)?;
        let stream = tokio_util::io::ReaderStream::new(file);
        let body = reqwest::Body::wrap_stream(stream);

        let form = reqwest::multipart::Form::new()
            .part("file", reqwest::multipart::Part::stream(body).file_name(file_name.to_string()));

        let url = format!("{}/uploadfile?path={}&filename={}&nopartial=1",
            self.config.api_base(),
            urlencoding::encode(dir_path),
            urlencoding::encode(file_name));

        // Note: multipart uploads cannot use send_with_retry because the stream body
        // is consumed on first attempt and cannot be cloned/replayed.
        let resp = self.client.post(&url)
            .header("Authorization", auth)
            .multipart(form)
            .send().await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::TransferFailed(
                format!("Upload failed ({}): {}", status, sanitize_api_error(&body))
            ));
        }

        // PA-008: Check pCloud JSON result field — pCloud returns result:0 for success
        let upload_resp: PCloudUploadResponse = resp.json().await
            .map_err(|e| ProviderError::ParseError(sanitize_api_error(&e.to_string())))?;

        if upload_resp.result != 0 {
            let msg = upload_resp.error.unwrap_or_else(|| format!("Upload error code: {}", upload_resp.result));
            return Err(ProviderError::TransferFailed(sanitize_api_error(&msg)));
        }

        // PA-005: Report upload completion to progress callback
        if let Some(ref cb) = progress {
            cb(file_size, file_size);
        }

        Ok(())
    }

    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let resolved = self.resolve_path(path);
        let url = format!("{}/createfolder?path={}",
            self.config.api_base(),
            urlencoding::encode(&resolved));

        let auth = self.auth_header().await?;
        let resp: PCloudResponse = self.get_with_retry(&url, &auth).await?
            .json().await
            .map_err(|e| ProviderError::ParseError(sanitize_api_error(&e.to_string())))?;

        Self::check_response(&resp)?;
        Ok(())
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        let resolved = self.resolve_path(path);
        let auth = self.auth_header().await?;

        // PA-007: Try deletefile first; if it fails, fall back to deletefolder
        let url = format!("{}/deletefile?path={}",
            self.config.api_base(),
            urlencoding::encode(&resolved));

        let resp: PCloudResponse = self.get_with_retry(&url, &auth).await?
            .json().await
            .map_err(|e| ProviderError::ParseError(sanitize_api_error(&e.to_string())))?;

        if resp.result == 0 {
            return Ok(());
        }

        // Fall back to deletefolder for directory paths
        let url = format!("{}/deletefolder?path={}",
            self.config.api_base(),
            urlencoding::encode(&resolved));

        let resp: PCloudResponse = self.get_with_retry(&url, &auth).await?
            .json().await
            .map_err(|e| ProviderError::ParseError(sanitize_api_error(&e.to_string())))?;

        Self::check_response(&resp)?;
        Ok(())
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let resolved = self.resolve_path(path);
        let url = format!("{}/deletefolderrecursive?path={}",
            self.config.api_base(),
            urlencoding::encode(&resolved));

        let auth = self.auth_header().await?;
        let resp: PCloudResponse = self.get_with_retry(&url, &auth).await?
            .json().await
            .map_err(|e| ProviderError::ParseError(sanitize_api_error(&e.to_string())))?;

        Self::check_response(&resp)?;
        Ok(())
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        self.rmdir(path).await
    }

    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let from_resolved = self.resolve_path(from);
        let to_resolved = self.resolve_path(to);
        let auth = self.auth_header().await?;

        // Try file rename first
        let url = format!("{}/renamefile?path={}&topath={}",
            self.config.api_base(),
            urlencoding::encode(&from_resolved),
            urlencoding::encode(&to_resolved));

        let resp: PCloudResponse = self.get_with_retry(&url, &auth).await?
            .json().await
            .map_err(|e| ProviderError::ParseError(sanitize_api_error(&e.to_string())))?;

        if resp.result == 0 {
            return Ok(());
        }

        // Try folder rename
        let url = format!("{}/renamefolder?path={}&topath={}",
            self.config.api_base(),
            urlencoding::encode(&from_resolved),
            urlencoding::encode(&to_resolved));

        let resp: PCloudResponse = self.get_with_retry(&url, &auth).await?
            .json().await
            .map_err(|e| ProviderError::ParseError(sanitize_api_error(&e.to_string())))?;

        Self::check_response(&resp)?;
        Ok(())
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        let resolved = self.resolve_path(path);
        let auth = self.auth_header().await?;

        // PA-006: Try stat first (works for both files and folders in pCloud API)
        let url = format!("{}/stat?path={}",
            self.config.api_base(),
            urlencoding::encode(&resolved));

        let resp: PCloudResponse = self.get_with_retry(&url, &auth).await?
            .json().await
            .map_err(|e| ProviderError::ParseError(sanitize_api_error(&e.to_string())))?;

        // PA-006: If stat fails, fall back to listfolder to get folder metadata
        if resp.result != 0 {
            let url = format!("{}/listfolder?path={}&nofiles=1",
                self.config.api_base(),
                urlencoding::encode(&resolved));

            let folder_resp: PCloudResponse = self.get_with_retry(&url, &auth).await?
                .json().await
                .map_err(|e| ProviderError::ParseError(sanitize_api_error(&e.to_string())))?;

            Self::check_response(&folder_resp)?;

            let meta = folder_resp.metadata
                .ok_or_else(|| ProviderError::ParseError("No metadata".to_string()))?;

            return Ok(RemoteEntry {
                name: meta.name,
                path: resolved,
                is_dir: true,
                size: meta.size,
                modified: meta.modified,
                permissions: None, owner: None, group: None,
                is_symlink: false, link_target: None,
                mime_type: None,
                metadata: Default::default(),
            });
        }

        let meta = resp.metadata
            .ok_or_else(|| ProviderError::ParseError("No metadata".to_string()))?;

        Ok(RemoteEntry {
            name: meta.name,
            path: resolved,
            is_dir: meta.isfolder,
            size: meta.size,
            modified: meta.modified,
            permissions: None, owner: None, group: None,
            is_symlink: false, link_target: None,
            // PA-012: Populate MIME type from pCloud contenttype field
            mime_type: meta.contenttype,
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
        Ok(format!("pCloud ({}) - {}", self.config.region.to_uppercase(), self.config.api_base()))
    }

    async fn storage_info(&mut self) -> Result<StorageInfo, ProviderError> {
        let url = format!("{}/userinfo", self.config.api_base());
        let auth = self.auth_header().await?;
        let info: PCloudUserInfo = self.get_with_retry(&url, &auth).await?
            .json().await
            .map_err(|e| ProviderError::ParseError(sanitize_api_error(&e.to_string())))?;

        let total = info.quota.unwrap_or(0);
        let used = info.usedquota.unwrap_or(0);

        Ok(StorageInfo {
            total,
            used,
            free: total.saturating_sub(used),
        })
    }

    fn supports_share_links(&self) -> bool { true }

    async fn create_share_link(&mut self, path: &str, _expires_in_secs: Option<u64>) -> Result<String, ProviderError> {
        let resolved = self.resolve_path(path);
        let url = format!("{}/getfilepublink?path={}",
            self.config.api_base(),
            urlencoding::encode(&resolved));

        let auth = self.auth_header().await?;
        let resp: PCloudPubLink = self.get_with_retry(&url, &auth).await?
            .json().await
            .map_err(|e| ProviderError::ParseError(sanitize_api_error(&e.to_string())))?;

        if resp.result != 0 {
            return Err(ProviderError::Other(
                sanitize_api_error(&resp.error.unwrap_or_else(|| "Failed to create link".to_string()))
            ));
        }

        resp.link.ok_or_else(|| ProviderError::Other("No link in response".to_string()))
    }

    async fn remove_share_link(&mut self, path: &str) -> Result<(), ProviderError> {
        let resolved = self.resolve_path(path);
        let auth = self.auth_header().await?;

        // PA-001 (CRITICAL): pCloud deletepublink requires `linkid`, not path.
        // First, list all public links to find the one matching this path.
        let list_url = format!("{}/listpublinks", self.config.api_base());

        let links_resp: PCloudPubLinksResponse = self.get_with_retry(&list_url, &auth).await?
            .json().await
            .map_err(|e| ProviderError::ParseError(sanitize_api_error(&e.to_string())))?;

        if links_resp.result != 0 {
            return Err(ProviderError::Other(
                sanitize_api_error(&links_resp.error.unwrap_or_else(|| "Failed to list public links".to_string()))
            ));
        }

        // Find the link whose metadata path matches the resolved path
        let link_id = links_resp.publinks.iter()
            .find(|link| {
                // Check metadata.path first, then top-level path
                if let Some(ref meta) = link.metadata {
                    if let Some(ref p) = meta.path {
                        if p == &resolved { return true; }
                    }
                }
                if let Some(ref p) = link.path {
                    if p == &resolved { return true; }
                }
                false
            })
            .map(|link| link.linkid)
            .ok_or_else(|| ProviderError::NotFound(
                format!("No public link found for path: {}", resolved)
            ))?;

        // Now delete using the correct linkid parameter
        let delete_url = format!("{}/deletepublink?linkid={}",
            self.config.api_base(), link_id);

        let resp: PCloudResponse = self.get_with_retry(&delete_url, &auth).await?
            .json().await
            .map_err(|e| ProviderError::ParseError(sanitize_api_error(&e.to_string())))?;

        Self::check_response(&resp)?;
        Ok(())
    }

    fn supports_server_copy(&self) -> bool { true }

    async fn server_copy(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let from_resolved = self.resolve_path(from);
        let to_resolved = self.resolve_path(to);
        let auth = self.auth_header().await?;

        // Try copyfile first
        let url = format!("{}/copyfile?path={}&topath={}",
            self.config.api_base(),
            urlencoding::encode(&from_resolved),
            urlencoding::encode(&to_resolved));

        let resp: PCloudResponse = self.get_with_retry(&url, &auth).await?
            .json().await
            .map_err(|e| ProviderError::ParseError(sanitize_api_error(&e.to_string())))?;

        if resp.result == 0 { return Ok(()); }

        // Try copyfolder
        let url = format!("{}/copyfolder?path={}&topath={}",
            self.config.api_base(),
            urlencoding::encode(&from_resolved),
            urlencoding::encode(&to_resolved));

        let resp: PCloudResponse = self.get_with_retry(&url, &auth).await?
            .json().await
            .map_err(|e| ProviderError::ParseError(sanitize_api_error(&e.to_string())))?;

        Self::check_response(&resp)?;
        Ok(())
    }

    fn supports_thumbnails(&self) -> bool { true }

    async fn get_thumbnail(&mut self, path: &str) -> Result<String, ProviderError> {
        let resolved = self.resolve_path(path);
        let url = format!("{}/getthumblink?path={}&size=256x256",
            self.config.api_base(),
            urlencoding::encode(&resolved));

        let auth = self.auth_header().await?;
        let resp: PCloudThumbLink = self.get_with_retry(&url, &auth).await?
            .json().await
            .map_err(|e| ProviderError::ParseError(sanitize_api_error(&e.to_string())))?;

        if resp.result != 0 {
            return Err(ProviderError::NotFound(
                sanitize_api_error(&resp.error.unwrap_or_else(|| "No thumbnail available".to_string()))
            ));
        }

        let host = resp.hosts.and_then(|h| h.into_iter().next())
            .ok_or_else(|| ProviderError::Other("No thumbnail host".to_string()))?;
        let thumb_path = resp.path
            .ok_or_else(|| ProviderError::Other("No thumbnail path".to_string()))?;

        // Fetch thumbnail bytes and encode as base64 data URI
        let thumb_url = format!("https://{}{}", host, thumb_path);
        let bytes = self.client.get(&thumb_url).send().await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?
            .bytes().await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
        Ok(format!("data:image/jpeg;base64,{}", BASE64.encode(&bytes)))
    }

    // PA-013: Version support implemented via pCloud revisions API (listrevisions/getfilelink?revisionid)
    fn supports_versions(&self) -> bool { true }

    async fn list_versions(&mut self, path: &str) -> Result<Vec<FileVersion>, ProviderError> {
        let resolved = self.resolve_path(path);
        let url = format!("{}/listrevisions?path={}",
            self.config.api_base(),
            urlencoding::encode(&resolved));

        let auth = self.auth_header().await?;
        let resp: PCloudRevisions = self.get_with_retry(&url, &auth).await?
            .json().await
            .map_err(|e| ProviderError::ParseError(sanitize_api_error(&e.to_string())))?;

        if resp.result != 0 {
            return Err(ProviderError::Other(
                sanitize_api_error(&resp.error.unwrap_or_else(|| "Failed to list revisions".to_string()))
            ));
        }

        Ok(resp.revisions.into_iter().map(|r| FileVersion {
            id: r.revisionid.to_string(),
            modified: r.modified,
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
        let resolved = self.resolve_path(path);
        let auth = self.auth_header().await?;

        // Get download link for specific revision
        let url = format!("{}/getfilelink?path={}&revisionid={}",
            self.config.api_base(),
            urlencoding::encode(&resolved), version_id);

        let link_resp: PCloudFileLink = self.get_with_retry(&url, &auth).await?
            .json().await
            .map_err(|e| ProviderError::ParseError(sanitize_api_error(&e.to_string())))?;

        if link_resp.result != 0 {
            return Err(ProviderError::TransferFailed(
                sanitize_api_error(&link_resp.error.unwrap_or_else(|| "Failed to get version link".to_string()))
            ));
        }

        let host = link_resp.hosts.and_then(|h| h.into_iter().next())
            .ok_or_else(|| ProviderError::TransferFailed("No download host".to_string()))?;
        let dl_path = link_resp.path
            .ok_or_else(|| ProviderError::TransferFailed("No download path".to_string()))?;

        let download_url = format!("https://{}{}", host, dl_path);
        let bytes = self.client.get(&download_url).send().await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?
            .bytes().await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        tokio::fs::write(local_path, &bytes).await
            .map_err(ProviderError::IoError)?;

        Ok(())
    }

    fn supports_checksum(&self) -> bool { true }

    async fn checksum(&mut self, path: &str) -> Result<HashMap<String, String>, ProviderError> {
        let resolved = self.resolve_path(path);
        let url = format!("{}/checksumfile?path={}",
            self.config.api_base(),
            urlencoding::encode(&resolved));

        #[derive(Debug, Deserialize)]
        struct ChecksumResponse {
            result: u32,
            #[serde(default)]
            error: Option<String>,
            sha256: Option<String>,
            sha1: Option<String>,
            md5: Option<String>,
        }

        let auth = self.auth_header().await?;
        let resp: ChecksumResponse = self.get_with_retry(&url, &auth).await?
            .json().await
            .map_err(|e| ProviderError::ParseError(sanitize_api_error(&e.to_string())))?;

        if resp.result != 0 {
            return Err(ProviderError::Other(
                sanitize_api_error(&resp.error.unwrap_or_else(|| "Checksum failed".to_string()))
            ));
        }

        let mut checksums = HashMap::new();
        if let Some(v) = resp.sha256 { checksums.insert("sha256".to_string(), v); }
        if let Some(v) = resp.sha1 { checksums.insert("sha1".to_string(), v); }
        if let Some(v) = resp.md5 { checksums.insert("md5".to_string(), v); }
        Ok(checksums)
    }

    fn supports_remote_upload(&self) -> bool { true }

    async fn remote_upload(&mut self, url: &str, dest_path: &str) -> Result<(), ProviderError> {
        let resolved = self.resolve_path(dest_path);
        // PA-003: pCloud's `downloadfile` is the correct endpoint for server-side URL fetch.
        // Despite the name, this tells pCloud's server to download a file FROM the given URL
        // and save it to the user's account. This is NOT the same as client-side download.
        let api_url = format!("{}/downloadfile?url={}&path={}",
            self.config.api_base(),
            urlencoding::encode(url),
            urlencoding::encode(&resolved));

        let auth = self.auth_header().await?;
        let resp: PCloudResponse = self.get_with_retry(&api_url, &auth).await?
            .json().await
            .map_err(|e| ProviderError::ParseError(sanitize_api_error(&e.to_string())))?;

        Self::check_response(&resp)?;
        info!("Remote upload from {} to {}", url, dest_path);
        Ok(())
    }

    fn supports_find(&self) -> bool { true }

    async fn find(&mut self, path: &str, pattern: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let resolved = self.resolve_path(path);
        let pattern_lower = pattern.to_lowercase();

        // Use recursive listfolder and filter client-side
        // PA-009: pCloud returns all items in a single recursive response (no pagination)
        let url = format!("{}/listfolder?path={}&recursive=1",
            self.config.api_base(),
            urlencoding::encode(&resolved));

        let auth = self.auth_header().await?;
        let resp: PCloudResponse = self.get_with_retry(&url, &auth).await?
            .json().await
            .map_err(|e| ProviderError::ParseError(sanitize_api_error(&e.to_string())))?;

        Self::check_response(&resp)?;

        let metadata = resp.metadata
            .ok_or_else(|| ProviderError::ParseError("No metadata".to_string()))?;

        let mut results = Vec::new();
        self.collect_matching(&metadata, &resolved, &pattern_lower, &mut results);
        Ok(results)
    }
}
