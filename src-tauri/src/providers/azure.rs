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

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use futures_util::StreamExt;
use hmac::{Hmac, Mac};
use quick_xml::events::Event;
use quick_xml::Reader;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_LENGTH, CONTENT_TYPE};
use secrecy::ExposeSecret;
use sha2::Sha256;
use tokio::io::AsyncReadExt;
use tracing::{debug, info, warn};

use super::types::AzureConfig;
use super::{
    sanitize_api_error, send_with_retry, HttpRetryConfig, MultipartHandle, ProviderError,
    ProviderTransferExecutorKind, ProviderType, RemoteEntry, ShareLinkCapabilities,
    ShareLinkOptions, ShareLinkResult, StorageProvider, UploadedPart,
};

type HmacSha256 = Hmac<Sha256>;

/// Azure API version
const API_VERSION: &str = "2024-11-04";

/// AZ-001: Threshold for switching from single Put Blob to block upload (100 MB)
const BLOCK_UPLOAD_THRESHOLD: u64 = 100 * 1024 * 1024;

/// AZ-001: Block size for Put Block requests (4 MB)
const BLOCK_SIZE: usize = 4 * 1024 * 1024;

/// DAG multipart uses a larger chunk than the legacy sequential helper so the
/// graph has enough fan-out without creating excessive Azure Put Block calls.
const DAG_MULTIPART_BLOCK_SIZE: u64 = 8 * 1024 * 1024;
const DAG_MULTIPART_MAX_PARALLEL: u8 = 4;
/// Size at or above which the shaped graph fans out into parallel Put Block
/// uploads. Below it a single-shot upload is faster: the 2026-05-29 lab
/// benchmark measured the parallel fan-out losing 17% at 100 MiB but winning
/// 69% at 256 MiB, so the crossover is kept conservatively at 200 MiB (audit
/// Patch Set 2). Distinct from the legacy `BLOCK_UPLOAD_THRESHOLD`.
const DAG_MULTIPART_THRESHOLD: u64 = 200 * 1024 * 1024;

/// AZ-016: Maximum time to wait for async copy completion (5 minutes)
const COPY_POLL_TIMEOUT_SECS: u64 = 300;

/// AZ-016: Interval between copy status polls (2 seconds)
const COPY_POLL_INTERVAL_MS: u64 = 2000;

/// AZ-005: Default retry configuration for Azure requests
fn azure_retry_config() -> HttpRetryConfig {
    HttpRetryConfig::default()
}

/// Parse Azure XML error response into a clean "Code: Message" string.
/// Falls back to sanitize_api_error if XML parsing fails.
fn parse_azure_xml_error(body: &str) -> String {
    let mut reader = Reader::from_str(body);
    // No trim_text: fragments around XML entities must survive intact;
    // code/message are trimmed once at output below.
    reader.config_mut().trim_text(false);
    let mut code = String::new();
    let mut message = String::new();
    let mut current_tag = String::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) => {
                current_tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
            }
            Ok(Event::Text(ref e)) => {
                let text = String::from_utf8_lossy(e.as_ref()).into_owned();
                if text.trim().is_empty() {
                    continue;
                }
                match current_tag.as_str() {
                    "Code" => code.push_str(&text),
                    "Message" if message.lines().count() <= 1 => {
                        // Azure messages often end with technical details after \n;
                        // keep only the first line of the FIRST text fragment.
                        message.push_str(text.lines().next().unwrap_or(&text));
                    }
                    _ => {}
                }
            }
            Ok(Event::GeneralRef(ref e)) => {
                if let Some(ch) = super::xml_text::xml_entity_to_str(e.as_ref()) {
                    match current_tag.as_str() {
                        "Code" => code.push_str(&ch),
                        "Message" if message.lines().count() <= 1 => message.push_str(&ch),
                        _ => {}
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    let code = code.trim().to_string();
    let message = message.trim().to_string();
    if !code.is_empty() {
        if message.is_empty() {
            code
        } else {
            format!("{}: {}", code, message)
        }
    } else {
        sanitize_api_error(body)
    }
}

/// KE-E2: Azure Blob rate-limit detection.
///
/// Azure Storage signals throttling primarily through **503 ServerBusy**
/// (`<Code>ServerBusy</Code>` in the XML body), occasionally accompanied
/// by **429 TooManyRequests** on the newer DFS / hot-path endpoints. The
/// `Retry-After` header is always present on a real throttle response
/// (Azure spec). Other 503 reasons (`InternalError`, `OperationTimedOut`)
/// are surfaced as retryable transients by `send_with_retry` but are NOT
/// rate-limit signals; only the explicit `ServerBusy` body counts.
fn azure_is_rate_limited(status: u16, body: &str) -> bool {
    if status == 429 {
        return true;
    }
    if status == 503 {
        return body.contains("<Code>ServerBusy</Code>");
    }
    false
}

/// KE-E2: Compute the marker tail to append to an Azure `ProviderError`
/// when the response was rate-limited and a usable `Retry-After` header
/// was present. Returns `None` when not a throttle signal or the hint is
/// missing/unparseable. Pure-fn for test coverage.
fn azure_retry_marker_tail(status: u16, body: &str, retry_header: Option<&str>) -> Option<String> {
    if !azure_is_rate_limited(status, body) {
        return None;
    }
    let hint = super::retry_after::parse_retry_after_seconds(retry_header.unwrap_or(""))?;
    Some(crate::transfer_dag::adaptive::embed_retry_after_marker(
        hint.as_secs(),
    ))
}

/// KE-E2: Build the error message tail for an Azure HTTP failure,
/// appending the Retry-After marker if the response is a throttle
/// signal. Use this at every error site that returns
/// `ProviderError::TransferFailed(...)` or `Other(...)` from an Azure
/// HTTP response.
fn format_azure_error(
    prefix: &str,
    status: reqwest::StatusCode,
    body: &str,
    retry_header: Option<&str>,
) -> String {
    let mut msg = format!("{}: {} ({})", prefix, parse_azure_xml_error(body), status);
    if let Some(tail) = azure_retry_marker_tail(status.as_u16(), body, retry_header) {
        msg.push_str(&tail);
    }
    msg
}

/// Azure list blobs XML item
#[derive(Debug)]
struct BlobItem {
    name: String,
    size: u64,
    last_modified: Option<String>,
    is_prefix: bool, // virtual directory
}

/// Azure Blob Storage Provider
pub struct AzureProvider {
    config: AzureConfig,
    client: reqwest::Client,
    connected: bool,
    current_prefix: String,
    /// KE-B4.1: Override for the number of `Put Block` requests in flight
    /// during `upload_blocks`. `None` keeps the historical sequential
    /// behaviour (one block at a time). Range `[1, MAX]` enforced by the
    /// setter; `0` resets to default.
    upload_concurrency_override: Option<usize>,
    /// KE-B4.2: Reserved for future Content-MD5 metadata gating. AeroFTP
    /// does not currently send `x-ms-blob-content-md5` on uploads, so this
    /// flag is structurally wired but has no observable effect today. It
    /// will gate the future MD5-on-upload code path once that lands.
    disable_checksum: bool,
    /// KE-B4.3: Apply this access tier to every successful upload as a
    /// post-PUT `Set Blob Tier` call. Values: `Hot`, `Cool`, `Cold`,
    /// `Archive`. Vendor-specific tiers are passed through; Azure rejects
    /// unknown values at the API level.
    access_tier_override: Option<String>,
    /// KE-B4.4: When true, every upload first issues a HEAD on the target
    /// blob; if the response indicates the existing blob is in Archive
    /// tier, the blob is DELETEd before the new PUT. Without this flag,
    /// overwriting an Archive blob fails with `BlobArchived`.
    archive_tier_delete: bool,
}

impl Clone for AzureProvider {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            client: self.client.clone(),
            connected: self.connected,
            current_prefix: self.current_prefix.clone(),
            upload_concurrency_override: self.upload_concurrency_override,
            disable_checksum: self.disable_checksum,
            access_tier_override: self.access_tier_override.clone(),
            archive_tier_delete: self.archive_tier_delete,
        }
    }
}

