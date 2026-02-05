//! Azure Blob Storage Provider
//!
//! Implements StorageProvider for Azure Blob Storage using the REST API.
//! Supports Shared Key and SAS token authentication.

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use hmac::{Hmac, Mac};
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_LENGTH, CONTENT_TYPE};
use sha2::Sha256;
use tracing::info;

use super::{
    StorageProvider, ProviderType, ProviderError, RemoteEntry,
};
use super::types::AzureConfig;

type HmacSha256 = Hmac<Sha256>;

/// Azure API version
const API_VERSION: &str = "2024-11-04";

/// Azure list blobs XML item
#[derive(Debug)]
struct BlobItem {
    name: String,
    size: u64,
    last_modified: Option<String>,
    is_prefix: bool,  // virtual directory
}

/// Azure Blob Storage Provider
pub struct AzureProvider {
    config: AzureConfig,
    client: reqwest::Client,
    connected: bool,
    current_prefix: String,
}

impl AzureProvider {
    pub fn new(config: AzureConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
            connected: false,
            current_prefix: String::new(),
        }
    }

    /// Build the full blob URL
    fn blob_url(&self, blob_path: &str) -> String {
        let endpoint = self.config.blob_endpoint();
        let path = blob_path.trim_start_matches('/');
        if path.is_empty() {
            format!("{}/{}", endpoint, self.config.container)
        } else {
            format!("{}/{}/{}", endpoint, self.config.container, path)
        }
    }

    /// Add SAS token or Shared Key auth to request
    fn sign_request(&self, method: &str, url: &str, headers: &HeaderMap, content_length: u64) -> Result<String, ProviderError> {
        if let Some(ref sas) = self.config.sas_token {
            // SAS token appended to URL
            let separator = if url.contains('?') { "&" } else { "?" };
            return Ok(format!("{}{}{}", url, separator, sas));
        }

        // Shared Key signing
        let now = chrono::Utc::now().format("%a, %d %b %Y %H:%M:%S GMT").to_string();
        let content_type = headers.get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        // Parse URL for canonicalized resource
        let parsed = url::Url::parse(url)
            .map_err(|e| ProviderError::Other(format!("Invalid URL: {}", e)))?;
        let path = parsed.path();
        let canonicalized_resource = format!("/{}{}", self.config.account_name, path);

        // Add query params (sorted)
        let mut query_parts: Vec<(String, String)> = parsed.query_pairs()
            .map(|(k, v)| (k.to_lowercase(), v.to_string()))
            .collect();
        query_parts.sort();
        let query_str = query_parts.iter()
            .map(|(k, v)| format!("\n{}:{}", k, v))
            .collect::<String>();

        let string_to_sign = format!(
            "{}\n\n\n{}\n\n{}\n\n\n\n\n\n\nx-ms-date:{}\nx-ms-version:{}\n{}{}",
            method,
            if content_length > 0 { content_length.to_string() } else { String::new() },
            content_type,
            now,
            API_VERSION,
            canonicalized_resource,
            query_str,
        );

        let key_bytes = BASE64.decode(&self.config.access_key)
            .map_err(|e| ProviderError::Other(format!("Invalid access key: {}", e)))?;

        let mut mac = HmacSha256::new_from_slice(&key_bytes)
            .map_err(|e| ProviderError::Other(format!("HMAC error: {}", e)))?;
        mac.update(string_to_sign.as_bytes());
        let signature = BASE64.encode(mac.finalize().into_bytes());

        Ok(format!("SharedKey {}:{}", self.config.account_name, signature))
    }

    /// Parse XML blob list response
    fn parse_blob_list(&self, xml: &str) -> Vec<BlobItem> {
        let mut items = Vec::new();

        // Parse BlobPrefix (virtual directories)
        for prefix_match in xml.split("<BlobPrefix>").skip(1) {
            if let Some(name_end) = prefix_match.find("</Name>") {
                if let Some(name_start) = prefix_match.find("<Name>") {
                    let name = &prefix_match[name_start + 6..name_end];
                    let display_name = name.trim_end_matches('/');
                    // Strip current prefix
                    let relative = display_name.strip_prefix(&self.current_prefix).unwrap_or(display_name);
                    let relative = relative.trim_start_matches('/');
                    if !relative.is_empty() {
                        items.push(BlobItem {
                            name: relative.to_string(),
                            size: 0,
                            last_modified: None,
                            is_prefix: true,
                        });
                    }
                }
            }
        }

        // Parse Blob items (files)
        for blob_match in xml.split("<Blob>").skip(1) {
            let name = Self::extract_xml_tag(blob_match, "Name").unwrap_or_default();
            let relative = name.strip_prefix(&self.current_prefix).unwrap_or(&name);
            let relative = relative.trim_start_matches('/');
            if relative.is_empty() || relative.contains('/') { continue; }

            let size = Self::extract_xml_tag(blob_match, "Content-Length")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let modified = Self::extract_xml_tag(blob_match, "Last-Modified");

            items.push(BlobItem {
                name: relative.to_string(),
                size,
                last_modified: modified,
                is_prefix: false,
            });
        }

        items
    }

    fn extract_xml_tag(xml: &str, tag: &str) -> Option<String> {
        let open = format!("<{}>", tag);
        let close = format!("</{}>", tag);
        let start = xml.find(&open)?;
        let end = xml.find(&close)?;
        Some(xml[start + open.len()..end].to_string())
    }

    fn resolve_blob_path(&self, path: &str) -> String {
        if path == "." || path.is_empty() {
            self.current_prefix.clone()
        } else if path.starts_with('/') {
            path.trim_start_matches('/').to_string()
        } else {
            if self.current_prefix.is_empty() {
                path.to_string()
            } else {
                format!("{}{}", self.current_prefix, path)
            }
        }
    }
}

