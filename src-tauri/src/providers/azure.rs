//! Azure Blob Storage Provider
//!
//! Implements StorageProvider for Azure Blob Storage using the REST API.
//! Supports Shared Key and SAS token authentication.
//!
//! ## Limitations (documented)
//! - AZ-008: No lease management (complex, rarely needed for file manager)
//! - AZ-009: No snapshot support
//! - AZ-010: Only block blob type supported (append/page blobs not used in file manager)
//! - AZ-011: No storage quota API (Azure Blob has no native quota endpoint)

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use futures_util::StreamExt;
use hmac::{Hmac, Mac};
use quick_xml::Reader;
use quick_xml::events::Event;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_LENGTH, CONTENT_TYPE};
use secrecy::ExposeSecret;
use sha2::Sha256;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{info, debug};

use super::{
    StorageProvider, ProviderType, ProviderError, RemoteEntry,
    sanitize_api_error, HttpRetryConfig, send_with_retry,
};
use super::types::AzureConfig;

type HmacSha256 = Hmac<Sha256>;

/// Azure API version
const API_VERSION: &str = "2024-11-04";

/// AZ-001: Threshold for switching from single Put Blob to block upload (100 MB)
const BLOCK_UPLOAD_THRESHOLD: u64 = 100 * 1024 * 1024;

/// AZ-001: Block size for Put Block requests (4 MB)
const BLOCK_SIZE: usize = 4 * 1024 * 1024;

/// AZ-016: Maximum time to wait for async copy completion (5 minutes)
const COPY_POLL_TIMEOUT_SECS: u64 = 300;

/// AZ-016: Interval between copy status polls (2 seconds)
const COPY_POLL_INTERVAL_MS: u64 = 2000;