impl AzureProvider {
    pub fn new(config: AzureConfig) -> Self {
        let client = reqwest::Client::builder()
            .user_agent(crate::providers::AEROFTP_USER_AGENT)
            .connect_timeout(std::time::Duration::from_secs(30))
            .read_timeout(std::time::Duration::from_secs(1800))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            config,
            client,
            connected: false,
            current_prefix: String::new(),
            upload_concurrency_override: None,
            disable_checksum: false,
            access_tier_override: None,
            archive_tier_delete: false,
        }
    }

    /// Default parallelism for block uploads. `1` keeps the historical
    /// sequential ordering used by `upload_blocks`. Overridable through
    /// `set_upload_concurrency`.
    pub const UPLOAD_CONCURRENCY_DEFAULT: usize = 1;
    /// Ceiling enforced by `set_upload_concurrency`. Azure tolerates more
    /// than this but it rarely improves throughput once the link is full.
    pub const UPLOAD_CONCURRENCY_MAX: usize = 32;

    /// KE-B4.1: Override the number of `Put Block` requests in flight
    /// during `upload_blocks`. Clamped to `[1, UPLOAD_CONCURRENCY_MAX]`.
    /// `0` resets to default.
    pub fn set_upload_concurrency(&mut self, parts_in_flight: usize) {
        if parts_in_flight == 0 {
            self.upload_concurrency_override = None;
            return;
        }
        self.upload_concurrency_override =
            Some(parts_in_flight.clamp(1, Self::UPLOAD_CONCURRENCY_MAX));
    }

    /// Effective block-upload concurrency, honoring any override.
    pub fn effective_upload_concurrency(&self) -> usize {
        self.upload_concurrency_override
            .unwrap_or(Self::UPLOAD_CONCURRENCY_DEFAULT)
    }

    /// KE-B4.2: Toggle the (currently structural) `disable_checksum` flag.
    /// No-op on the wire today; reserved for the future MD5-on-upload path.
    pub fn set_disable_checksum(&mut self, enabled: bool) {
        self.disable_checksum = enabled;
    }

    /// KE-B4.3: Set the access tier applied to every successful upload as
    /// a post-PUT `Set Blob Tier` call. Whitespace-only normalises to None.
    pub fn set_access_tier(&mut self, tier: Option<String>) {
        self.access_tier_override = tier.filter(|s| !s.trim().is_empty());
    }

    /// KE-B4.4: Toggle the pre-upload Archive-tier delete pattern.
    pub fn set_archive_tier_delete(&mut self, enabled: bool) {
        self.archive_tier_delete = enabled;
    }

    /// Effective access tier: runtime override or None (no tier change).
    fn effective_access_tier(&self) -> Option<&str> {
        self.access_tier_override.as_deref()
    }

    fn dag_block_id(part_number: u32) -> Result<String, ProviderError> {
        if part_number == 0 {
            return Err(ProviderError::InvalidConfig(
                "Azure multipart part numbers are 1-based".to_string(),
            ));
        }

        // Azure requires every block ID for a blob to have the same length
        // before Base64 encoding. Twenty digits leaves ample room while
        // keeping the final encoded token compact and XML-safe.
        let raw = format!("{:020}", part_number);
        Ok(BASE64.encode(raw.as_bytes()))
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
        x_ms_headers
            .iter()
            .map(|(k, v)| format!("{}:{}\n", k, v))
            .collect::<String>()
    }

    /// Add SAS token or Shared Key auth to request
    fn sign_request(
        &self,
        method: &str,
        url: &str,
        headers: &HeaderMap,
        content_length: u64,
    ) -> Result<String, ProviderError> {
        if let Some(ref sas) = self.config.sas_token {
            // SAS token appended to URL
            let separator = if url.contains('?') { "&" } else { "?" };
            return Ok(format!("{}{}{}", url, separator, sas.expose_secret()));
        }

        // Shared Key signing
        let content_type = headers
            .get(CONTENT_TYPE)
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
        let mut query_parts: Vec<(String, String)> = parsed
            .query_pairs()
            .map(|(k, v)| (k.to_lowercase(), v.to_string()))
            .collect();
        query_parts.sort();
        let query_str = query_parts
            .iter()
            .map(|(k, v)| format!("\n{}:{}", k, v))
            .collect::<String>();

        let string_to_sign = format!(
            "{}\n\n\n{}\n\n{}\n\n\n\n\n\n\n{}{}{}",
            method,
            if content_length > 0 {
                content_length.to_string()
            } else {
                String::new()
            },
            content_type,
            canonical_headers,
            canonicalized_resource,
            query_str,
        );

        let key_bytes = BASE64
            .decode(self.config.access_key.expose_secret())
            .map_err(|e| ProviderError::Other(format!("Invalid access key: {}", e)))?;

        let mut mac = HmacSha256::new_from_slice(&key_bytes)
            .map_err(|e| ProviderError::Other(format!("HMAC error: {}", e)))?;
        mac.update(string_to_sign.as_bytes());
        let signature = BASE64.encode(mac.finalize().into_bytes());

        Ok(format!(
            "SharedKey {}:{}",
            self.config.account_name, signature
        ))
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

        let actual_url = if self.config.sas_token.is_some() {
            &auth
        } else {
            url
        };

        let mut builder = self.client.request(method.clone(), actual_url);
        let mut final_headers = headers.clone();
        if self.config.sas_token.is_none() {
            final_headers.insert(
                "Authorization",
                HeaderValue::from_str(&auth)
                    .map_err(|e| ProviderError::Other(format!("Invalid header value: {}", e)))?,
            );
        }
        builder = builder.headers(final_headers);
        if let Some(ref body_bytes) = body {
            builder = builder.body(body_bytes.clone());
        }

        let request = builder
            .build()
            .map_err(|e| ProviderError::NetworkError(format!("Failed to build request: {}", e)))?;

        send_with_retry(&self.client, request, &azure_retry_config())
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))
    }

    /// Parse XML blob list response using quick-xml event-based parser.
    /// Returns (items, next_marker) where next_marker is Some if pagination continues.
    /// Parse one page of a `comp=list` response.
    ///
    /// `strip_prefix` MUST be the blob prefix the request was issued with (the
    /// `&prefix=` query value), not the provider's `current_prefix`: Azure
    /// echoes every `<Name>` as a full path from the container root, so the
    /// listed prefix is what makes an entry relative. Using `current_prefix`
    /// instead made every one-shot `ls /a/b` (CLI, MCP, first GUI call, sync
    /// scan) return an empty directory, because each blob still carried a `/`
    /// and was dropped by the depth filter below.
    fn parse_blob_list(xml: &str, strip_prefix: &str) -> (Vec<BlobItem>, Option<String>) {
        let mut items = Vec::new();
        let mut next_marker: Option<String> = None;

        let mut reader = Reader::from_str(xml);
        // No trim_text: blob-name fragments around XML entities must
        // survive intact (entity-adjacent spaces are part of the name).
        reader.config_mut().trim_text(false);

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
                Ok(Event::Start(ref e)) => match e.name().as_ref() {
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
                },
                Ok(Event::Text(ref e)) => {
                    let text = String::from_utf8_lossy(e.as_ref()).into_owned();
                    // Skip indentation-only fragments, but preserve
                    // whitespace while a blob/prefix <Name> is open: there
                    // it is payload (e.g. `a&amp; &amp;b.txt`).
                    if text.trim().is_empty()
                        && !matches!(state, ParseState::BlobName | ParseState::BlobPrefixName)
                    {
                        buf.clear();
                        continue;
                    }
                    match state {
                        ParseState::BlobPrefixName => {
                            current_name.push_str(&text);
                        }
                        ParseState::BlobName => {
                            current_name.push_str(&text);
                        }
                        ParseState::BlobContentLength => {
                            current_size = text.trim().parse().unwrap_or(current_size);
                        }
                        ParseState::BlobLastModified => {
                            current_modified
                                .get_or_insert_with(String::new)
                                .push_str(&text);
                        }
                        ParseState::NextMarker => {
                            next_marker.get_or_insert_with(String::new).push_str(&text);
                        }
                        _ => {}
                    }
                }
                Ok(Event::GeneralRef(ref e)) => {
                    if let Some(ch) = super::xml_text::xml_entity_to_str(e.as_ref()) {
                        match state {
                            ParseState::BlobPrefixName | ParseState::BlobName => {
                                current_name.push_str(&ch);
                            }
                            ParseState::BlobLastModified => {
                                current_modified
                                    .get_or_insert_with(String::new)
                                    .push_str(&ch);
                            }
                            ParseState::NextMarker => {
                                next_marker.get_or_insert_with(String::new).push_str(&ch);
                            }
                            _ => {}
                        }
                    }
                }
                Ok(Event::End(ref e)) => match e.name().as_ref() {
                    b"BlobPrefix" if in_prefix => {
                        let display_name = current_name.trim_end_matches('/');
                        let relative = display_name
                            .strip_prefix(strip_prefix)
                            .unwrap_or(display_name);
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
                    b"Blob" if in_blob => {
                        let relative = current_name
                            .strip_prefix(strip_prefix)
                            .unwrap_or(&current_name);
                        let relative = relative.trim_start_matches('/');
                        if !relative.is_empty() && !relative.contains('/') {
                            items.push(BlobItem {
                                name: relative.to_string(),
                                size: current_size,
                                last_modified: current_modified
                                    .as_ref()
                                    .map(|m| m.trim().to_string()),
                                is_prefix: false,
                            });
                        }
                        in_blob = false;
                        state = ParseState::Root;
                    }
                    b"Name" => {
                        if in_prefix {
                            state = ParseState::BlobPrefix;
                        } else if in_blob {
                            state = ParseState::Blob;
                        }
                    }
                    b"Properties" if in_blob => {
                        state = ParseState::Blob;
                    }
                    b"Content-Length" | b"Last-Modified" if in_blob => {
                        state = ParseState::BlobProperties;
                    }
                    b"NextMarker" => {
                        state = ParseState::Root;
                    }
                    _ => {}
                },
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

        (items, next_marker.map(|m| m.trim().to_string()))
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
    async fn list_blobs_paginated(
        &self,
        base_url: &str,
        strip_prefix: &str,
    ) -> Result<Vec<BlobItem>, ProviderError> {
        let mut all_items = Vec::new();
        let mut marker: Option<String> = None;

        loop {
            let url = match &marker {
                Some(m) => format!("{}&marker={}", base_url, urlencoding::encode(m)),
                None => base_url.to_string(),
            };

            let mut headers = HeaderMap::new();
            let now = chrono::Utc::now()
                .format("%a, %d %b %Y %H:%M:%S GMT")
                .to_string();
            headers.insert(
                "x-ms-date",
                HeaderValue::from_str(&now)
                    .map_err(|e| ProviderError::Other(format!("Invalid header value: {}", e)))?,
            );
            headers.insert("x-ms-version", HeaderValue::from_static(API_VERSION));

            let resp = self
                .send_with_auth_and_retry(reqwest::Method::GET, &url, headers, 0, None)
                .await?;

            // AZ-004: Check HTTP status before parsing XML
            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                return Err(ProviderError::ServerError(format!(
                    "List blobs failed (HTTP {}): {}",
                    status.as_u16(),
                    parse_azure_xml_error(&body)
                )));
            }

            let body = resp
                .text()
                .await
                .map_err(|e| ProviderError::ParseError(e.to_string()))?;

            let (items, next_marker) = Self::parse_blob_list(&body, strip_prefix);
            all_items.extend(items);

            match next_marker {
                Some(m) if !m.is_empty() => {
                    marker = Some(m);
                }
                _ => break,
            }
        }

        Ok(all_items)
    }

    /// AZ-001: Upload a single block via Put Block API.
    /// PUT /{container}/{blob}?comp=block&blockid={base64_id}
    async fn put_block(
        &self,
        blob_url: &str,
        block_id: &str,
        data: Vec<u8>,
    ) -> Result<(), ProviderError> {
        let encoded_block_id = urlencoding::encode(block_id);
        let url = format!("{}?comp=block&blockid={}", blob_url, encoded_block_id);
        let data_len = data.len() as u64;

        let mut headers = HeaderMap::new();
        let now = chrono::Utc::now()
            .format("%a, %d %b %Y %H:%M:%S GMT")
            .to_string();
        headers.insert(
            "x-ms-date",
            HeaderValue::from_str(&now)
                .map_err(|e| ProviderError::Other(format!("Invalid header value: {}", e)))?,
        );
        headers.insert("x-ms-version", HeaderValue::from_static(API_VERSION));
        headers.insert(CONTENT_LENGTH, HeaderValue::from(data_len));

        let resp = self
            .send_with_auth_and_retry(reqwest::Method::PUT, &url, headers, data_len, Some(data))
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let retry_header = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .map(String::from);
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::TransferFailed(format_azure_error(
                "Put Block failed",
                status,
                &body,
                retry_header.as_deref(),
            )));
        }

        Ok(())
    }

    /// AZ-001: Commit blocks via Put Block List API.
    /// PUT /{container}/{blob}?comp=blocklist with XML body listing all block IDs.
    async fn put_block_list(
        &self,
        blob_url: &str,
        block_ids: &[String],
    ) -> Result<(), ProviderError> {
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
        let now = chrono::Utc::now()
            .format("%a, %d %b %Y %H:%M:%S GMT")
            .to_string();
        headers.insert(
            "x-ms-date",
            HeaderValue::from_str(&now)
                .map_err(|e| ProviderError::Other(format!("Invalid header value: {}", e)))?,
        );
        headers.insert("x-ms-version", HeaderValue::from_static(API_VERSION));
        headers.insert(CONTENT_LENGTH, HeaderValue::from(body_len));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/xml"));

        let resp = self
            .send_with_auth_and_retry(
                reqwest::Method::PUT,
                &url,
                headers,
                body_len,
                Some(body_bytes),
            )
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let retry_header = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .map(String::from);
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::TransferFailed(format_azure_error(
                "Put Block List failed",
                status,
                &body,
                retry_header.as_deref(),
            )));
        }

        Ok(())
    }

    /// AZ-016: Poll copy status until completion or timeout.
    /// Azure Copy Blob can be async for large blobs: must confirm completion before deleting source.
    async fn poll_copy_status(&self, dest_url: &str) -> Result<(), ProviderError> {
        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_secs(COPY_POLL_TIMEOUT_SECS);

        loop {
            if start.elapsed() > timeout {
                return Err(ProviderError::Other(format!(
                    "Copy operation timed out after {}s",
                    COPY_POLL_TIMEOUT_SECS
                )));
            }

            let mut headers = HeaderMap::new();
            let now = chrono::Utc::now()
                .format("%a, %d %b %Y %H:%M:%S GMT")
                .to_string();
            headers.insert(
                "x-ms-date",
                HeaderValue::from_str(&now)
                    .map_err(|e| ProviderError::Other(format!("Invalid header value: {}", e)))?,
            );
            headers.insert("x-ms-version", HeaderValue::from_static(API_VERSION));

            let resp = self
                .send_with_auth_and_retry(reqwest::Method::HEAD, dest_url, headers, 0, None)
                .await?;

            if !resp.status().is_success() {
                return Err(ProviderError::Other(format!(
                    "Copy status check failed: HTTP {}",
                    resp.status()
                )));
            }

            let copy_status = resp
                .headers()
                .get("x-ms-copy-status")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("success")
                .to_lowercase();

            match copy_status.as_str() {
                "success" => return Ok(()),
                "failed" => {
                    let desc = resp
                        .headers()
                        .get("x-ms-copy-status-description")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("unknown reason");
                    return Err(ProviderError::Other(format!("Azure copy failed: {}", desc)));
                }
                "aborted" => {
                    return Err(ProviderError::Other("Azure copy was aborted".to_string()));
                }
                "pending" => {
                    debug!(
                        "Azure copy still pending, polling again in {}ms",
                        COPY_POLL_INTERVAL_MS
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(COPY_POLL_INTERVAL_MS))
                        .await;
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
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn provider_type(&self) -> ProviderType {
        ProviderType::Azure
    }

    fn display_name(&self) -> String {
        format!(
            "Azure:{}/{}",
            self.config.account_name, self.config.container
        )
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        // Validate config before attempting connection
        if self.config.account_name.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Azure account name is empty".to_string(),
            ));
        }
        if self.config.container.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Azure container name is empty".to_string(),
            ));
        }

        // Test connection by listing with max_results=1
        let endpoint = self.config.blob_endpoint();
        info!(
            "Azure connect: account='{}', container='{}', endpoint='{}'",
            self.config.account_name, self.config.container, endpoint
        );
        let url = format!(
            "{}/{}?restype=container&comp=list&maxresults=1",
            endpoint, self.config.container
        );

        let mut headers = HeaderMap::new();
        let now = chrono::Utc::now()
            .format("%a, %d %b %Y %H:%M:%S GMT")
            .to_string();
        headers.insert(
            "x-ms-date",
            HeaderValue::from_str(&now)
                .map_err(|e| ProviderError::Other(format!("Invalid header value: {}", e)))?,
        );
        headers.insert("x-ms-version", HeaderValue::from_static(API_VERSION));

        // AZ-005: Use retry for connect test
        let resp = self
            .send_with_auth_and_retry(reqwest::Method::GET, &url, headers, 0, None)
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::AuthenticationFailed(format!(
                "Azure auth failed: {}",
                parse_azure_xml_error(&body)
            )));
        }

        self.connected = true;
        self.current_prefix = String::new();
        info!(
            "Connected to Azure Blob Storage: {}/{}",
            self.config.account_name, self.config.container
        );
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.connected = false;
        Ok(())
    }

    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let prefix = self.resolve_blob_path(path);
        // The prefix actually sent to Azure, trailing slash included. It is
        // also what makes the echoed full-path names relative, so the parser
        // gets this exact string and not `current_prefix` (which is unrelated
        // to `path` on any absolute or one-shot listing).
        let listing_prefix = if prefix.is_empty() || prefix.ends_with('/') {
            prefix.clone()
        } else {
            format!("{}/", prefix)
        };
        let prefix_param = if listing_prefix.is_empty() {
            String::new()
        } else {
            format!("&prefix={}", urlencoding::encode(&listing_prefix))
        };

        let base_url = format!(
            "{}/{}?restype=container&comp=list&delimiter=/{}",
            self.config.blob_endpoint(),
            self.config.container,
            prefix_param
        );

        let items = self
            .list_blobs_paginated(&base_url, &listing_prefix)
            .await?;

        let display_prefix = if prefix.is_empty() { "/" } else { &prefix };
        Ok(items
            .into_iter()
            .map(|item| {
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
                    permissions: None,
                    owner: None,
                    group: None,
                    is_symlink: false,
                    link_target: None,
                    mime_type: None,
                    metadata: Default::default(),
                }
            })
            .collect())
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
    async fn download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let blob_path = self.resolve_blob_path(remote_path);
        let url = self.blob_url(&blob_path);

        let mut headers = HeaderMap::new();
        let now = chrono::Utc::now()
            .format("%a, %d %b %Y %H:%M:%S GMT")
            .to_string();
        headers.insert(
            "x-ms-date",
            HeaderValue::from_str(&now)
                .map_err(|e| ProviderError::Other(format!("Invalid header value: {}", e)))?,
        );
        headers.insert("x-ms-version", HeaderValue::from_static(API_VERSION));

        let resp = self
            .send_with_auth_and_retry(reqwest::Method::GET, &url, headers, 0, None)
            .await?;

        if !resp.status().is_success() {
            return Err(ProviderError::TransferFailed(format!(
                "Download failed: {}",
                resp.status()
            )));
        }

        // AZ-003: Get total size for progress reporting
        let total_size = resp
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);

        // H-01: Streaming download: chunked writes instead of buffering entire response
        let mut stream = resp.bytes_stream();
        let mut atomic = super::atomic_write::AtomicFile::new(local_path)
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        let mut bytes_received: u64 = 0;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
            atomic
                .write_all(&chunk)
                .await
                .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

            // AZ-003: Report download progress
            bytes_received += chunk.len() as u64;
            if let Some(ref cb) = progress {
                cb(bytes_received, total_size);
            }
        }
        atomic.commit().await.map_err(|e| {
            ProviderError::TransferFailed(format!("Failed to finalize download: {}", e))
        })?;

        Ok(())
    }

    fn supports_resume(&self) -> bool {
        true
    }

    async fn begin_multipart_upload(
        &mut self,
        remote_path: &str,
        _total_size: u64,
        _content_type: Option<&str>,
        _local_source_path: Option<&str>,
    ) -> Result<MultipartHandle, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let blob_path = self.resolve_blob_path(remote_path);
        Ok(MultipartHandle {
            upload_id: uuid::Uuid::new_v4().to_string(),
            remote_path: blob_path,
        })
    }

    async fn upload_part(
        &mut self,
        handle: &MultipartHandle,
        part_number: u32,
        data: Vec<u8>,
    ) -> Result<UploadedPart, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let block_id = Self::dag_block_id(part_number)?;
        let blob_url = self.blob_url(&handle.remote_path);
        self.put_block(&blob_url, &block_id, data).await?;
        Ok(UploadedPart {
            part_number,
            etag: block_id,
        })
    }

    async fn complete_multipart_upload(
        &mut self,
        handle: MultipartHandle,
        parts: Vec<UploadedPart>,
    ) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let mut sorted = parts;
        sorted.sort_by_key(|part| part.part_number);
        let block_ids: Vec<String> = sorted.into_iter().map(|part| part.etag).collect();
        let blob_url = self.blob_url(&handle.remote_path);
        self.put_block_list(&blob_url, &block_ids).await
    }

    async fn abort_multipart_upload(
        &mut self,
        _handle: MultipartHandle,
    ) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        // Azure uncommitted blocks expire automatically. There is no explicit
        // abort request for Put Block sessions.
        Ok(())
    }

    fn transfer_optimization_hints(&self) -> super::TransferOptimizationHints {
        super::TransferOptimizationHints {
            supports_multipart: true,
            multipart_threshold: DAG_MULTIPART_THRESHOLD,
            multipart_part_size: DAG_MULTIPART_BLOCK_SIZE,
            multipart_max_parallel: DAG_MULTIPART_MAX_PARALLEL,
            supports_range_download: true,
            supports_resume_download: true,
            ..Default::default()
        }
    }

    fn transfer_executor_kind(&self) -> ProviderTransferExecutorKind {
        ProviderTransferExecutorKind::HttpClonePool
    }

    fn transfer_executor_max_sessions(&self) -> u16 {
        8
    }

    fn clone_for_transfer(&self) -> Result<Box<dyn StorageProvider>, ProviderError> {
        Ok(Box::new(self.clone()))
    }

    fn list_executor_kind(&self) -> super::ProviderListExecutorKind {
        super::ProviderListExecutorKind::HttpClonePool
    }

    fn list_executor_max_sessions(&self) -> u16 {
        8
    }

    fn clone_for_list(&self) -> Result<Box<dyn StorageProvider>, ProviderError> {
        Ok(Box::new(self.clone()))
    }

    async fn resume_download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        offset: u64,
        progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let blob_path = self.resolve_blob_path(remote_path);
        let url = self.blob_url(&blob_path);

        let mut headers = HeaderMap::new();
        let now = chrono::Utc::now()
            .format("%a, %d %b %Y %H:%M:%S GMT")
            .to_string();
        headers.insert(
            "x-ms-date",
            HeaderValue::from_str(&now)
                .map_err(|e| ProviderError::Other(format!("Invalid header value: {}", e)))?,
        );
        headers.insert("x-ms-version", HeaderValue::from_static(API_VERSION));
        headers.insert(
            "Range",
            HeaderValue::from_str(&format!("bytes={}-", offset))
                .map_err(|e| ProviderError::Other(format!("Invalid range header: {}", e)))?,
        );

        let resp = self
            .send_with_auth_and_retry(reqwest::Method::GET, &url, headers, 0, None)
            .await?;

        match resp.status() {
            reqwest::StatusCode::PARTIAL_CONTENT => {
                let content_len = resp.content_length().unwrap_or(0);
                let total_size = offset + content_len;
                let mut resumable = super::atomic_write::ResumableFile::open(local_path)
                    .await
                    .map_err(ProviderError::IoError)?;
                super::stream_response_to_resumable(resp, &mut resumable, total_size, progress)
                    .await?;
                resumable.commit().await.map_err(|e| {
                    ProviderError::TransferFailed(format!("Failed to finalize download: {}", e))
                })?;
                Ok(())
            }
            reqwest::StatusCode::OK => {
                // Server ignored Range: restart from scratch
                let total_size = resp.content_length().unwrap_or(0);
                let mut fresh = super::atomic_write::ResumableFile::open_fresh(local_path)
                    .await
                    .map_err(ProviderError::IoError)?;
                super::stream_response_to_resumable(resp, &mut fresh, total_size, progress).await?;
                fresh.commit().await.map_err(|e| {
                    ProviderError::TransferFailed(format!("Failed to finalize download: {}", e))
                })?;
                Ok(())
            }
            reqwest::StatusCode::RANGE_NOT_SATISFIABLE => {
                let tmp = format!("{}.aerotmp", local_path);
                let _ = tokio::fs::remove_file(&tmp).await;
                Err(ProviderError::TransferFailed(
                    "Range not satisfiable: file may have changed on server".to_string(),
                ))
            }
            status => Err(ProviderError::TransferFailed(format!(
                "Resume download failed: {}",
                status
            ))),
        }
    }

    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        let blob_path = self.resolve_blob_path(remote_path);
        let url = self.blob_url(&blob_path);

        let mut headers = HeaderMap::new();
        let now = chrono::Utc::now()
            .format("%a, %d %b %Y %H:%M:%S GMT")
            .to_string();
        headers.insert(
            "x-ms-date",
            HeaderValue::from_str(&now)
                .map_err(|e| ProviderError::Other(format!("Invalid header value: {}", e)))?,
        );
        headers.insert("x-ms-version", HeaderValue::from_static(API_VERSION));

        // AZ-005: Use retry
        let resp = self
            .send_with_auth_and_retry(reqwest::Method::GET, &url, headers, 0, None)
            .await?;

        if !resp.status().is_success() {
            return Err(ProviderError::TransferFailed(format!(
                "Download failed: {}",
                resp.status()
            )));
        }

        // H2: Size-limited download to prevent OOM on large files
        super::response_bytes_with_limit(resp, super::MAX_DOWNLOAD_TO_BYTES).await
    }

    /// Ranged read for remote archive/encryption surfacing (header/tail windows).
    /// Azure Blob honours the range via the signed `x-ms-range` header: because
    /// `build_canonical_headers` folds every `x-ms-*` header into the SharedKey
    /// signature, no separate signing step is needed. Falls back to a local slice
    /// if the service ever returns the whole blob (200 OK) instead of 206.
    async fn read_range(
        &mut self,
        remote_path: &str,
        offset: u64,
        len: u64,
    ) -> Result<Vec<u8>, ProviderError> {
        if len == 0 {
            return Ok(Vec::new());
        }
        let blob_path = self.resolve_blob_path(remote_path);
        let url = self.blob_url(&blob_path);

        let mut headers = HeaderMap::new();
        let now = chrono::Utc::now()
            .format("%a, %d %b %Y %H:%M:%S GMT")
            .to_string();
        headers.insert(
            "x-ms-date",
            HeaderValue::from_str(&now)
                .map_err(|e| ProviderError::Other(format!("Invalid header value: {}", e)))?,
        );
        headers.insert("x-ms-version", HeaderValue::from_static(API_VERSION));
        // offset + len - 1 must not wrap: a wrapped end makes an invalid Range
        // header that servers ignore, turning a bounded probe into a full-body
        // download. (len == 0 already returned above.)
        let end = offset
            .checked_add(len - 1)
            .ok_or_else(|| ProviderError::Other("read_range end overflows u64".to_string()))?;
        headers.insert(
            "x-ms-range",
            HeaderValue::from_str(&format!("bytes={}-{}", offset, end))
                .map_err(|e| ProviderError::Other(format!("Invalid range header: {}", e)))?,
        );

        let resp = self
            .send_with_auth_and_retry(reqwest::Method::GET, &url, headers, 0, None)
            .await?;

        let status = resp.status();
        if !status.is_success() {
            return Err(ProviderError::TransferFailed(format!(
                "Range download failed: {}",
                status
            )));
        }
        let bytes = super::response_bytes_with_limit(resp, super::MAX_DOWNLOAD_TO_BYTES).await?;
        if status == reqwest::StatusCode::OK {
            // Server ignored the range and returned the full blob: slice locally.
            if offset >= bytes.len() as u64 {
                Ok(Vec::new())
            } else {
                let start = offset as usize;
                let stop = std::cmp::min(start.saturating_add(len as usize), bytes.len());
                Ok(bytes[start..stop].to_vec())
            }
        } else {
            Ok(bytes)
        }
    }

    /// AZ-001: Upload with block upload support for files >100MB.
    /// AZ-003: Reports upload progress.
    /// - Files <= 100MB: Single Put Blob (streaming)
    /// - Files > 100MB: Put Block (4MB chunks) + Put Block List
    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let blob_path = self.resolve_blob_path(remote_path);
        let url = self.blob_url(&blob_path);

        let file_meta = tokio::fs::metadata(local_path)
            .await
            .map_err(ProviderError::IoError)?;
        let file_len = file_meta.len();

        // KE-B4.4: when --azure-archive-tier-delete is active, pre-delete an
        // existing blob in Archive tier so the subsequent PUT does not fail
        // with `BlobArchived`. Best-effort: any non-Archive response (200
        // with another tier, 404, or unexpected) lets the upload proceed.
        if self.archive_tier_delete {
            if let Err(e) = self.pre_upload_archive_purge(&blob_path).await {
                warn!(
                    "azure --azure-archive-tier-delete: pre-upload purge failed for {}: {}",
                    blob_path, e
                );
            }
        }

        let upload_result = if file_len > BLOCK_UPLOAD_THRESHOLD {
            // AZ-001: Block upload for large files
            self.upload_blocks(local_path, &url, file_len, progress)
                .await
        } else {
            // Small file: single Put Blob with streaming body
            self.upload_single(local_path, &url, file_len, progress)
                .await
        };

        // KE-B4.3: post-upload `Set Blob Tier`. Best-effort: a tier failure
        // does not invalidate the upload itself, but we surface it via the
        // returned error so callers can react if they need atomic tier
        // semantics.
        if upload_result.is_ok() {
            if let Some(tier) = self.effective_access_tier().map(str::to_string) {
                if let Err(e) = self.set_blob_tier(&blob_path, &tier).await {
                    warn!(
                        "azure --azure-access-tier {}: post-upload Set Blob Tier failed for {}: {}",
                        tier, blob_path, e
                    );
                }
            }
        }

        upload_result
    }

    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        // Azure Blob Storage doesn't have real directories.
        // Create a zero-byte marker blob with trailing "/" to preserve empty directories
        // (same pattern as S3). The marker is visible in listing but ignored by most tools.
        let blob_path = format!("{}/", self.resolve_blob_path(path).trim_end_matches('/'));
        let url = self.blob_url(&blob_path);

        let mut headers = HeaderMap::new();
        let now = chrono::Utc::now()
            .format("%a, %d %b %Y %H:%M:%S GMT")
            .to_string();
        headers.insert(
            "x-ms-date",
            HeaderValue::from_str(&now)
                .map_err(|e| ProviderError::Other(format!("Invalid header value: {}", e)))?,
        );
        headers.insert("x-ms-version", HeaderValue::from_static(API_VERSION));
        headers.insert("x-ms-blob-type", HeaderValue::from_static("BlockBlob"));
        headers.insert(CONTENT_LENGTH, HeaderValue::from(0u64));

        let resp = self
            .send_with_auth_and_retry(reqwest::Method::PUT, &url, headers, 0, Some(Vec::new()))
            .await?;

        let status = resp.status();
        if status.is_success() || status.as_u16() == 409 {
            // 409 Conflict = already exists
            Ok(())
        } else {
            Err(ProviderError::ServerError(format!(
                "mkdir marker failed: {}",
                status
            )))
        }
    }

    /// AZ-012: Delete with lease conflict detection.
    /// If delete fails with HTTP 412 (Precondition Failed), returns a clear error
    /// indicating a lease conflict. Full lease management (acquire/break/release)
    /// is not implemented as it is rarely needed for file manager use cases.
    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        let blob_path = self.resolve_blob_path(path);
        let url = self.blob_url(&blob_path);

        let mut headers = HeaderMap::new();
        let now = chrono::Utc::now()
            .format("%a, %d %b %Y %H:%M:%S GMT")
            .to_string();
        headers.insert(
            "x-ms-date",
            HeaderValue::from_str(&now)
                .map_err(|e| ProviderError::Other(format!("Invalid header value: {}", e)))?,
        );
        headers.insert("x-ms-version", HeaderValue::from_static(API_VERSION));

        // AZ-005: Use retry
        let resp = self
            .send_with_auth_and_retry(reqwest::Method::DELETE, &url, headers, 0, None)
            .await?;

        let status = resp.status();
        if !status.is_success() && status.as_u16() != 202 {
            // AZ-012: Detect lease conflict (HTTP 412 Precondition Failed)
            if status.as_u16() == 412 {
                return Err(ProviderError::Other(
                    "Delete failed: blob has an active lease. Break or release the lease first."
                        .to_string(),
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
        if path.trim_matches('/').is_empty() {
            return Err(ProviderError::InvalidPath(
                "Refusing to recursively delete root '/'. This would erase the entire container."
                    .into(),
            ));
        }
        let entries = self.list(path).await?;
        for entry in entries {
            if entry.is_dir {
                Box::pin(self.rmdir_recursive(&entry.path)).await?;
            } else {
                self.delete(&entry.path).await?;
            }
        }

        // `mkdir` writes a zero-byte marker blob named `<path>/` so an empty
        // folder survives a listing. That marker is invisible to `list`: with
        // prefix `<path>/` it comes back as a blob whose name IS the prefix, so
        // stripping the prefix leaves an empty string and the entry is dropped.
        // The loop above therefore never deleted it, and `rmdir` reported
        // "Removed empty directory" while the folder was still there on the
        // next listing. Delete it explicitly; a folder that only ever held
        // files has no marker, so a 404 here is the normal case and must not
        // fail the operation.
        let marker = format!("{}/", self.resolve_blob_path(path).trim_end_matches('/'));
        let url = self.blob_url(&marker);
        let mut headers = HeaderMap::new();
        let now = chrono::Utc::now()
            .format("%a, %d %b %Y %H:%M:%S GMT")
            .to_string();
        headers.insert(
            "x-ms-date",
            HeaderValue::from_str(&now)
                .map_err(|e| ProviderError::Other(format!("Invalid header value: {}", e)))?,
        );
        headers.insert("x-ms-version", HeaderValue::from_static(API_VERSION));
        let resp = self
            .send_with_auth_and_retry(reqwest::Method::DELETE, &url, headers, 0, None)
            .await?;
        let status = resp.status();
        if !status.is_success()
            && status.as_u16() != 202
            && status != reqwest::StatusCode::NOT_FOUND
        {
            return Err(ProviderError::Other(format!(
                "Delete of the directory marker failed: {}",
                status
            )));
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
        let now = chrono::Utc::now()
            .format("%a, %d %b %Y %H:%M:%S GMT")
            .to_string();
        headers.insert(
            "x-ms-date",
            HeaderValue::from_str(&now)
                .map_err(|e| ProviderError::Other(format!("Invalid header value: {}", e)))?,
        );
        headers.insert("x-ms-version", HeaderValue::from_static(API_VERSION));
        headers.insert(
            "x-ms-copy-source",
            HeaderValue::from_str(&source_url)
                .map_err(|e| ProviderError::Other(format!("Invalid header value: {}", e)))?,
        );
        // Azure requires explicit Content-Length: 0 for PUT Copy Blob
        headers.insert(CONTENT_LENGTH, HeaderValue::from_static("0"));

        // AZ-005: Use retry for copy request
        let resp = self
            .send_with_auth_and_retry(reqwest::Method::PUT, &dest_url, headers, 0, None)
            .await?;

        if !resp.status().is_success() {
            return Err(ProviderError::Other(format!(
                "Copy failed: {}",
                resp.status()
            )));
        }

        // AZ-016: Check copy status: may be async for large blobs
        let copy_status = resp
            .headers()
            .get("x-ms-copy-status")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("success")
            .to_lowercase();

        if copy_status == "pending" {
            debug!("Azure copy is async (pending), polling for completion");
            self.poll_copy_status(&dest_url).await?;
        } else if copy_status == "failed" {
            let desc = resp
                .headers()
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
        let now = chrono::Utc::now()
            .format("%a, %d %b %Y %H:%M:%S GMT")
            .to_string();
        headers.insert(
            "x-ms-date",
            HeaderValue::from_str(&now)
                .map_err(|e| ProviderError::Other(format!("Invalid header value: {}", e)))?,
        );
        headers.insert("x-ms-version", HeaderValue::from_static(API_VERSION));

        // AZ-005: Use retry
        let resp = self
            .send_with_auth_and_retry(reqwest::Method::HEAD, &url, headers, 0, None)
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            // Only a genuine absence (404/410) is NotFound. The prior blanket
            // mapping turned a 403/500/503 on an EXISTING blob into a false
            // "not found", so `exists()` would wrongly report Ok(false) and a
            // real auth/server fault would be silently swallowed.
            if status.as_u16() == 404 || status.as_u16() == 410 {
                return Err(ProviderError::NotFound(path.to_string()));
            }
            return Err(ProviderError::ServerError(format!(
                "Azure stat failed ({}): {}",
                status, path
            )));
        }

        let size = resp
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        let modified = resp
            .headers()
            .get("Last-Modified")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        // AZ-007: Extract Content-Type for mime_type
        let mime_type = resp
            .headers()
            .get(CONTENT_TYPE)
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
            permissions: None,
            owner: None,
            group: None,
            is_symlink: false,
            link_target: None,
            mime_type,
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

    async fn keep_alive(&mut self) -> Result<(), ProviderError> {
        Ok(())
    }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        Ok(format!(
            "Azure Blob Storage: {}/{}",
            self.config.account_name, self.config.container
        ))
    }

    fn supports_server_copy(&self) -> bool {
        true
    }

    fn supports_server_side_copy(&self) -> bool {
        true
    }

    async fn server_copy(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        // Legacy alias kept so CLI / MCP / provider_commands callers keep
        // working. The real Copy Blob implementation lives on
        // `server_side_copy` (S3-T10 migration, v4.0.0).
        StorageProvider::server_side_copy(self, from, to).await
    }

    async fn server_side_copy(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let from_path = self.resolve_blob_path(from);
        let to_path = self.resolve_blob_path(to);

        let source_url = self.blob_url(&from_path);
        let dest_url = self.blob_url(&to_path);

        let now = chrono::Utc::now();
        let date_str = now.format("%a, %d %b %Y %H:%M:%S GMT").to_string();

        let mut headers = HeaderMap::new();
        headers.insert("x-ms-date", HeaderValue::from_str(&date_str).unwrap());
        headers.insert("x-ms-version", HeaderValue::from_static(API_VERSION));
        // x-ms-copy-source points to the source blob URL
        headers.insert(
            "x-ms-copy-source",
            HeaderValue::from_str(&source_url)
                .map_err(|e| ProviderError::Other(format!("Invalid source URL: {}", e)))?,
        );
        // Azure requires Content-Length: 0 for Copy Blob
        headers.insert(CONTENT_LENGTH, HeaderValue::from_static("0"));

        let resp = self
            .send_with_auth_and_retry(reqwest::Method::PUT, &dest_url, headers, 0, None)
            .await?;

        let status = resp.status();
        if status.is_success() || status.as_u16() == 202 {
            Ok(())
        } else {
            let body = resp.text().await.unwrap_or_default();
            Err(ProviderError::ServerError(format!(
                "Copy Blob failed ({}): {}",
                status,
                sanitize_api_error(&body)
            )))
        }
    }

    fn supports_share_links(&self) -> bool {
        true
    }

    fn share_link_capabilities(&self) -> ShareLinkCapabilities {
        ShareLinkCapabilities {
            supports_expiration: true,
            supports_password: false,
            supports_permissions: false,
            available_permissions: vec![],
            ..Default::default()
        }
    }

    async fn create_share_link(
        &mut self,
        path: &str,
        options: ShareLinkOptions,
    ) -> Result<ShareLinkResult, ProviderError> {
        let blob_path = path.trim_start_matches('/');
        let expiry_secs = options.expires_in_secs.unwrap_or(7 * 24 * 3600); // default 7 days
        let now = chrono::Utc::now();
        let start = (now - chrono::Duration::minutes(5))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        let expiry = (now + chrono::Duration::seconds(expiry_secs as i64))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();

        // Service SAS for a specific blob
        let signed_permissions = "r"; // read only
        let signed_start = &start;
        let signed_expiry = &expiry;
        let canonicalized_resource = format!(
            "/blob/{}/{}/{}",
            self.config.account_name, self.config.container, blob_path
        );
        let signed_version = API_VERSION;
        let signed_protocol = "https";

        // StringToSign for Service SAS (Blob), API version 2024-11-04. The field
        // order after canonicalizedResource is fixed by the Azure Service SAS
        // spec: signedIdentifier, signedIP, signedProtocol, signedVersion,
        // signedResource, signedSnapshotTime, signedEncryptionScope, then the
        // five response-header overrides (rscc/rscd/rsce/rscl/rsct). We set
        // signedResource = "b" and leave the rest empty; the emitted token below
        // MUST declare exactly the same set (sp, st, se, spr, sv, sr=b, no sip,
        // no si), or the server recomputes a different HMAC and returns 403.
        let string_to_sign = format!(
            "{}\n{}\n{}\n{}\n\n\n{}\n{}\nb\n\n\n\n\n\n\n",
            signed_permissions,
            signed_start,
            signed_expiry,
            canonicalized_resource,
            signed_protocol,
            signed_version,
        );

        let key_bytes = BASE64
            .decode(self.config.access_key.expose_secret())
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
        Ok(ShareLinkResult {
            url: share_url,
            password: None,
            expires_at: Some(expiry),
        })
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
        let file = tokio::fs::File::open(local_path)
            .await
            .map_err(ProviderError::IoError)?;
        let stream = tokio_util::io::ReaderStream::new(file);
        let body = reqwest::Body::wrap_stream(stream);

        let mut headers = HeaderMap::new();
        let now = chrono::Utc::now()
            .format("%a, %d %b %Y %H:%M:%S GMT")
            .to_string();
        headers.insert(
            "x-ms-date",
            HeaderValue::from_str(&now)
                .map_err(|e| ProviderError::Other(format!("Invalid header value: {}", e)))?,
        );
        headers.insert("x-ms-version", HeaderValue::from_static(API_VERSION));
        headers.insert("x-ms-blob-type", HeaderValue::from_static("BlockBlob"));
        headers.insert(CONTENT_LENGTH, HeaderValue::from(file_len));

        let auth = self.sign_request("PUT", url, &headers, file_len)?;

        // Cannot use send_with_auth_and_retry for streaming body (body is not cloneable).
        // Streaming uploads are not retryable at this level: the caller can retry the entire upload.
        let resp = if self.config.sas_token.is_some() {
            self.client
                .put(&auth)
                .headers(headers)
                .body(body)
                .send()
                .await
        } else {
            headers.insert(
                "Authorization",
                HeaderValue::from_str(&auth)
                    .map_err(|e| ProviderError::Other(format!("Invalid header value: {}", e)))?,
            );
            self.client
                .put(url)
                .headers(headers)
                .body(body)
                .send()
                .await
        }
        .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let retry_header = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .map(String::from);
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::TransferFailed(format_azure_error(
                "Upload failed",
                status,
                &body,
                retry_header.as_deref(),
            )));
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
    ///
    /// KE-B4.1: Block uploads can be parallelised through
    /// `set_upload_concurrency`. With the default `concurrency=1` the
    /// historical strictly-sequential path is preserved. When the user
    /// raises the knob, each batch of `concurrency` blocks is pre-read
    /// from disk and dispatched as parallel `Put Block` requests via
    /// `tokio::task::JoinSet`. The block list is reassembled in
    /// monotonic index order so `Put Block List` sees the expected
    /// sequence regardless of completion order.
    async fn upload_blocks(
        &self,
        local_path: &str,
        blob_url: &str,
        file_len: u64,
        progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let concurrency = self.effective_upload_concurrency().max(1);
        let mut file = tokio::fs::File::open(local_path)
            .await
            .map_err(ProviderError::IoError)?;

        let mut block_ids: Vec<String> = Vec::new();
        let mut bytes_uploaded: u64 = 0;
        let mut block_index: u32 = 0;

        loop {
            // Pre-read up to `concurrency` blocks from disk before
            // dispatching them. Each batch entry is (index, id, payload).
            let mut batch: Vec<(u32, String, Vec<u8>)> = Vec::with_capacity(concurrency);
            for _ in 0..concurrency {
                let mut buf = vec![0u8; BLOCK_SIZE];
                let mut filled = 0;

                while filled < BLOCK_SIZE {
                    let n = file.read(&mut buf[filled..]).await.map_err(|e| {
                        ProviderError::TransferFailed(format!("File read error: {}", e))
                    })?;
                    if n == 0 {
                        break;
                    }
                    filled += n;
                }

                if filled == 0 {
                    break;
                }
                buf.truncate(filled);

                // Zero-padded 6-digit base64 keeps every block ID the same
                // length, a hard Azure requirement for Put Block List.
                let block_id_raw = format!("{:06}", block_index);
                let block_id = BASE64.encode(block_id_raw.as_bytes());
                batch.push((block_index, block_id, buf));
                block_index += 1;
            }

            if batch.is_empty() {
                break;
            }

            if concurrency == 1 {
                // Fast path: a single block, send inline so we don't pay
                // the JoinSet bookkeeping cost on legacy serial uploads.
                let (idx, id, data) = batch.into_iter().next().expect("batch non-empty");
                let data_len = data.len() as u64;
                self.put_block(blob_url, &id, data).await?;
                let _ = idx; // index is implicit in append order on serial path
                block_ids.push(id);
                bytes_uploaded += data_len;
                if let Some(ref cb) = progress {
                    cb(bytes_uploaded, file_len);
                }
            } else {
                let mut joinset = tokio::task::JoinSet::new();
                for (idx, id, data) in batch.into_iter() {
                    let provider = self.clone();
                    let url = blob_url.to_string();
                    let id_owned = id.clone();
                    let data_len = data.len() as u64;
                    joinset.spawn(async move {
                        provider.put_block(&url, &id_owned, data).await?;
                        Ok::<(u32, String, u64), ProviderError>((idx, id_owned, data_len))
                    });
                }
                // Collect results, reassemble in monotonic index order.
                let mut completed: Vec<(u32, String, u64)> = Vec::new();
                while let Some(joined) = joinset.join_next().await {
                    match joined {
                        Ok(Ok(tuple)) => completed.push(tuple),
                        Ok(Err(e)) => {
                            joinset.abort_all();
                            while joinset.join_next().await.is_some() {}
                            return Err(e);
                        }
                        Err(e) => {
                            joinset.abort_all();
                            while joinset.join_next().await.is_some() {}
                            return Err(ProviderError::TransferFailed(format!(
                                "Block upload task panicked: {e}"
                            )));
                        }
                    }
                }
                completed.sort_by_key(|(idx, _, _)| *idx);
                for (_, id, data_len) in completed.into_iter() {
                    block_ids.push(id);
                    bytes_uploaded += data_len;
                    if let Some(ref cb) = progress {
                        cb(bytes_uploaded, file_len);
                    }
                }
            }
        }

        if block_ids.is_empty() {
            return Err(ProviderError::TransferFailed(
                "No data read from file".to_string(),
            ));
        }

        // Commit all blocks
        self.put_block_list(blob_url, &block_ids).await?;

        // AZ-003: Final progress report
        if let Some(ref cb) = progress {
            cb(file_len, file_len);
        }

        debug!(
            "Block upload complete: {} blocks, {} bytes",
            block_ids.len(),
            file_len
        );
        Ok(())
    }

    // =========================================================================
    // Azure Enterprise Features (Blob Tier, Soft Delete)
    // =========================================================================

    /// KE-B4.4: HEAD the target blob; if it exists in `Archive` access
    /// tier, DELETE it so a subsequent PUT does not fail with
    /// `BlobArchived`. Returns `Ok(())` on success (blob purged) or when
    /// no purge was needed (blob missing, or not in Archive tier).
    /// Network or auth errors are propagated; the upload caller logs them
    /// at WARN and proceeds anyway because the eventual PUT will surface
    /// any real failure.
    async fn pre_upload_archive_purge(&self, blob_path: &str) -> Result<(), ProviderError> {
        let url = self.blob_url(blob_path);
        let mut head_headers = HeaderMap::new();
        let now = chrono::Utc::now()
            .format("%a, %d %b %Y %H:%M:%S GMT")
            .to_string();
        head_headers.insert(
            "x-ms-date",
            HeaderValue::from_str(&now)
                .map_err(|e| ProviderError::Other(format!("Invalid header value: {}", e)))?,
        );
        head_headers.insert("x-ms-version", HeaderValue::from_static(API_VERSION));

        let resp = self
            .send_with_auth_and_retry(reqwest::Method::HEAD, &url, head_headers, 0, None)
            .await?;

        let status = resp.status();
        if status.as_u16() == 404 {
            return Ok(());
        }
        if !status.is_success() {
            // Don't propagate non-404 HEAD failures: the upload PUT will
            // hit the same auth path and surface a more useful error.
            debug!(
                "azure pre_upload_archive_purge: HEAD {} returned {}; skipping",
                blob_path, status
            );
            return Ok(());
        }

        let tier = resp
            .headers()
            .get("x-ms-access-tier")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();

        if tier != "archive" {
            return Ok(());
        }

        // Existing blob is Archive: delete it so the PUT succeeds.
        let mut del_headers = HeaderMap::new();
        let now = chrono::Utc::now()
            .format("%a, %d %b %Y %H:%M:%S GMT")
            .to_string();
        del_headers.insert(
            "x-ms-date",
            HeaderValue::from_str(&now)
                .map_err(|e| ProviderError::Other(format!("Invalid header value: {}", e)))?,
        );
        del_headers.insert("x-ms-version", HeaderValue::from_static(API_VERSION));
        let del = self
            .send_with_auth_and_retry(reqwest::Method::DELETE, &url, del_headers, 0, None)
            .await?;
        if del.status().is_success() || del.status().as_u16() == 202 {
            info!(
                "azure --azure-archive-tier-delete: purged Archive blob {}",
                blob_path
            );
            Ok(())
        } else {
            let s = del.status();
            let body = del.text().await.unwrap_or_default();
            Err(ProviderError::Other(format!(
                "Archive purge DELETE failed ({}): {}",
                s,
                parse_azure_xml_error(&body)
            )))
        }
    }

    /// Set the access tier of a blob (Hot, Cool, Cold, Archive).
    /// For rehydration from Archive, set tier to Hot or Cool.
    pub async fn set_blob_tier(&self, blob_path: &str, tier: &str) -> Result<(), ProviderError> {
        let resolved = self.resolve_blob_path(blob_path);
        let url = format!("{}?comp=tier", self.blob_url(&resolved));

        let mut headers = HeaderMap::new();
        let now = chrono::Utc::now()
            .format("%a, %d %b %Y %H:%M:%S GMT")
            .to_string();
        headers.insert(
            "x-ms-date",
            HeaderValue::from_str(&now)
                .map_err(|e| ProviderError::Other(format!("Invalid header value: {}", e)))?,
        );
        headers.insert("x-ms-version", HeaderValue::from_static(API_VERSION));
        headers.insert(
            "x-ms-access-tier",
            tier.parse()
                .map_err(|_| ProviderError::InvalidConfig(format!("Invalid tier: {}", tier)))?,
        );

        let response = self
            .send_with_auth_and_retry(reqwest::Method::PUT, &url, headers, 0, None)
            .await?;

        match response.status() {
            reqwest::StatusCode::OK | reqwest::StatusCode::ACCEPTED => {
                tracing::info!("Set blob tier '{}' -> {}", resolved, tier);
                Ok(())
            }
            status => {
                let body = response.text().await.unwrap_or_default();
                Err(ProviderError::ServerError(format!(
                    "Set blob tier failed ({}): {}",
                    status,
                    parse_azure_xml_error(&body)
                )))
            }
        }
    }

    /// List soft-deleted blobs in the container.
    pub async fn list_deleted_blobs(&self) -> Result<Vec<super::RemoteEntry>, ProviderError> {
        let base_url = format!(
            "{}?restype=container&comp=list&include=deleted",
            self.blob_url("")
        );

        let mut all_entries = Vec::new();
        let mut marker: Option<String> = None;

        loop {
            let url = match &marker {
                Some(m) => format!("{}&marker={}", base_url, urlencoding::encode(m)),
                None => base_url.clone(),
            };

            let mut headers = HeaderMap::new();
            let now = chrono::Utc::now()
                .format("%a, %d %b %Y %H:%M:%S GMT")
                .to_string();
            headers.insert(
                "x-ms-date",
                HeaderValue::from_str(&now)
                    .map_err(|e| ProviderError::Other(format!("Invalid header value: {}", e)))?,
            );
            headers.insert("x-ms-version", HeaderValue::from_static(API_VERSION));

            let response = self
                .send_with_auth_and_retry(reqwest::Method::GET, &url, headers, 0, None)
                .await?;

            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            if !status.is_success() {
                return Err(ProviderError::ServerError(format!(
                    "List deleted blobs failed ({}): {}",
                    status,
                    parse_azure_xml_error(&body)
                )));
            }

            let mut next_marker: Option<String> = None;
            let mut reader = quick_xml::Reader::from_str(&body);
            // No trim_text: blob-name fragments around XML entities must
            // survive intact (entity-adjacent spaces are part of the name).
            reader.config_mut().trim_text(false);
            let mut buf = Vec::new();
            let mut in_blob = false;
            let mut blob_name = String::new();
            let mut blob_size: u64 = 0;
            let mut blob_modified: Option<String> = None;
            let mut is_deleted = false;
            let mut tag_name = String::new();

            loop {
                match reader.read_event_into(&mut buf) {
                    Ok(quick_xml::events::Event::Start(ref e)) => {
                        let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                        if name == "Blob" {
                            in_blob = true;
                            blob_name.clear();
                            blob_size = 0;
                            blob_modified = None;
                            is_deleted = false;
                        }
                        tag_name = name;
                    }
                    Ok(quick_xml::events::Event::Text(ref e)) => {
                        let text = String::from_utf8_lossy(e.as_ref()).into_owned();
                        // Skip indentation-only fragments, but preserve
                        // whitespace inside <Name>: there it is payload
                        // (e.g. `a&amp; &amp;b.txt`).
                        if text.trim().is_empty() && !(in_blob && tag_name == "Name") {
                            buf.clear();
                            continue;
                        }
                        if in_blob {
                            match tag_name.as_str() {
                                "Name" => blob_name.push_str(&text),
                                "Deleted" => is_deleted = text.trim() == "true",
                                "Content-Length" => {
                                    blob_size = text.trim().parse().unwrap_or(blob_size);
                                }
                                "Last-Modified" => {
                                    blob_modified
                                        .get_or_insert_with(String::new)
                                        .push_str(&text);
                                }
                                _ => {}
                            }
                        } else if tag_name == "NextMarker" {
                            next_marker.get_or_insert_with(String::new).push_str(&text);
                        }
                    }
                    Ok(quick_xml::events::Event::GeneralRef(ref e)) => {
                        if let Some(ch) = super::xml_text::xml_entity_to_str(e.as_ref()) {
                            if in_blob {
                                match tag_name.as_str() {
                                    "Name" => blob_name.push_str(&ch),
                                    "Last-Modified" => {
                                        blob_modified.get_or_insert_with(String::new).push_str(&ch);
                                    }
                                    _ => {}
                                }
                            } else if tag_name == "NextMarker" {
                                next_marker.get_or_insert_with(String::new).push_str(&ch);
                            }
                        }
                    }
                    Ok(quick_xml::events::Event::End(ref e))
                        if String::from_utf8_lossy(e.name().as_ref()) == "Blob" =>
                    {
                        if in_blob && is_deleted && !blob_name.is_empty() {
                            let mut meta = std::collections::HashMap::new();
                            meta.insert("deleted".to_string(), "true".to_string());
                            all_entries.push(super::RemoteEntry {
                                name: blob_name
                                    .rsplit('/')
                                    .next()
                                    .unwrap_or(&blob_name)
                                    .to_string(),
                                path: blob_name.clone(),
                                is_dir: false,
                                size: blob_size,
                                modified: blob_modified.as_ref().map(|m| m.trim().to_string()),
                                permissions: None,
                                owner: None,
                                group: None,
                                is_symlink: false,
                                link_target: None,
                                mime_type: None,
                                metadata: meta,
                            });
                        }
                        in_blob = false;
                    }
                    Ok(quick_xml::events::Event::Eof) => break,
                    Err(_) => break,
                    _ => {}
                }
                buf.clear();
            }

            if next_marker.is_some() {
                marker = next_marker.map(|m| m.trim().to_string());
            } else {
                break;
            }
        }

        Ok(all_entries)
    }

    /// Undelete a soft-deleted blob.
    pub async fn undelete_blob(&self, blob_path: &str) -> Result<(), ProviderError> {
        let resolved = self.resolve_blob_path(blob_path);
        let url = format!("{}?comp=undelete", self.blob_url(&resolved));

        let mut headers = HeaderMap::new();
        let now = chrono::Utc::now()
            .format("%a, %d %b %Y %H:%M:%S GMT")
            .to_string();
        headers.insert(
            "x-ms-date",
            HeaderValue::from_str(&now)
                .map_err(|e| ProviderError::Other(format!("Invalid header value: {}", e)))?,
        );
        headers.insert("x-ms-version", HeaderValue::from_static(API_VERSION));

        let response = self
            .send_with_auth_and_retry(reqwest::Method::PUT, &url, headers, 0, None)
            .await?;

        match response.status() {
            reqwest::StatusCode::OK => {
                tracing::info!("Undeleted blob '{}'", resolved);
                Ok(())
            }
            status => {
                let body = response.text().await.unwrap_or_default();
                Err(ProviderError::ServerError(format!(
                    "Undelete blob failed ({}): {}",
                    status,
                    parse_azure_xml_error(&body)
                )))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderValue};

    fn test_config() -> AzureConfig {
        AzureConfig {
            account_name: "myacc".to_string(),
            access_key: secrecy::SecretString::from("dGVzdGtleQ==".to_string()),
            container: "mycontainer".to_string(),
            sas_token: None,
            endpoint: None,
        }
    }

    #[test]
    fn blob_endpoint_defaults_to_azure_and_respects_custom() {
        let c = test_config();
        assert_eq!(c.blob_endpoint(), "https://myacc.blob.core.windows.net");

        let mut c2 = test_config();
        c2.endpoint = Some("blob.local:10000".to_string());
        assert_eq!(c2.blob_endpoint(), "https://blob.local:10000");

        let mut c3 = test_config();
        c3.endpoint = Some("http://azurite:10000".to_string());
        assert_eq!(c3.blob_endpoint(), "http://azurite:10000");
    }

    #[test]
    fn parse_azure_xml_error_extracts_code_and_first_message_line() {
        let xml = r#"<?xml version="1.0"?><Error>
            <Code>BlobNotFound</Code>
            <Message>The specified blob does not exist.
RequestId:abc-123
Time:2026-01-01</Message>
        </Error>"#;
        let formatted = parse_azure_xml_error(xml);
        assert!(formatted.starts_with("BlobNotFound:"));
        assert!(formatted.contains("specified blob"));
        assert!(!formatted.contains("RequestId"));
    }

    #[test]
    fn parse_azure_xml_error_falls_back_when_no_code_element() {
        // Plain body (not Azure XML): falls back to sanitize_api_error
        let out = parse_azure_xml_error("plain text error");
        assert!(!out.is_empty());
    }

    #[test]
    fn build_canonical_headers_lowercases_sorts_and_filters_xms() {
        let mut headers = HeaderMap::new();
        headers.insert("x-ms-version", HeaderValue::from_static("2024-11-04"));
        headers.insert("x-ms-date", HeaderValue::from_static("Mon, 01 Jan 2026"));
        headers.insert("x-ms-blob-type", HeaderValue::from_static("BlockBlob"));
        headers.insert("content-type", HeaderValue::from_static("text/plain"));
        headers.insert("authorization", HeaderValue::from_static("Bearer xyz"));

        let canonical = AzureProvider::build_canonical_headers(&headers);
        // Only x-ms-* headers, sorted alphabetically
        let lines: Vec<&str> = canonical.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("x-ms-blob-type:"));
        assert!(lines[1].starts_with("x-ms-date:"));
        assert!(lines[2].starts_with("x-ms-version:"));
        // No non-xms headers
        assert!(!canonical.contains("content-type"));
        assert!(!canonical.contains("authorization"));
    }

    #[test]
    fn build_canonical_headers_is_empty_when_no_xms_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("content-type", HeaderValue::from_static("text/plain"));
        assert_eq!(AzureProvider::build_canonical_headers(&headers), "");
    }

    #[test]
    fn blob_url_prefixes_container_and_endpoint() {
        let p = AzureProvider::new(test_config());
        assert_eq!(
            p.blob_url("folder/file.txt"),
            "https://myacc.blob.core.windows.net/mycontainer/folder/file.txt"
        );
        assert_eq!(
            p.blob_url(""),
            "https://myacc.blob.core.windows.net/mycontainer"
        );
        assert_eq!(
            p.blob_url("/leading-slash/file"),
            "https://myacc.blob.core.windows.net/mycontainer/leading-slash/file"
        );
    }

    #[test]
    fn resolve_blob_path_joins_relative_against_current_prefix() {
        let mut p = AzureProvider::new(test_config());
        p.current_prefix = "project/".to_string();

        assert_eq!(p.resolve_blob_path("."), "project/");
        assert_eq!(p.resolve_blob_path(""), "project/");
        // absolute paths strip leading slashes and bypass current_prefix
        assert_eq!(p.resolve_blob_path("/other/file"), "other/file");
        // relative joins against current_prefix
        assert_eq!(p.resolve_blob_path("sub/file.txt"), "project/sub/file.txt");

        // empty current_prefix
        let p2 = AzureProvider::new(test_config());
        assert_eq!(p2.resolve_blob_path("child"), "child");
    }

    #[test]
    fn dag_block_id_is_fixed_width_and_rejects_zero() {
        let first = AzureProvider::dag_block_id(1).unwrap();
        let later = AzureProvider::dag_block_id(42).unwrap();

        assert_eq!(first.len(), later.len());
        assert_eq!(
            String::from_utf8(BASE64.decode(first.as_bytes()).unwrap()).unwrap(),
            "00000000000000000001"
        );
        assert!(matches!(
            AzureProvider::dag_block_id(0),
            Err(ProviderError::InvalidConfig(_))
        ));
    }

    #[test]
    fn transfer_hints_advertise_dag_multipart_for_azure() {
        let p = AzureProvider::new(test_config());
        let hints = p.transfer_optimization_hints();

        assert!(hints.supports_multipart);
        // Fan-out only kicks in at the DAG threshold (200 MiB); below it Azure
        // stays single-shot (audit Patch Set 2).
        assert_eq!(hints.multipart_threshold, DAG_MULTIPART_THRESHOLD);
        assert_eq!(hints.multipart_part_size, DAG_MULTIPART_BLOCK_SIZE);
        assert_eq!(hints.multipart_max_parallel, DAG_MULTIPART_MAX_PARALLEL);
        assert!(hints.supports_range_download);
        assert!(hints.supports_resume_download);
    }

    #[tokio::test]
    async fn begin_multipart_upload_resolves_path_for_handle() {
        let mut p = AzureProvider::new(test_config());
        p.connected = true;
        p.current_prefix = "project/".to_string();

        let handle = StorageProvider::begin_multipart_upload(&mut p, "asset.bin", 10, None, None)
            .await
            .unwrap();

        assert_eq!(handle.remote_path, "project/asset.bin");
        assert!(!handle.upload_id.is_empty());
    }

    // ---------------------------------------------------------------------
    // KE-E2: Azure Retry-After detection (Sprint K1)
    // ---------------------------------------------------------------------

    #[test]
    fn azure_is_rate_limited_recognises_429() {
        assert!(azure_is_rate_limited(429, ""));
        assert!(azure_is_rate_limited(429, "any body"));
    }

    #[test]
    fn azure_is_rate_limited_recognises_503_server_busy() {
        let body = r#"<?xml version="1.0" encoding="utf-8"?><Error><Code>ServerBusy</Code><Message>The server is busy.</Message></Error>"#;
        assert!(azure_is_rate_limited(503, body));
    }

    #[test]
    fn azure_is_rate_limited_rejects_503_other_codes() {
        // InternalError / OperationTimedOut are transient, not throttle signals.
        let internal = r#"<Error><Code>InternalError</Code></Error>"#;
        let timeout = r#"<Error><Code>OperationTimedOut</Code></Error>"#;
        assert!(!azure_is_rate_limited(503, internal));
        assert!(!azure_is_rate_limited(503, timeout));
        assert!(!azure_is_rate_limited(503, ""));
    }

    #[test]
    fn azure_is_rate_limited_rejects_non_throttle_status() {
        assert!(!azure_is_rate_limited(500, "<Code>ServerBusy</Code>"));
        assert!(!azure_is_rate_limited(404, ""));
        assert!(!azure_is_rate_limited(200, "<Code>ServerBusy</Code>"));
    }

    #[test]
    fn azure_retry_marker_tail_emits_marker_on_429_with_header() {
        let tail = azure_retry_marker_tail(429, "", Some("8")).expect("rate-limited + hint");
        assert!(tail.contains("retry-after-secs=8"));
    }

    #[test]
    fn azure_retry_marker_tail_emits_marker_on_503_server_busy_with_header() {
        let body = r#"<Error><Code>ServerBusy</Code></Error>"#;
        let tail = azure_retry_marker_tail(503, body, Some("20")).expect("ServerBusy + hint");
        assert!(tail.contains("retry-after-secs=20"));
    }

    #[test]
    fn azure_retry_marker_tail_returns_none_without_header() {
        assert_eq!(azure_retry_marker_tail(429, "", None), None);
        assert_eq!(azure_retry_marker_tail(429, "", Some("")), None);
        assert_eq!(azure_retry_marker_tail(429, "", Some("abc")), None);
    }

    #[test]
    fn azure_retry_marker_tail_returns_none_when_not_rate_limited() {
        assert_eq!(azure_retry_marker_tail(500, "boom", Some("30")), None);
        assert_eq!(
            azure_retry_marker_tail(503, r#"<Code>InternalError</Code>"#, Some("30")),
            None
        );
    }

    #[test]
    fn format_azure_error_appends_marker_on_throttle() {
        let body = r#"<Error><Code>ServerBusy</Code><Message>Slow down</Message></Error>"#;
        let msg = format_azure_error(
            "Put Block failed",
            reqwest::StatusCode::SERVICE_UNAVAILABLE,
            body,
            Some("12"),
        );
        assert!(msg.contains("Put Block failed"));
        assert!(msg.contains("ServerBusy"));
        assert!(msg.contains("retry-after-secs=12"));
    }

    #[test]
    fn format_azure_error_omits_marker_on_non_throttle() {
        let body = r#"<Error><Code>BlobNotFound</Code></Error>"#;
        let msg = format_azure_error(
            "Get Blob failed",
            reqwest::StatusCode::NOT_FOUND,
            body,
            Some("12"),
        );
        assert!(msg.contains("BlobNotFound"));
        assert!(!msg.contains("retry-after-secs"));
    }

    // ── KE-B4 per-backend Azure knob tests ─────────────────────────────

    fn test_provider() -> AzureProvider {
        AzureProvider::new(test_config())
    }

    /// KE-B4.1: default concurrency is 1 (sequential, historical) and
    /// `set_upload_concurrency` clamps to `[1, MAX]`. `0` resets to default.
    #[test]
    fn set_upload_concurrency_clamps_and_resets() {
        let mut p = test_provider();
        assert_eq!(
            p.effective_upload_concurrency(),
            AzureProvider::UPLOAD_CONCURRENCY_DEFAULT
        );

        p.set_upload_concurrency(4);
        assert_eq!(p.effective_upload_concurrency(), 4);

        p.set_upload_concurrency(999);
        assert_eq!(
            p.effective_upload_concurrency(),
            AzureProvider::UPLOAD_CONCURRENCY_MAX
        );

        p.set_upload_concurrency(0);
        assert_eq!(
            p.effective_upload_concurrency(),
            AzureProvider::UPLOAD_CONCURRENCY_DEFAULT
        );
    }

    /// KE-B4.2: `set_disable_checksum` toggles the flag. Structurally
    /// wired but no observable upload effect today.
    #[test]
    fn set_disable_checksum_toggles_flag() {
        let mut p = test_provider();
        assert!(!p.disable_checksum);
        p.set_disable_checksum(true);
        assert!(p.disable_checksum);
        p.set_disable_checksum(false);
        assert!(!p.disable_checksum);
    }

    /// KE-B4.3: `set_access_tier` accepts non-empty strings and stores
    /// them; whitespace-only normalises to None. `effective_access_tier`
    /// returns None when no override is set.
    #[test]
    fn set_access_tier_stores_and_clears() {
        let mut p = test_provider();
        assert!(p.effective_access_tier().is_none());

        p.set_access_tier(Some("Cool".to_string()));
        assert_eq!(p.effective_access_tier(), Some("Cool"));

        p.set_access_tier(Some("Archive".to_string()));
        assert_eq!(p.effective_access_tier(), Some("Archive"));

        // Whitespace-only normalises to None
        p.set_access_tier(Some("   ".to_string()));
        assert!(p.effective_access_tier().is_none());

        // Vendor / future tier passes through
        p.set_access_tier(Some("Premium".to_string()));
        assert_eq!(p.effective_access_tier(), Some("Premium"));

        p.set_access_tier(None);
        assert!(p.effective_access_tier().is_none());
    }

    /// KE-B4.4: `set_archive_tier_delete` toggles the flag.
    #[test]
    fn set_archive_tier_delete_toggles_flag() {
        let mut p = test_provider();
        assert!(!p.archive_tier_delete);
        p.set_archive_tier_delete(true);
        assert!(p.archive_tier_delete);
        p.set_archive_tier_delete(false);
        assert!(!p.archive_tier_delete);
    }

    /// KE-B4: Clone preserves all the runtime knob state. Concurrency
    /// override survives clone because `upload_blocks` spawns clones of
    /// `self` into the JoinSet workers and each must see the same knob.
    #[test]
    fn clone_preserves_runtime_knobs() {
        let mut p = test_provider();
        p.set_upload_concurrency(8);
        p.set_disable_checksum(true);
        p.set_access_tier(Some("Cool".to_string()));
        p.set_archive_tier_delete(true);

        let q = p.clone();
        assert_eq!(q.effective_upload_concurrency(), 8);
        assert!(q.disable_checksum);
        assert_eq!(q.effective_access_tier(), Some("Cool"));
        assert!(q.archive_tier_delete);
    }

    // ── Row 4: offline XML listing parser (body -> entries) ────────────
    // parse_blob_list is the body->struct half of the provider; these lock
    // its behaviour without any live HTTP (deterministic, CI-safe).

    /// A real "List Blobs" page (root prefix): one virtual directory, one
    /// direct blob with size + Last-Modified, one DEEPER blob that must be
    /// filtered out (it lives under the virtual dir, not at this level), and
    /// a NextMarker signalling another page.
    #[test]
    fn parse_blob_list_extracts_dirs_files_and_pagination_marker() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<EnumerationResults ServiceEndpoint="https://myacc.blob.core.windows.net/" ContainerName="mycontainer">
  <Blobs>
    <BlobPrefix>
      <Name>photos/</Name>
    </BlobPrefix>
    <Blob>
      <Name>readme.txt</Name>
      <Properties>
        <Last-Modified>Mon, 01 Jan 2026 12:00:00 GMT</Last-Modified>
        <Content-Length>1234</Content-Length>
        <Content-Type>text/plain</Content-Type>
        <BlobType>BlockBlob</BlobType>
      </Properties>
    </Blob>
    <Blob>
      <Name>photos/deep.jpg</Name>
      <Properties>
        <Content-Length>9999</Content-Length>
      </Properties>
    </Blob>
  </Blobs>
  <NextMarker>2!ABC</NextMarker>
</EnumerationResults>"#;

        let (items, marker) = AzureProvider::parse_blob_list(xml, "");

        // The nested blob (photos/deep.jpg) is filtered -> only the prefix and
        // the top-level file survive.
        assert_eq!(items.len(), 2, "nested blob must be filtered out");

        let dir = items
            .iter()
            .find(|i| i.is_prefix)
            .expect("the virtual directory must be present");
        assert_eq!(dir.name, "photos");
        assert_eq!(dir.size, 0);

        let file = items
            .iter()
            .find(|i| !i.is_prefix)
            .expect("the top-level blob must be present");
        assert_eq!(file.name, "readme.txt");
        assert_eq!(file.size, 1234);
        assert_eq!(
            file.last_modified.as_deref(),
            Some("Mon, 01 Jan 2026 12:00:00 GMT")
        );

        assert!(!items.iter().any(|i| i.name.contains("deep")));
        assert_eq!(marker.as_deref(), Some("2!ABC"));
    }

    /// Inside a sub-prefix the LISTED prefix is stripped from every entry,
    /// and an absent NextMarker yields None (last page).
    #[test]
    fn parse_blob_list_strips_listed_prefix_and_reports_last_page() {
        let xml = r#"<?xml version="1.0"?>
<EnumerationResults>
  <Blobs>
    <Blob>
      <Name>photos/sunset.jpg</Name>
      <Properties>
        <Content-Length>2048</Content-Length>
      </Properties>
    </Blob>
    <BlobPrefix>
      <Name>photos/2026/</Name>
    </BlobPrefix>
  </Blobs>
</EnumerationResults>"#;

        let (items, marker) = AzureProvider::parse_blob_list(xml, "photos/");
        assert_eq!(items.len(), 2);

        let file = items.iter().find(|i| !i.is_prefix).unwrap();
        assert_eq!(file.name, "sunset.jpg", "listed prefix must be stripped");
        assert_eq!(file.size, 2048);

        let dir = items.iter().find(|i| i.is_prefix).unwrap();
        assert_eq!(dir.name, "2026", "sub-prefix relative to the listed prefix");

        assert_eq!(marker, None, "no NextMarker -> last page");
    }

    /// AZURE-LIST-1 regression: a nested listing must strip the prefix the
    /// REQUEST carried, not the provider's `current_prefix`. Every one-shot
    /// `ls /a/b` (CLI, MCP, sync's remote scan, the first GUI call after
    /// connect) runs with `current_prefix` still empty; stripping that instead
    /// left each blob name as a full path, the `!relative.contains('/')` depth
    /// filter dropped all of them, and the directory came back EMPTY even
    /// though `stat` on the very same blob succeeded. A `sync --delete`
    /// against such a directory then planned `delete_local` for every local
    /// file, i.e. data loss on files that do exist remotely.
    #[test]
    fn parse_blob_list_nested_listing_ignores_current_prefix() {
        let mut p = test_provider();
        p.current_prefix = String::new(); // one-shot listing: never cd'd

        let xml = r#"<?xml version="1.0"?>
<EnumerationResults>
  <Blobs>
    <Blob>
      <Name>backup/2026/report.pdf</Name>
      <Properties>
        <Content-Length>512</Content-Length>
      </Properties>
    </Blob>
    <BlobPrefix>
      <Name>backup/2026/raw/</Name>
    </BlobPrefix>
  </Blobs>
</EnumerationResults>"#;

        // What `list("/backup/2026")` now passes down: the prefix it queried.
        let (items, _) = AzureProvider::parse_blob_list(xml, "backup/2026/");
        assert_eq!(items.len(), 2, "nested blobs must not be swallowed");

        let file = items.iter().find(|i| !i.is_prefix).unwrap();
        assert_eq!(file.name, "report.pdf");
        assert_eq!(file.size, 512);

        let dir = items.iter().find(|i| i.is_prefix).unwrap();
        assert_eq!(dir.name, "raw");

        // The old behaviour, reproduced: stripping the (empty) current_prefix
        // leaves full paths, so the depth filter eats the file and the
        // directory keeps a bogus multi-segment name.
        let (broken, _) = AzureProvider::parse_blob_list(xml, &p.current_prefix);
        assert!(
            !broken.iter().any(|i| !i.is_prefix),
            "regression guard: this is exactly the empty-directory bug"
        );
    }

    /// A malformed / non-list body never panics: it yields no items and no
    /// marker (the async caller then surfaces the HTTP error instead).
    #[test]
    fn parse_blob_list_tolerates_garbage_body() {
        let (items, marker) = AzureProvider::parse_blob_list("not xml at all <<<", "");
        assert!(items.is_empty());
        assert_eq!(marker, None);
    }

    /// CR-536 regression: a whitespace-ONLY Text fragment is indentation
    /// only when no <Name> element is open. Between two entity refs
    /// (`a&amp; &amp;b.txt`) or at an element edge (` &amp;x.txt`) it is
    /// part of the blob name and must survive; pretty-print indentation
    /// between elements must still be ignored.
    #[test]
    fn parse_blob_list_preserves_whitespace_only_fragments_around_entities() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<EnumerationResults>
  <Blobs>
    <Blob>
      <Name>a&amp; &amp;b.txt</Name>
      <Properties>
        <Content-Length>3</Content-Length>
      </Properties>
    </Blob>
    <Blob>
      <Name> &amp;x.txt</Name>
      <Properties>
        <Content-Length>4</Content-Length>
      </Properties>
    </Blob>
  </Blobs>
</EnumerationResults>"#;

        let (items, marker) = AzureProvider::parse_blob_list(xml, "");
        assert_eq!(marker, None);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].name, "a& &b.txt");
        assert_eq!(items[0].size, 3);
        assert_eq!(items[1].name, " &x.txt");
    }
}