#[async_trait]
impl StorageProvider for AzureProvider {
    fn provider_type(&self) -> ProviderType { ProviderType::Azure }

    fn display_name(&self) -> String {
        format!("Azure:{}/{}", self.config.account_name, self.config.container)
    }

    fn is_connected(&self) -> bool { self.connected }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        // Test connection by listing with max_results=1
        let url = format!("{}?restype=container&comp=list&maxresults=1",
            format!("{}/{}", self.config.blob_endpoint(), self.config.container));

        let mut headers = HeaderMap::new();
        let now = chrono::Utc::now().format("%a, %d %b %Y %H:%M:%S GMT").to_string();
        headers.insert("x-ms-date", HeaderValue::from_str(&now).unwrap());
        headers.insert("x-ms-version", HeaderValue::from_static(API_VERSION));

        let auth = self.sign_request("GET", &url, &headers, 0)?;

        let resp = if self.config.sas_token.is_some() {
            self.client.get(&auth).headers(headers).send().await
        } else {
            headers.insert("Authorization", HeaderValue::from_str(&auth).unwrap());
            self.client.get(&url).headers(headers).send().await
        }.map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::AuthenticationFailed(format!("Azure auth failed: {}", body)));
        }

        self.connected = true;
        self.current_prefix = String::new();
        info!("Connected to Azure Blob Storage: {}/{}", self.config.account_name, self.config.container);
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.connected = false;
        Ok(())
    }

    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let prefix = self.resolve_blob_path(path);
        let prefix_param = if prefix.is_empty() { String::new() } else {
            let p = if prefix.ends_with('/') { prefix.clone() } else { format!("{}/", prefix) };
            format!("&prefix={}", urlencoding::encode(&p))
        };

        let url = format!("{}?restype=container&comp=list&delimiter=/{}",
            format!("{}/{}", self.config.blob_endpoint(), self.config.container),
            prefix_param);

        let mut headers = HeaderMap::new();
        let now = chrono::Utc::now().format("%a, %d %b %Y %H:%M:%S GMT").to_string();
        headers.insert("x-ms-date", HeaderValue::from_str(&now).unwrap());
        headers.insert("x-ms-version", HeaderValue::from_static(API_VERSION));

        let auth = self.sign_request("GET", &url, &headers, 0)?;

        let resp = if self.config.sas_token.is_some() {
            self.client.get(&auth).headers(headers).send().await
        } else {
            headers.insert("Authorization", HeaderValue::from_str(&auth).unwrap());
            self.client.get(&url).headers(headers).send().await
        }.map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        let body = resp.text().await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        let items = self.parse_blob_list(&body);

        let display_prefix = if prefix.is_empty() { "/" } else { &prefix };
        Ok(items.into_iter().map(|item| {
            let entry_path = if display_prefix == "/" {
                format!("/{}", item.name)
            } else {
                format!("/{}/{}", display_prefix.trim_end_matches('/'), item.name)
            };

            RemoteEntry {
                name: item.name,
                path: entry_path,
                is_dir: item.is_prefix,
                size: item.size,
                modified: item.last_modified,
                permissions: None, owner: None, group: None,
                is_symlink: false, link_target: None, mime_type: None,
                metadata: Default::default(),
            }
        }).collect())
    }

    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        if path == ".." || path == "/" {
            return self.cd_up().await;
        }

        let new_prefix = if path.starts_with('/') {
            path.trim_start_matches('/').to_string()
        } else if self.current_prefix.is_empty() {
            path.to_string()
        } else {
            format!("{}{}/", self.current_prefix, path.trim_end_matches('/'))
        };

        // Ensure trailing slash for prefix
        self.current_prefix = if new_prefix.ends_with('/') || new_prefix.is_empty() {
            new_prefix
        } else {
            format!("{}/", new_prefix)
        };

        Ok(())
    }

    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        let trimmed = self.current_prefix.trim_end_matches('/');
        self.current_prefix = match trimmed.rfind('/') {
            Some(pos) => format!("{}/", &trimmed[..pos]),
            None => String::new(),
        };
        Ok(())
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> {
        if self.current_prefix.is_empty() {
            Ok("/".to_string())
        } else {
            Ok(format!("/{}", self.current_prefix.trim_end_matches('/')))
        }
    }

    async fn download(&mut self, remote_path: &str, local_path: &str, _progress: Option<Box<dyn Fn(u64, u64) + Send>>) -> Result<(), ProviderError> {
        let blob_path = self.resolve_blob_path(remote_path);
        let url = self.blob_url(&blob_path);

        let mut headers = HeaderMap::new();
        let now = chrono::Utc::now().format("%a, %d %b %Y %H:%M:%S GMT").to_string();
        headers.insert("x-ms-date", HeaderValue::from_str(&now).unwrap());
        headers.insert("x-ms-version", HeaderValue::from_static(API_VERSION));

        let auth = self.sign_request("GET", &url, &headers, 0)?;

        let resp = if self.config.sas_token.is_some() {
            self.client.get(&auth).headers(headers).send().await
        } else {
            headers.insert("Authorization", HeaderValue::from_str(&auth).unwrap());
            self.client.get(&url).headers(headers).send().await
        }.map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(ProviderError::TransferFailed(format!("Download failed: {}", resp.status())));
        }

        let bytes = resp.bytes().await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        tokio::fs::write(local_path, &bytes).await
            .map_err(|e| ProviderError::IoError(e))?;

        Ok(())
    }

    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        let blob_path = self.resolve_blob_path(remote_path);
        let url = self.blob_url(&blob_path);

        let mut headers = HeaderMap::new();
        let now = chrono::Utc::now().format("%a, %d %b %Y %H:%M:%S GMT").to_string();
        headers.insert("x-ms-date", HeaderValue::from_str(&now).unwrap());
        headers.insert("x-ms-version", HeaderValue::from_static(API_VERSION));

        let auth = self.sign_request("GET", &url, &headers, 0)?;

        let resp = if self.config.sas_token.is_some() {
            self.client.get(&auth).headers(headers).send().await
        } else {
            headers.insert("Authorization", HeaderValue::from_str(&auth).unwrap());
            self.client.get(&url).headers(headers).send().await
        }.map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(ProviderError::TransferFailed(format!("Download failed: {}", resp.status())));
        }

        resp.bytes().await
            .map(|b| b.to_vec())
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))
    }

    async fn upload(&mut self, local_path: &str, remote_path: &str, _progress: Option<Box<dyn Fn(u64, u64) + Send>>) -> Result<(), ProviderError> {
        let blob_path = self.resolve_blob_path(remote_path);
        let url = self.blob_url(&blob_path);
        let data = tokio::fs::read(local_path).await
            .map_err(|e| ProviderError::IoError(e))?;

        let mut headers = HeaderMap::new();
        let now = chrono::Utc::now().format("%a, %d %b %Y %H:%M:%S GMT").to_string();
        headers.insert("x-ms-date", HeaderValue::from_str(&now).unwrap());
        headers.insert("x-ms-version", HeaderValue::from_static(API_VERSION));
        headers.insert("x-ms-blob-type", HeaderValue::from_static("BlockBlob"));
        headers.insert(CONTENT_LENGTH, HeaderValue::from_str(&data.len().to_string()).unwrap());

        let auth = self.sign_request("PUT", &url, &headers, data.len() as u64)?;

        let resp = if self.config.sas_token.is_some() {
            self.client.put(&auth).headers(headers).body(data).send().await
        } else {
            headers.insert("Authorization", HeaderValue::from_str(&auth).unwrap());
            self.client.put(&url).headers(headers).body(data).send().await
        }.map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::TransferFailed(format!("Upload failed: {}", body)));
        }

        Ok(())
    }

    async fn mkdir(&mut self, _path: &str) -> Result<(), ProviderError> {
        // Azure Blob Storage doesn't have real directories
        // Virtual directories are created implicitly by blobs with "/" in names
        Ok(())
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        let blob_path = self.resolve_blob_path(path);
        let url = self.blob_url(&blob_path);

        let mut headers = HeaderMap::new();
        let now = chrono::Utc::now().format("%a, %d %b %Y %H:%M:%S GMT").to_string();
        headers.insert("x-ms-date", HeaderValue::from_str(&now).unwrap());
        headers.insert("x-ms-version", HeaderValue::from_static(API_VERSION));

        let auth = self.sign_request("DELETE", &url, &headers, 0)?;

        let resp = if self.config.sas_token.is_some() {
            self.client.delete(&auth).headers(headers).send().await
        } else {
            headers.insert("Authorization", HeaderValue::from_str(&auth).unwrap());
            self.client.delete(&url).headers(headers).send().await
        }.map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() && resp.status().as_u16() != 202 {
            return Err(ProviderError::Other(format!("Delete failed: {}", resp.status())));
        }

        Ok(())
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        // Delete all blobs with this prefix
        self.rmdir_recursive(path).await
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        let entries = self.list(path).await?;
        for entry in entries {
            if entry.is_dir {
                Box::pin(self.rmdir_recursive(&entry.path)).await?;
            } else {
                self.delete(&entry.path).await?;
            }
        }
        Ok(())
    }

    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        // Azure doesn't have native rename - must copy then delete
        let from_blob = self.resolve_blob_path(from);
        let to_blob = self.resolve_blob_path(to);

        let source_url = self.blob_url(&from_blob);
        let dest_url = self.blob_url(&to_blob);

        let mut headers = HeaderMap::new();
        let now = chrono::Utc::now().format("%a, %d %b %Y %H:%M:%S GMT").to_string();
        headers.insert("x-ms-date", HeaderValue::from_str(&now).unwrap());
        headers.insert("x-ms-version", HeaderValue::from_static(API_VERSION));
        headers.insert("x-ms-copy-source", HeaderValue::from_str(&source_url).unwrap());

        let auth = self.sign_request("PUT", &dest_url, &headers, 0)?;

        let resp = if self.config.sas_token.is_some() {
            self.client.put(&auth).headers(headers).send().await
        } else {
            headers.insert("Authorization", HeaderValue::from_str(&auth).unwrap());
            self.client.put(&dest_url).headers(headers).send().await
        }.map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(ProviderError::Other(format!("Copy failed: {}", resp.status())));
        }

        // Delete original
        self.delete(from).await?;

        Ok(())
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        let blob_path = self.resolve_blob_path(path);
        let url = self.blob_url(&blob_path);

        let mut headers = HeaderMap::new();
        let now = chrono::Utc::now().format("%a, %d %b %Y %H:%M:%S GMT").to_string();
        headers.insert("x-ms-date", HeaderValue::from_str(&now).unwrap());
        headers.insert("x-ms-version", HeaderValue::from_static(API_VERSION));

        let auth = self.sign_request("HEAD", &url, &headers, 0)?;

        let resp = if self.config.sas_token.is_some() {
            self.client.head(&auth).headers(headers).send().await
        } else {
            headers.insert("Authorization", HeaderValue::from_str(&auth).unwrap());
            self.client.head(&url).headers(headers).send().await
        }.map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(ProviderError::NotFound(path.to_string()));
        }

        let size = resp.headers().get(CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        let modified = resp.headers().get("Last-Modified")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        let name = std::path::Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string());

        Ok(RemoteEntry {
            name,
            path: format!("/{}", blob_path),
            is_dir: false,
            size,
            modified,
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
        Ok(format!("Azure Blob Storage: {}/{}", self.config.account_name, self.config.container))
    }

    fn supports_share_links(&self) -> bool {
        true
    }

    async fn create_share_link(
        &mut self,
        path: &str,
        expires_in_secs: Option<u64>,
    ) -> Result<String, ProviderError> {
        let blob_path = path.trim_start_matches('/');
        let expiry_secs = expires_in_secs.unwrap_or(7 * 24 * 3600); // default 7 days
        let now = chrono::Utc::now();
        let start = (now - chrono::Duration::minutes(5)).format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let expiry = (now + chrono::Duration::seconds(expiry_secs as i64)).format("%Y-%m-%dT%H:%M:%SZ").to_string();

        // Service SAS for a specific blob
        let signed_permissions = "r"; // read only
        let signed_start = &start;
        let signed_expiry = &expiry;
        let canonicalized_resource = format!("/blob/{}/{}/{}", self.config.account_name, self.config.container, blob_path);
        let signed_version = API_VERSION;
        let signed_protocol = "https";

        // StringToSign for Service SAS (Blob)
        let string_to_sign = format!(
            "{}\n{}\n{}\n{}\n\n{}\n{}\n\n\n\n\n\n",
            signed_permissions,
            signed_start,
            signed_expiry,
            canonicalized_resource,
            signed_version,
            signed_protocol,
        );

        let key_bytes = BASE64.decode(&self.config.access_key)
            .map_err(|e| ProviderError::Other(format!("Invalid access key: {}", e)))?;

        let mut mac = HmacSha256::new_from_slice(&key_bytes)
            .map_err(|e| ProviderError::Other(format!("HMAC error: {}", e)))?;
        mac.update(string_to_sign.as_bytes());
        let signature = BASE64.encode(mac.finalize().into_bytes());

        let sas_token = format!(
            "sp={}&st={}&se={}&spr={}&sv={}&sr=b&sig={}",
            signed_permissions,
            urlencoding::encode(signed_start),
            urlencoding::encode(signed_expiry),
            signed_protocol,
            signed_version,
            urlencoding::encode(&signature),
        );

        let blob_url = self.blob_url(blob_path);
        let share_url = format!("{}?{}", blob_url, sas_token);

        info!("Created SAS share link for {} (expires: {})", path, expiry);
        Ok(share_url)
    }
}