/// AZ-005: Default retry configuration for Azure requests
fn azure_retry_config() -> HttpRetryConfig {
    HttpRetryConfig::default()
}

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
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            config,
            client,
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

    /// Build canonicalized headers string from a HeaderMap.
    /// Collects all `x-ms-*` headers, sorts them alphabetically,
    /// and formats as `headername:value\n`.
    fn build_canonical_headers(headers: &HeaderMap) -> String {
        let mut x_ms_headers: Vec<(String, String)> = Vec::new();
        for (name, value) in headers.iter() {
            let name_lower = name.as_str().to_lowercase();
            if name_lower.starts_with("x-ms-") {
                let val = value.to_str().unwrap_or("").trim().to_string();
                x_ms_headers.push((name_lower, val));
            }
        }
        x_ms_headers.sort_by(|a, b| a.0.cmp(&b.0));
        x_ms_headers.iter()
            .map(|(k, v)| format!("{}:{}\n", k, v))
            .collect::<String>()
    }

    /// Add SAS token or Shared Key auth to request
    fn sign_request(&self, method: &str, url: &str, headers: &HeaderMap, content_length: u64) -> Result<String, ProviderError> {
        if let Some(ref sas) = self.config.sas_token {
            // SAS token appended to URL
            let separator = if url.contains('?') { "&" } else { "?" };
            return Ok(format!("{}{}{}", url, separator, sas.expose_secret()));
        }

        // Shared Key signing
        let content_type = headers.get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        // Build canonical headers dynamically (sorted, lowercased, all x-ms-* headers)
        let canonical_headers = Self::build_canonical_headers(headers);

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
            "{}\n\n\n{}\n\n{}\n\n\n\n\n\n\n{}{}{}",
            method,
            if content_length > 0 { content_length.to_string() } else { String::new() },
            content_type,
            canonical_headers,
            canonicalized_resource,
            query_str,
        );

        let key_bytes = BASE64.decode(self.config.access_key.expose_secret())
            .map_err(|e| ProviderError::Other(format!("Invalid access key: {}", e)))?;

        let mut mac = HmacSha256::new_from_slice(&key_bytes)
            .map_err(|e| ProviderError::Other(format!("HMAC error: {}", e)))?;
        mac.update(string_to_sign.as_bytes());
        let signature = BASE64.encode(mac.finalize().into_bytes());

        Ok(format!("SharedKey {}:{}", self.config.account_name, signature))
    }

    /// AZ-005/AZ-006: Send a request with retry logic for transient errors (429/5xx).
    /// Handles both SAS token and Shared Key auth modes.
    /// Note: This cannot be used for streaming uploads (Put Blob with body) because
    /// `send_with_retry` clones the request body, which only works for byte bodies.
    async fn send_with_auth_and_retry(
        &self,
        method: reqwest::Method,
        url: &str,
        headers: HeaderMap,
        content_length: u64,
        body: Option<Vec<u8>>,
    ) -> Result<reqwest::Response, ProviderError> {
        let auth = self.sign_request(method.as_str(), url, &headers, content_length)?;

        let actual_url = if self.config.sas_token.is_some() { &auth } else { url };

        let mut builder = self.client.request(method.clone(), actual_url);
        let mut final_headers = headers.clone();
        if self.config.sas_token.is_none() {
            final_headers.insert("Authorization", HeaderValue::from_str(&auth)
                .map_err(|e| ProviderError::Other(format!("Invalid header value: {}", e)))?
            );
        }
        builder = builder.headers(final_headers);
        if let Some(ref body_bytes) = body {
            builder = builder.body(body_bytes.clone());
        }

        let request = builder.build()
            .map_err(|e| ProviderError::NetworkError(format!("Failed to build request: {}", e)))?;

        send_with_retry(&self.client, request, &azure_retry_config())
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))
    }

    /// Parse XML blob list response using quick-xml event-based parser.
    /// Returns (items, next_marker) where next_marker is Some if pagination continues.
    fn parse_blob_list(&self, xml: &str) -> (Vec<BlobItem>, Option<String>) {
        let mut items = Vec::new();
        let mut next_marker: Option<String> = None;

        let mut reader = Reader::from_str(xml);
        reader.config_mut().trim_text(true);

        // State machine for XML parsing
        #[derive(PartialEq)]
        enum ParseState {
            Root,
            BlobPrefix,
            BlobPrefixName,
            Blob,
            BlobName,
            BlobProperties,
            BlobContentLength,
            BlobLastModified,
            NextMarker,
        }

        let mut state = ParseState::Root;
        let mut current_name = String::new();
        let mut current_size: u64 = 0;
        let mut current_modified: Option<String> = None;
        let mut in_blob = false;
        let mut in_prefix = false;
        let mut buf = Vec::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => {
                    match e.name().as_ref() {
                        b"BlobPrefix" => {
                            state = ParseState::BlobPrefix;
                            in_prefix = true;
                            current_name.clear();
                        }
                        b"Blob" => {
                            state = ParseState::Blob;
                            in_blob = true;
                            current_name.clear();
                            current_size = 0;
                            current_modified = None;
                        }
                        b"Name" if in_prefix => {
                            state = ParseState::BlobPrefixName;
                        }
                        b"Name" if in_blob => {
                            state = ParseState::BlobName;
                        }
                        b"Properties" if in_blob => {
                            state = ParseState::BlobProperties;
                        }
                        b"Content-Length" if in_blob => {
                            state = ParseState::BlobContentLength;
                        }
                        b"Last-Modified" if in_blob => {
                            state = ParseState::BlobLastModified;
                        }
                        b"NextMarker" => {
                            state = ParseState::NextMarker;
                        }
                        _ => {}
                    }
                }
                Ok(Event::Text(ref e)) => {
                    let text = String::from_utf8_lossy(e.as_ref()).into_owned();
                    match state {
                        ParseState::BlobPrefixName => {
                            current_name = text;
                        }
                        ParseState::BlobName => {
                            current_name = text;
                        }
                        ParseState::BlobContentLength => {
                            current_size = text.parse().unwrap_or(0);
                        }
                        ParseState::BlobLastModified => {
                            if !text.is_empty() {
                                current_modified = Some(text);
                            }
                        }
                        ParseState::NextMarker => {
                            if !text.is_empty() {
                                next_marker = Some(text);
                            }
                        }
                        _ => {}
                    }
                }
                Ok(Event::End(ref e)) => {
                    match e.name().as_ref() {
                        b"BlobPrefix" => {
                            if in_prefix {
                                let display_name = current_name.trim_end_matches('/');
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
                                in_prefix = false;
                                state = ParseState::Root;
                            }
                        }
                        b"Blob" => {
                            if in_blob {
                                let relative = current_name.strip_prefix(&self.current_prefix).unwrap_or(&current_name);
                                let relative = relative.trim_start_matches('/');
                                if !relative.is_empty() && !relative.contains('/') {
                                    items.push(BlobItem {
                                        name: relative.to_string(),
                                        size: current_size,
                                        last_modified: current_modified.clone(),
                                        is_prefix: false,
                                    });
                                }
                                in_blob = false;
                                state = ParseState::Root;
                            }
                        }
                        b"Name" => {
                            if in_prefix {
                                state = ParseState::BlobPrefix;
                            } else if in_blob {
                                state = ParseState::Blob;
                            }
                        }
                        b"Properties" => {
                            if in_blob {
                                state = ParseState::Blob;
                            }
                        }
                        b"Content-Length" | b"Last-Modified" => {
                            if in_blob {
                                state = ParseState::BlobProperties;
                            }
                        }
                        b"NextMarker" => {
                            state = ParseState::Root;
                        }
                        _ => {}
                    }
                }
                Ok(Event::Eof) => break,
                // AZ-002: Log XML parse errors at debug level instead of silently swallowing
                Err(e) => {
                    debug!("Azure XML parse error: {}", e);
                    break;
                }
                _ => {}
            }
            buf.clear();
        }

        (items, next_marker)
    }

    fn resolve_blob_path(&self, path: &str) -> String {
        if path == "." || path.is_empty() {
            self.current_prefix.clone()
        } else if path.starts_with('/') {
            path.trim_start_matches('/').to_string()
        } else if self.current_prefix.is_empty() {
            path.to_string()
        } else {
            format!("{}{}", self.current_prefix, path)
        }
    }

    /// Execute a paginated blob list request, returning all items across pages.
    /// AZ-004: Checks HTTP status before attempting XML parsing.
    /// AZ-005: Uses retry logic for transient errors.
    async fn list_blobs_paginated(&self, base_url: &str) -> Result<Vec<BlobItem>, ProviderError> {
        let mut all_items = Vec::new();
        let mut marker: Option<String> = None;

        loop {
            let url = match &marker {
                Some(m) => format!("{}&marker={}", base_url, urlencoding::encode(m)),
                None => base_url.to_string(),
            };

            let mut headers = HeaderMap::new();
            let now = chrono::Utc::now().format("%a, %d %b %Y %H:%M:%S GMT").to_string();
            headers.insert("x-ms-date", HeaderValue::from_str(&now)
                .map_err(|e| ProviderError::Other(format!("Invalid header value: {}", e)))?);
            headers.insert("x-ms-version", HeaderValue::from_static(API_VERSION));

            let resp = self.send_with_auth_and_retry(
                reqwest::Method::GET, &url, headers, 0, None,
            ).await?;

            // AZ-004: Check HTTP status before parsing XML
            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                return Err(ProviderError::ServerError(
                    format!("List blobs failed (HTTP {}): {}", status.as_u16(), sanitize_api_error(&body))
                ));
            }

            let body = resp.text().await
                .map_err(|e| ProviderError::ParseError(e.to_string()))?;

            let (items, next_marker) = self.parse_blob_list(&body);
            all_items.extend(items);

            match next_marker {
                Some(m) if !m.is_empty() => { marker = Some(m); }
                _ => break,
            }
        }

        Ok(all_items)
    }

    /// AZ-001: Upload a single block via Put Block API.
    /// PUT /{container}/{blob}?comp=block&blockid={base64_id}
    async fn put_block(&self, blob_url: &str, block_id: &str, data: Vec<u8>) -> Result<(), ProviderError> {
        let encoded_block_id = urlencoding::encode(block_id);
        let url = format!("{}?comp=block&blockid={}", blob_url, encoded_block_id);
        let data_len = data.len() as u64;

        let mut headers = HeaderMap::new();
        let now = chrono::Utc::now().format("%a, %d %b %Y %H:%M:%S GMT").to_string();
        headers.insert("x-ms-date", HeaderValue::from_str(&now)
            .map_err(|e| ProviderError::Other(format!("Invalid header value: {}", e)))?);
        headers.insert("x-ms-version", HeaderValue::from_static(API_VERSION));
        headers.insert(CONTENT_LENGTH, HeaderValue::from(data_len));

        let resp = self.send_with_auth_and_retry(
            reqwest::Method::PUT, &url, headers, data_len, Some(data),
        ).await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::TransferFailed(
                format!("Put Block failed: {}", sanitize_api_error(&body))
            ));
        }

        Ok(())
    }

    /// AZ-001: Commit blocks via Put Block List API.
    /// PUT /{container}/{blob}?comp=blocklist with XML body listing all block IDs.
    async fn put_block_list(&self, blob_url: &str, block_ids: &[String]) -> Result<(), ProviderError> {
        let url = format!("{}?comp=blocklist", blob_url);

        // Build XML body: <BlockList><Latest>{id}</Latest>...</BlockList>
        let mut xml = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<BlockList>");
        for id in block_ids {
            xml.push_str(&format!("<Latest>{}</Latest>", id));
        }
        xml.push_str("</BlockList>");

        let body_bytes = xml.into_bytes();
        let body_len = body_bytes.len() as u64;

        let mut headers = HeaderMap::new();
        let now = chrono::Utc::now().format("%a, %d %b %Y %H:%M:%S GMT").to_string();
        headers.insert("x-ms-date", HeaderValue::from_str(&now)
            .map_err(|e| ProviderError::Other(format!("Invalid header value: {}", e)))?);
        headers.insert("x-ms-version", HeaderValue::from_static(API_VERSION));
        headers.insert(CONTENT_LENGTH, HeaderValue::from(body_len));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/xml"));

        let resp = self.send_with_auth_and_retry(
            reqwest::Method::PUT, &url, headers, body_len, Some(body_bytes),
        ).await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::TransferFailed(
                format!("Put Block List failed: {}", sanitize_api_error(&body))
            ));
        }

        Ok(())
    }

    /// AZ-016: Poll copy status until completion or timeout.
    /// Azure Copy Blob can be async for large blobs — must confirm completion before deleting source.
    async fn poll_copy_status(&self, dest_url: &str) -> Result<(), ProviderError> {
        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_secs(COPY_POLL_TIMEOUT_SECS);

        loop {
            if start.elapsed() > timeout {
                return Err(ProviderError::Other(
                    format!("Copy operation timed out after {}s", COPY_POLL_TIMEOUT_SECS)
                ));
            }

            let mut headers = HeaderMap::new();
            let now = chrono::Utc::now().format("%a, %d %b %Y %H:%M:%S GMT").to_string();
            headers.insert("x-ms-date", HeaderValue::from_str(&now)
                .map_err(|e| ProviderError::Other(format!("Invalid header value: {}", e)))?);
            headers.insert("x-ms-version", HeaderValue::from_static(API_VERSION));

            let resp = self.send_with_auth_and_retry(
                reqwest::Method::HEAD, dest_url, headers, 0, None,
            ).await?;

            if !resp.status().is_success() {
                return Err(ProviderError::Other(
                    format!("Copy status check failed: HTTP {}", resp.status())
                ));
            }

            let copy_status = resp.headers()
                .get("x-ms-copy-status")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("success")
                .to_lowercase();

            match copy_status.as_str() {
                "success" => return Ok(()),
                "failed" => {
                    let desc = resp.headers()
                        .get("x-ms-copy-status-description")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("unknown reason");
                    return Err(ProviderError::Other(
                        format!("Azure copy failed: {}", desc)
                    ));
                }
                "aborted" => {
                    return Err(ProviderError::Other("Azure copy was aborted".to_string()));
                }
                "pending" => {
                    debug!("Azure copy still pending, polling again in {}ms", COPY_POLL_INTERVAL_MS);
                    tokio::time::sleep(std::time::Duration::from_millis(COPY_POLL_INTERVAL_MS)).await;
                }
                other => {
                    debug!("Unknown copy status '{}', treating as success", other);
                    return Ok(());
                }
            }
        }
    }
}

#[async_trait]
impl StorageProvider for AzureProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }

    fn provider_type(&self) -> ProviderType { ProviderType::Azure }

    fn display_name(&self) -> String {
        format!("Azure:{}/{}", self.config.account_name, self.config.container)
    }

    fn is_connected(&self) -> bool { self.connected }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        // Validate config before attempting connection
        if self.config.account_name.is_empty() {
            return Err(ProviderError::InvalidConfig("Azure account name is empty".to_string()));
        }
        if self.config.container.is_empty() {
            return Err(ProviderError::InvalidConfig("Azure container name is empty".to_string()));
        }

        // Test connection by listing with max_results=1
        let endpoint = self.config.blob_endpoint();
        info!("Azure connect: account='{}', container='{}', endpoint='{}'",
            self.config.account_name, self.config.container, endpoint);
        let url = format!("{}/{}?restype=container&comp=list&maxresults=1",
            endpoint, self.config.container);

        let mut headers = HeaderMap::new();
        let now = chrono::Utc::now().format("%a, %d %b %Y %H:%M:%S GMT").to_string();
        headers.insert("x-ms-date", HeaderValue::from_str(&now)
            .map_err(|e| ProviderError::Other(format!("Invalid header value: {}", e)))?);
        headers.insert("x-ms-version", HeaderValue::from_static(API_VERSION));

        // AZ-005: Use retry for connect test
        let resp = self.send_with_auth_and_retry(
            reqwest::Method::GET, &url, headers, 0, None,
        ).await.map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::AuthenticationFailed(format!("Azure auth failed: {}", sanitize_api_error(&body))));
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

        let base_url = format!("{}/{}?restype=container&comp=list&delimiter=/{}",
            self.config.blob_endpoint(), self.config.container,
            prefix_param);

        let items = self.list_blobs_paginated(&base_url).await?;

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
        if path == ".." {
            return self.cd_up().await;
        }
        if path == "/" {
            self.current_prefix = String::new();
            return Ok(());
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

    /// AZ-003: Download with progress callback support.
    /// AZ-005: Uses retry for the initial GET request.
    async fn download(&mut self, remote_path: &str, local_path: &str, progress: Option<Box<dyn Fn(u64, u64) + Send>>) -> Result<(), ProviderError> {
        let blob_path = self.resolve_blob_path(remote_path);
        let url = self.blob_url(&blob_path);

        let mut headers = HeaderMap::new();
        let now = chrono::Utc::now().format("%a, %d %b %Y %H:%M:%S GMT").to_string();
        headers.insert("x-ms-date", HeaderValue::from_str(&now)
            .map_err(|e| ProviderError::Other(format!("Invalid header value: {}", e)))?);
        headers.insert("x-ms-version", HeaderValue::from_static(API_VERSION));

        let resp = self.send_with_auth_and_retry(
            reqwest::Method::GET, &url, headers, 0, None,
        ).await?;

        if !resp.status().is_success() {
            return Err(ProviderError::TransferFailed(format!("Download failed: {}", resp.status())));
        }

        // AZ-003: Get total size for progress reporting
        let total_size = resp.headers().get(CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);

        // H-01: Streaming download — chunked writes instead of buffering entire response
        let mut stream = resp.bytes_stream();
        let mut file = tokio::fs::File::create(local_path).await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        let mut bytes_received: u64 = 0;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
            file.write_all(&chunk).await
                .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

            // AZ-003: Report download progress
            bytes_received += chunk.len() as u64;
            if let Some(ref cb) = progress {
                cb(bytes_received, total_size);
            }
        }

        Ok(())
    }

    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        let blob_path = self.resolve_blob_path(remote_path);
        let url = self.blob_url(&blob_path);

        let mut headers = HeaderMap::new();
        let now = chrono::Utc::now().format("%a, %d %b %Y %H:%M:%S GMT").to_string();
        headers.insert("x-ms-date", HeaderValue::from_str(&now)
            .map_err(|e| ProviderError::Other(format!("Invalid header value: {}", e)))?);
        headers.insert("x-ms-version", HeaderValue::from_static(API_VERSION));

        // AZ-005: Use retry
        let resp = self.send_with_auth_and_retry(
            reqwest::Method::GET, &url, headers, 0, None,
        ).await?;

        if !resp.status().is_success() {
            return Err(ProviderError::TransferFailed(format!("Download failed: {}", resp.status())));
        }

        // H2: Size-limited download to prevent OOM on large files
        super::response_bytes_with_limit(resp, super::MAX_DOWNLOAD_TO_BYTES).await
    }

    /// AZ-001: Upload with block upload support for files >100MB.
    /// AZ-003: Reports upload progress.
    /// - Files <= 100MB: Single Put Blob (streaming)
    /// - Files > 100MB: Put Block (4MB chunks) + Put Block List
    async fn upload(&mut self, local_path: &str, remote_path: &str, progress: Option<Box<dyn Fn(u64, u64) + Send>>) -> Result<(), ProviderError> {
        let blob_path = self.resolve_blob_path(remote_path);
        let url = self.blob_url(&blob_path);

        let file_meta = tokio::fs::metadata(local_path).await
            .map_err(ProviderError::IoError)?;
        let file_len = file_meta.len();

        if file_len > BLOCK_UPLOAD_THRESHOLD {
            // AZ-001: Block upload for large files
            self.upload_blocks(local_path, &url, file_len, progress).await
        } else {
            // Small file: single Put Blob with streaming body
            self.upload_single(local_path, &url, file_len, progress).await
        }
    }

    async fn mkdir(&mut self, _path: &str) -> Result<(), ProviderError> {
        // Azure Blob Storage doesn't have real directories
        // Virtual directories are created implicitly by blobs with "/" in names
        Ok(())
    }

    /// AZ-012: Delete with lease conflict detection.
    /// If delete fails with HTTP 412 (Precondition Failed), returns a clear error
    /// indicating a lease conflict. Full lease management (acquire/break/release)
    /// is not implemented as it is rarely needed for file manager use cases.
    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        let blob_path = self.resolve_blob_path(path);
        let url = self.blob_url(&blob_path);

        let mut headers = HeaderMap::new();
        let now = chrono::Utc::now().format("%a, %d %b %Y %H:%M:%S GMT").to_string();
        headers.insert("x-ms-date", HeaderValue::from_str(&now)
            .map_err(|e| ProviderError::Other(format!("Invalid header value: {}", e)))?);
        headers.insert("x-ms-version", HeaderValue::from_static(API_VERSION));

        // AZ-005: Use retry
        let resp = self.send_with_auth_and_retry(
            reqwest::Method::DELETE, &url, headers, 0, None,
        ).await?;

        let status = resp.status();
        if !status.is_success() && status.as_u16() != 202 {
            // AZ-012: Detect lease conflict (HTTP 412 Precondition Failed)
            if status.as_u16() == 412 {
                return Err(ProviderError::Other(
                    "Delete failed: blob has an active lease. Break or release the lease first.".to_string()
                ));
            }
            return Err(ProviderError::Other(format!("Delete failed: {}", status)));
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

    /// AZ-016: Rename via Copy + Delete with async copy polling.
    /// Azure Copy Blob can be async for large blobs. After issuing the copy,
    /// we check `x-ms-copy-status` and poll until completion before deleting the source.
    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        // Azure doesn't have native rename - must copy then delete
        let from_blob = self.resolve_blob_path(from);
        let to_blob = self.resolve_blob_path(to);

        let source_url = self.blob_url(&from_blob);
        let dest_url = self.blob_url(&to_blob);

        let mut headers = HeaderMap::new();
        let now = chrono::Utc::now().format("%a, %d %b %Y %H:%M:%S GMT").to_string();
        headers.insert("x-ms-date", HeaderValue::from_str(&now)
            .map_err(|e| ProviderError::Other(format!("Invalid header value: {}", e)))?);
        headers.insert("x-ms-version", HeaderValue::from_static(API_VERSION));
        headers.insert("x-ms-copy-source", HeaderValue::from_str(&source_url)
            .map_err(|e| ProviderError::Other(format!("Invalid header value: {}", e)))?);
        // Azure requires explicit Content-Length: 0 for PUT Copy Blob
        headers.insert(CONTENT_LENGTH, HeaderValue::from_static("0"));

        // AZ-005: Use retry for copy request
        let resp = self.send_with_auth_and_retry(
            reqwest::Method::PUT, &dest_url, headers, 0, None,
        ).await?;

        if !resp.status().is_success() {
            return Err(ProviderError::Other(format!("Copy failed: {}", resp.status())));
        }

        // AZ-016: Check copy status — may be async for large blobs
        let copy_status = resp.headers()
            .get("x-ms-copy-status")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("success")
            .to_lowercase();

        if copy_status == "pending" {
            debug!("Azure copy is async (pending), polling for completion");
            self.poll_copy_status(&dest_url).await?;
        } else if copy_status == "failed" {
            let desc = resp.headers()
                .get("x-ms-copy-status-description")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("unknown reason");
            return Err(ProviderError::Other(format!("Copy failed: {}", desc)));
        }

        // Delete original only after copy is confirmed
        self.delete(from).await?;

        Ok(())
    }

    /// AZ-007: Extracts Content-Type from HEAD response to populate mime_type.
    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        let blob_path = self.resolve_blob_path(path);
        let url = self.blob_url(&blob_path);

        let mut headers = HeaderMap::new();
        let now = chrono::Utc::now().format("%a, %d %b %Y %H:%M:%S GMT").to_string();
        headers.insert("x-ms-date", HeaderValue::from_str(&now)
            .map_err(|e| ProviderError::Other(format!("Invalid header value: {}", e)))?);
        headers.insert("x-ms-version", HeaderValue::from_static(API_VERSION));

        // AZ-005: Use retry
        let resp = self.send_with_auth_and_retry(
            reqwest::Method::HEAD, &url, headers, 0, None,
        ).await?;

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

        // AZ-007: Extract Content-Type for mime_type
        let mime_type = resp.headers().get(CONTENT_TYPE)
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
            is_symlink: false, link_target: None, mime_type,
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

        let key_bytes = BASE64.decode(self.config.access_key.expose_secret())
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

/// Private upload helper methods (outside trait impl to avoid async_trait limitations)
impl AzureProvider {
    /// Single Put Blob upload for files <= BLOCK_UPLOAD_THRESHOLD.
    /// AZ-003: Reports progress after completion.
    async fn upload_single(
        &self,
        local_path: &str,
        url: &str,
        file_len: u64,
        progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let file = tokio::fs::File::open(local_path).await
            .map_err(ProviderError::IoError)?;
        let stream = tokio_util::io::ReaderStream::new(file);
        let body = reqwest::Body::wrap_stream(stream);

        let mut headers = HeaderMap::new();
        let now = chrono::Utc::now().format("%a, %d %b %Y %H:%M:%S GMT").to_string();
        headers.insert("x-ms-date", HeaderValue::from_str(&now)
            .map_err(|e| ProviderError::Other(format!("Invalid header value: {}", e)))?);
        headers.insert("x-ms-version", HeaderValue::from_static(API_VERSION));
        headers.insert("x-ms-blob-type", HeaderValue::from_static("BlockBlob"));
        headers.insert(CONTENT_LENGTH, HeaderValue::from(file_len));

        let auth = self.sign_request("PUT", url, &headers, file_len)?;

        // Cannot use send_with_auth_and_retry for streaming body (body is not cloneable).
        // Streaming uploads are not retryable at this level — the caller can retry the entire upload.
        let resp = if self.config.sas_token.is_some() {
            self.client.put(&auth).headers(headers).body(body).send().await
        } else {
            headers.insert("Authorization", HeaderValue::from_str(&auth)
                .map_err(|e| ProviderError::Other(format!("Invalid header value: {}", e)))?
            );
            self.client.put(url).headers(headers).body(body).send().await
        }.map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::TransferFailed(format!("Upload failed: {}", sanitize_api_error(&body))));
        }

        // AZ-003: Report completion
        if let Some(ref cb) = progress {
            cb(file_len, file_len);
        }

        Ok(())
    }

    /// AZ-001: Block upload for files > BLOCK_UPLOAD_THRESHOLD.
    /// Splits file into 4MB blocks, uploads each with Put Block, then commits with Put Block List.
    /// AZ-003: Reports progress after each block.
    async fn upload_blocks(
        &self,
        local_path: &str,
        blob_url: &str,
        file_len: u64,
        progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let mut file = tokio::fs::File::open(local_path).await
            .map_err(ProviderError::IoError)?;

        let mut block_ids: Vec<String> = Vec::new();
        let mut bytes_uploaded: u64 = 0;
        let mut block_index: u32 = 0;

        loop {
            let mut buf = vec![0u8; BLOCK_SIZE];
            let mut filled = 0;

            // Read a full block (or whatever remains)
            while filled < BLOCK_SIZE {
                let n = file.read(&mut buf[filled..]).await
                    .map_err(|e| ProviderError::TransferFailed(format!("File read error: {}", e)))?;
                if n == 0 {
                    break; // EOF
                }
                filled += n;
            }

            if filled == 0 {
                break; // No more data
            }

            buf.truncate(filled);

            // Generate block ID: zero-padded index, base64-encoded
            // All block IDs in a block list must be the same length, so pad to 6 digits
            let block_id_raw = format!("{:06}", block_index);
            let block_id = BASE64.encode(block_id_raw.as_bytes());

            self.put_block(blob_url, &block_id, buf).await?;

            block_ids.push(block_id);
            bytes_uploaded += filled as u64;
            block_index += 1;

            // AZ-003: Report progress after each block
            if let Some(ref cb) = progress {
                cb(bytes_uploaded, file_len);
            }
        }

        if block_ids.is_empty() {
            return Err(ProviderError::TransferFailed("No data read from file".to_string()));
        }

        // Commit all blocks
        self.put_block_list(blob_url, &block_ids).await?;

        // AZ-003: Final progress report
        if let Some(ref cb) = progress {
            cb(file_len, file_len);
        }

        debug!("Block upload complete: {} blocks, {} bytes", block_ids.len(), file_len);
        Ok(())
    }
}
