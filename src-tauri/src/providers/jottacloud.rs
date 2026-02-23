//! Jottacloud Storage Provider
//!
//! Implements StorageProvider for Jottacloud using the JFS REST API.
//! Authentication: Personal Login Token → OIDC discovery → OAuth2 token exchange.
//! API reference: rclone Jottacloud backend (no official docs available).
//!
//! JFS Base: https://jfs.jottacloud.com/jfs/
//! API Base: https://api.jottacloud.com/
//! Path: /{username}/{device}/{mountpoint}/{path}
//! Upload: two-phase (allocate → upload)
//! Listing: XML format parsed with quick-xml

use async_trait::async_trait;
use base64::Engine;
use reqwest::header::{HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use secrecy::ExposeSecret;
use serde::Deserialize;
use std::collections::HashMap;
use std::time::Instant;
use tracing::info;

use super::{
    ProviderError, ProviderType, RemoteEntry, StorageInfo, StorageProvider,
    sanitize_api_error, JottacloudConfig, HttpRetryConfig, send_with_retry,
};

const JFS_BASE: &str = "https://jfs.jottacloud.com/jfs";
const API_BASE: &str = "https://api.jottacloud.com";

fn jotta_log(msg: &str) {
    info!("[JOTTACLOUD] {}", msg);
}

// ─── Auth Types ─────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct LoginToken {
    username: Option<String>,
    auth_token: Option<String>,
    #[serde(alias = "wellKnownLink")]
    well_known_link: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OidcConfig {
    token_endpoint: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
}

// ─── API Types ──────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct CustomerInfo {
    username: Option<String>,
    #[serde(default)]
    usage: i64,
    #[serde(default)]
    quota: i64,
}

// ─── Provider ───────────────────────────────────────────────────────────

pub struct JottacloudProvider {
    config: JottacloudConfig,
    client: reqwest::Client,
    connected: bool,
    username: String,
    access_token: String,
    refresh_token: String,
    token_endpoint: String,
    token_expiry: Instant,
    current_path: String,
}

impl JottacloudProvider {
    pub fn new(config: JottacloudConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .connect_timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            config,
            client,
            connected: false,
            username: String::new(),
            access_token: String::new(),
            refresh_token: String::new(),
            token_endpoint: String::new(),
            token_expiry: Instant::now(),
            current_path: "/".to_string(),
        }
    }

    // ─── Auth Helpers ───────────────────────────────────────────────────

    fn decode_login_token(token_str: &str) -> Result<LoginToken, ProviderError> {
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(token_str.trim())
            .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(token_str.trim()))
            .map_err(|e| ProviderError::AuthenticationFailed(
                format!("Invalid login token (Base64 decode failed): {}", e)
            ))?;
        let token: LoginToken = serde_json::from_slice(&decoded)
            .map_err(|e| ProviderError::AuthenticationFailed(
                format!("Invalid login token (JSON parse failed): {}", e)
            ))?;
        if token.username.as_ref().map_or(true, |u| u.is_empty()) {
            return Err(ProviderError::AuthenticationFailed("Login token missing username".to_string()));
        }
        if token.auth_token.as_ref().map_or(true, |t| t.is_empty()) {
            return Err(ProviderError::AuthenticationFailed("Login token missing auth_token".to_string()));
        }
        if token.well_known_link.as_ref().map_or(true, |l| l.is_empty()) {
            return Err(ProviderError::AuthenticationFailed("Login token missing wellKnownLink".to_string()));
        }
        Ok(token)
    }

    async fn discover_oidc(&self, well_known_url: &str) -> Result<String, ProviderError> {
        // Validate URL scheme
        if !well_known_url.starts_with("https://") {
            return Err(ProviderError::AuthenticationFailed(
                "OIDC well-known URL must use HTTPS".to_string()
            ));
        }
        let resp = self.client.get(well_known_url)
            .send()
            .await
            .map_err(|e| ProviderError::AuthenticationFailed(
                format!("OIDC discovery failed: {}", e)
            ))?;
        if !resp.status().is_success() {
            return Err(ProviderError::AuthenticationFailed(
                format!("OIDC discovery returned {}", resp.status())
            ));
        }
        let config: OidcConfig = resp.json().await.map_err(|e| {
            ProviderError::AuthenticationFailed(format!("OIDC config parse failed: {}", e))
        })?;
        config.token_endpoint.ok_or_else(|| {
            ProviderError::AuthenticationFailed("OIDC config missing token_endpoint".to_string())
        })
    }

    async fn exchange_token(
        &self,
        token_endpoint: &str,
        username: &str,
        auth_token: &str,
    ) -> Result<TokenResponse, ProviderError> {
        let form_body = format!(
            "grant_type=password&username={}&password={}&scope={}&client_id=jottacli",
            urlencoding::encode(username),
            urlencoding::encode(auth_token),
            urlencoding::encode("openid offline_access"),
        );
        let resp = self.client.post(token_endpoint)
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(form_body)
            .send()
            .await
            .map_err(|e| ProviderError::AuthenticationFailed(
                format!("Token exchange failed: {}", e)
            ))?;
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::AuthenticationFailed(
                format!("Token exchange failed: {}", sanitize_api_error(&body))
            ));
        }
        let token_resp: TokenResponse = resp.json().await.map_err(|e| {
            ProviderError::AuthenticationFailed(format!("Token response parse failed: {}", e))
        })?;
        if token_resp.access_token.is_none() {
            return Err(ProviderError::AuthenticationFailed(
                "Token exchange returned no access_token".to_string()
            ));
        }
        Ok(token_resp)
    }

    async fn refresh_if_needed(&mut self) -> Result<(), ProviderError> {
        // Refresh 60 seconds before expiry
        if Instant::now() < self.token_expiry - std::time::Duration::from_secs(60) {
            return Ok(());
        }
        if self.refresh_token.is_empty() || self.token_endpoint.is_empty() {
            return Err(ProviderError::AuthenticationFailed(
                "Cannot refresh: no refresh token available".to_string()
            ));
        }
        jotta_log("Refreshing access token");
        // Jottacloud quirk: uppercase REFRESH_TOKEN
        let form_body = format!(
            "grant_type=REFRESH_TOKEN&refresh_token={}&client_id=jottacli",
            urlencoding::encode(&self.refresh_token),
        );
        let resp = self.client.post(&self.token_endpoint)
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(form_body)
            .send()
            .await
            .map_err(|e| ProviderError::AuthenticationFailed(
                format!("Token refresh failed: {}", e)
            ))?;
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::AuthenticationFailed(
                format!("Token refresh failed: {}", sanitize_api_error(&body))
            ));
        }
        let token_resp: TokenResponse = resp.json().await.map_err(|e| {
            ProviderError::AuthenticationFailed(format!("Refresh response parse failed: {}", e))
        })?;
        if let Some(ref at) = token_resp.access_token {
            self.access_token = at.clone();
        }
        if let Some(ref rt) = token_resp.refresh_token {
            self.refresh_token = rt.clone();
        }
        let expires_in = token_resp.expires_in.unwrap_or(3600);
        self.token_expiry = Instant::now() + std::time::Duration::from_secs(expires_in);
        jotta_log("Access token refreshed");
        Ok(())
    }

    fn auth_header(&self) -> HeaderValue {
        HeaderValue::from_str(&format!("Bearer {}", self.access_token))
            .unwrap_or_else(|_| HeaderValue::from_static(""))
    }

    // ─── HTTP Helpers ───────────────────────────────────────────────────

    async fn get_with_retry(&mut self, url: &str) -> Result<reqwest::Response, ProviderError> {
        self.refresh_if_needed().await?;
        let request = self.client.get(url)
            .header(AUTHORIZATION, self.auth_header())
            .build()
            .map_err(|e| ProviderError::ConnectionFailed(format!("Build request failed: {}", e)))?;
        send_with_retry(&self.client, request, &HttpRetryConfig::default())
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("Request failed: {}", e)))
    }

    async fn post_with_retry(&mut self, url: &str, content_type: &str, body: Vec<u8>) -> Result<reqwest::Response, ProviderError> {
        self.refresh_if_needed().await?;
        let request = self.client.post(url)
            .header(AUTHORIZATION, self.auth_header())
            .header(CONTENT_TYPE, content_type)
            .body(body)
            .build()
            .map_err(|e| ProviderError::ConnectionFailed(format!("Build request failed: {}", e)))?;
        send_with_retry(&self.client, request, &HttpRetryConfig::default())
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("Request failed: {}", e)))
    }

    // ─── Path Helpers ───────────────────────────────────────────────────

    /// Build full JFS URL: /jfs/{username}/{device}/{mountpoint}/{path}
    fn jfs_url(&self, path: &str) -> String {
        let clean = path.trim_start_matches('/');
        if clean.is_empty() {
            format!("{}/{}/{}/{}", JFS_BASE, self.username, self.config.device, self.config.mountpoint)
        } else {
            format!("{}/{}/{}/{}/{}", JFS_BASE, self.username, self.config.device, self.config.mountpoint, clean)
        }
    }

    fn normalize_path(path: &str) -> String {
        let trimmed = path.trim().replace('\\', "/");
        if trimmed.is_empty() || trimmed == "/" {
            return "/".to_string();
        }
        let p = if trimmed.starts_with('/') { trimmed } else { format!("/{}", trimmed) };
        p.trim_end_matches('/').to_string()
    }

    fn resolve_path(&self, path: &str) -> String {
        let path = path.trim();
        if path.is_empty() || path == "." {
            return self.current_path.clone();
        }
        if path.starts_with('/') {
            return Self::normalize_path(path);
        }
        let base = self.current_path.trim_end_matches('/');
        Self::normalize_path(&format!("{}/{}", base, path))
    }

    fn split_path(path: &str) -> (String, String) {
        let normalized = Self::normalize_path(path);
        if let Some(pos) = normalized.rfind('/') {
            let parent = if pos == 0 { "/".to_string() } else { normalized[..pos].to_string() };
            let name = normalized[pos + 1..].to_string();
            (parent, name)
        } else {
            ("/".to_string(), normalized)
        }
    }

    // ─── Discovery Helpers ────────────────────────────────────────────

    /// Parse device names from /jfs/{username} XML response.
    /// Looks for <device><name>text</name></device> under <devices>.
    fn parse_device_names(xml: &str) -> Vec<String> {
        use quick_xml::events::Event;
        use quick_xml::Reader;

        let mut names = Vec::new();
        let mut reader = Reader::from_str(xml);
        reader.config_mut().trim_text(true);
        let mut buf = Vec::new();
        let mut in_devices = false;
        let mut in_device = false;
        let mut in_name = false;

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => {
                    let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                    match tag.as_str() {
                        "devices" => in_devices = true,
                        "device" if in_devices => in_device = true,
                        "name" if in_device => in_name = true,
                        _ => {}
                    }
                }
                Ok(Event::End(ref e)) => {
                    let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                    match tag.as_str() {
                        "devices" => { in_devices = false; in_device = false; }
                        "device" => in_device = false,
                        "name" => in_name = false,
                        _ => {}
                    }
                }
                Ok(Event::Text(ref e)) if in_name => {
                    let text = String::from_utf8_lossy(e.as_ref()).trim().to_string();
                    if !text.is_empty() {
                        names.push(text);
                    }
                }
                Ok(Event::Eof) => break,
                Err(_) => break,
                _ => {}
            }
            buf.clear();
        }
        names
    }

    /// Parse mountpoint names from /jfs/{username}/{device} XML response.
    /// Looks for <mountPoint name="..."> elements.
    fn parse_mountpoint_names(xml: &str) -> Vec<String> {
        use quick_xml::events::Event;
        use quick_xml::Reader;

        let mut names = Vec::new();
        let mut reader = Reader::from_str(xml);
        reader.config_mut().trim_text(true);
        let mut buf = Vec::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                    let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                    if tag == "mountPoint" {
                        for attr in e.attributes().flatten() {
                            if attr.key.as_ref() == b"name" {
                                let name = String::from_utf8_lossy(&attr.value).to_string();
                                if !name.is_empty() {
                                    names.push(name);
                                }
                            }
                        }
                    }
                }
                Ok(Event::Eof) => break,
                Err(_) => break,
                _ => {}
            }
            buf.clear();
        }
        names
    }

    /// Auto-discover device and mountpoint from the user's account.
    /// Queries /jfs/{username} for devices, then /jfs/{username}/{device} for mountpoints.
    /// Falls back to configured defaults if discovery fails.
    async fn discover_device_mountpoint(&mut self) -> Result<(), ProviderError> {
        // Step 1: Query user root to find devices
        let user_url = format!("{}/{}", JFS_BASE, self.username);
        let resp = self.get_with_retry(&user_url).await;

        if let Ok(resp) = resp {
            if resp.status().is_success() {
                let xml = resp.text().await.unwrap_or_default();
                jotta_log(&format!("Device discovery XML ({} bytes): {}",
                    xml.len(), &xml[..xml.len().min(500)]));
                let devices = Self::parse_device_names(&xml);
                jotta_log(&format!("Available devices: {:?}", devices));

                // Pick device: prefer configured, then "Jotta", then first available
                if !devices.is_empty() {
                    if !devices.contains(&self.config.device) {
                        if devices.contains(&"Jotta".to_string()) {
                            self.config.device = "Jotta".to_string();
                        } else {
                            self.config.device = devices[0].clone();
                        }
                    }
                }
            }
        }

        // Step 2: Query device to find mountpoints
        let device_url = format!("{}/{}/{}", JFS_BASE, self.username, self.config.device);
        let resp = self.get_with_retry(&device_url).await;

        if let Ok(resp) = resp {
            if resp.status().is_success() {
                let xml = resp.text().await.unwrap_or_default();
                jotta_log(&format!("Mountpoint discovery XML ({} bytes): {}",
                    xml.len(), &xml[..xml.len().min(500)]));
                let mountpoints = Self::parse_mountpoint_names(&xml);
                jotta_log(&format!("Available mountpoints on {}: {:?}", self.config.device, mountpoints));

                // Pick mountpoint: prefer configured, then "Archive", then "Sync", then first
                if !mountpoints.is_empty() {
                    if !mountpoints.contains(&self.config.mountpoint) {
                        if mountpoints.contains(&"Archive".to_string()) {
                            self.config.mountpoint = "Archive".to_string();
                        } else if mountpoints.contains(&"Sync".to_string()) {
                            self.config.mountpoint = "Sync".to_string();
                        } else {
                            self.config.mountpoint = mountpoints[0].clone();
                        }
                    }
                }
            }
        }

        jotta_log(&format!("Using device={}, mountpoint={}", self.config.device, self.config.mountpoint));
        Ok(())
    }

    // ─── XML Parsing ────────────────────────────────────────────────────

    /// Parse JFS XML folder listing into RemoteEntry items.
    /// Handles both `<folder>` and `<mountPoint>` as root elements.
    /// Handles both full `<folder name="X">...</folder>` and self-closing `<folder name="X"/>`.
    /// Only includes files with state=COMPLETED (skips INCOMPLETE, CORRUPT, ADDED).
    fn parse_folder_xml(xml: &str, base_path: &str) -> Vec<RemoteEntry> {
        use quick_xml::events::Event;
        use quick_xml::Reader;

        let mut entries = Vec::new();
        let mut reader = Reader::from_str(xml);
        reader.config_mut().trim_text(true);
        let mut buf = Vec::new();

        let mut depth: u32 = 0;
        let mut root_depth: Option<u32> = None;

        // Folder section: <folders> wrapper inside root
        let mut in_folders_section = false;
        let mut folders_section_depth: u32 = 0;
        let mut child_folder_depth: Option<u32> = None; // skip nested content

        // File parsing state
        let mut in_file = false;
        let mut in_revision = false;
        let mut current_name = String::new();
        let mut current_size: u64 = 0;
        let mut current_modified = String::new();
        let mut current_mime = String::new();
        let mut current_md5 = String::new();
        let mut current_state = String::new();
        let mut current_deleted = false; // skip trashed files
        let mut current_tag = String::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => {
                    let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                    depth += 1;

                    match tag.as_str() {
                        "folder" | "mountPoint" if root_depth.is_none() => {
                            root_depth = Some(depth);
                        }
                        "folders" if root_depth == Some(depth - 1) && child_folder_depth.is_none() => {
                            in_folders_section = true;
                            folders_section_depth = depth;
                        }
                        "folder" if child_folder_depth.is_none() && (
                            in_folders_section ||
                            (root_depth.is_some() && depth == root_depth.unwrap() + 1)
                        ) => {
                            // Direct child folder (full element) — inside <folders> or direct child of root
                            let mut name = String::new();
                            let mut is_deleted = false;
                            for attr in e.attributes().flatten() {
                                if attr.key.as_ref() == b"name" {
                                    name = String::from_utf8_lossy(&attr.value).to_string();
                                }
                                if attr.key.as_ref() == b"deleted" {
                                    is_deleted = true;
                                }
                            }
                            if !name.is_empty() && !is_deleted {
                                let entry_path = if base_path == "/" {
                                    format!("/{}", name)
                                } else {
                                    format!("{}/{}", base_path, name)
                                };
                                entries.push(RemoteEntry {
                                    name,
                                    path: entry_path,
                                    is_dir: true,
                                    size: 0,
                                    modified: None,
                                    permissions: None,
                                    owner: None,
                                    group: None,
                                    is_symlink: false,
                                    link_target: None,
                                    metadata: HashMap::new(),
                                    mime_type: None,
                                });
                            }
                            child_folder_depth = Some(depth);
                        }
                        "file" if !in_file && child_folder_depth.is_none() => {
                            in_file = true;
                            current_name.clear();
                            current_size = 0;
                            current_modified.clear();
                            current_mime.clear();
                            current_md5.clear();
                            current_state.clear();
                            current_deleted = false;
                            for attr in e.attributes().flatten() {
                                if attr.key.as_ref() == b"name" {
                                    current_name = String::from_utf8_lossy(&attr.value).to_string();
                                }
                            }
                        }
                        "currentRevision" if in_file => {
                            in_revision = true;
                        }
                        _ => {
                            current_tag = tag;
                        }
                    }
                }
                Ok(Event::Empty(ref e)) => {
                    let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();

                    if tag == "folder" && child_folder_depth.is_none() && (
                        in_folders_section ||
                        (root_depth.is_some() && depth == root_depth.unwrap())
                    ) {
                        // Self-closing <folder name="X"/> — direct child
                        let mut name = String::new();
                        let mut is_deleted = false;
                        for attr in e.attributes().flatten() {
                            if attr.key.as_ref() == b"name" {
                                name = String::from_utf8_lossy(&attr.value).to_string();
                            }
                            if attr.key.as_ref() == b"deleted" {
                                is_deleted = true;
                            }
                        }
                        if !name.is_empty() && !is_deleted {
                            let entry_path = if base_path == "/" {
                                format!("/{}", name)
                            } else {
                                format!("{}/{}", base_path, name)
                            };
                            entries.push(RemoteEntry {
                                name,
                                path: entry_path,
                                is_dir: true,
                                size: 0,
                                modified: None,
                                permissions: None,
                                owner: None,
                                group: None,
                                is_symlink: false,
                                link_target: None,
                                metadata: HashMap::new(),
                                mime_type: None,
                            });
                        }
                    }
                }
                Ok(Event::End(ref e)) => {
                    let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();

                    match tag.as_str() {
                        "folders" if in_folders_section && depth == folders_section_depth => {
                            in_folders_section = false;
                        }
                        "folder" if child_folder_depth == Some(depth) => {
                            child_folder_depth = None;
                        }
                        "file" if in_file => {
                            in_file = false;
                            in_revision = false;
                            if current_state == "COMPLETED" && !current_deleted && !current_name.is_empty() {
                                let entry_path = if base_path == "/" {
                                    format!("/{}", current_name)
                                } else {
                                    format!("{}/{}", base_path, current_name)
                                };
                                let mut metadata = HashMap::new();
                                if !current_md5.is_empty() {
                                    metadata.insert("md5".to_string(), current_md5.clone());
                                }
                                entries.push(RemoteEntry {
                                    name: current_name.clone(),
                                    path: entry_path,
                                    is_dir: false,
                                    size: current_size,
                                    modified: if current_modified.is_empty() { None } else { Some(current_modified.clone()) },
                                    permissions: None,
                                    owner: None,
                                    group: None,
                                    is_symlink: false,
                                    link_target: None,
                                    metadata,
                                    mime_type: if current_mime.is_empty() { None } else { Some(current_mime.clone()) },
                                });
                            }
                        }
                        "currentRevision" => {
                            in_revision = false;
                        }
                        _ => {}
                    }

                    depth = depth.saturating_sub(1);
                    current_tag.clear();
                }
                Ok(Event::Text(ref e)) => {
                    let text = String::from_utf8_lossy(e.as_ref()).trim().to_string();
                    if in_file {
                        // <deleted> tag at file level (outside revision) marks trashed files
                        if current_tag == "deleted" && !text.is_empty() {
                            current_deleted = true;
                        }
                        if in_revision {
                            match current_tag.as_str() {
                                "size" => { current_size = text.parse().unwrap_or(0); }
                                "mime" => { current_mime = text; }
                                "md5" => { current_md5 = text; }
                                "state" => { current_state = text; }
                                "modified" | "updated" => {
                                    if current_modified.is_empty() {
                                        current_modified = Self::parse_jotta_time(&text);
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
                Ok(Event::Eof) => break,
                Err(_) => break,
                _ => {}
            }
            buf.clear();
        }

        entries
    }

    /// Parse Jottacloud time format "2006-01-02-T15:04:05Z0700" into ISO 8601
    fn parse_jotta_time(s: &str) -> String {
        // Jottacloud uses a non-standard format with an extra dash before T
        // "2023-01-15-T10:30:45Z0100" → "2023-01-15T10:30:45+01:00"
        let cleaned = s.replace("-T", "T");
        // Try to parse and format nicely, or return as-is
        if let Ok(dt) = chrono::DateTime::parse_from_str(&cleaned, "%Y-%m-%dT%H:%M:%S%z") {
            return dt.format("%Y-%m-%d %H:%M:%S").to_string();
        }
        // Try RFC3339
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&cleaned) {
            return dt.format("%Y-%m-%d %H:%M:%S").to_string();
        }
        // Return cleaned version
        cleaned
    }
}

// ─── StorageProvider Implementation ──────────────────────────────────────

#[async_trait]
impl StorageProvider for JottacloudProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }

    fn provider_type(&self) -> ProviderType {
        ProviderType::Jottacloud
    }

    fn display_name(&self) -> String {
        format!("Jottacloud ({})", self.username)
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        jotta_log("Connecting to Jottacloud");

        // Step 1: Decode login token
        let login_token = Self::decode_login_token(self.config.login_token.expose_secret())?;
        let username = login_token.username.unwrap_or_default();
        let auth_token = login_token.auth_token.unwrap_or_default();
        let well_known_link = login_token.well_known_link.unwrap_or_default();

        jotta_log(&format!("Username: {}, discovering OIDC from well-known URL", username));

        // Step 2: OIDC discovery
        let token_endpoint = self.discover_oidc(&well_known_link).await?;
        jotta_log(&format!("Token endpoint discovered: {}", token_endpoint));

        // Step 3: Exchange credentials for access token
        let token_resp = self.exchange_token(&token_endpoint, &username, &auth_token).await?;

        self.username = username;
        self.access_token = token_resp.access_token.unwrap_or_default();
        self.refresh_token = token_resp.refresh_token.unwrap_or_default();
        self.token_endpoint = token_endpoint;
        let expires_in = token_resp.expires_in.unwrap_or(3600);
        self.token_expiry = Instant::now() + std::time::Duration::from_secs(expires_in);

        // Step 4: Verify by fetching customer info
        let url = format!("{}/account/v1/customer", API_BASE);
        let resp = self.get_with_retry(&url).await?;

        if resp.status().as_u16() == 401 {
            return Err(ProviderError::AuthenticationFailed(
                "Invalid credentials. Regenerate your Personal Login Token at jottacloud.com → Settings → Security".to_string()
            ));
        }
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ConnectionFailed(format!(
                "Jottacloud connection failed: {}", sanitize_api_error(&body)
            )));
        }

        let customer: CustomerInfo = resp.json().await.map_err(|e| {
            ProviderError::ConnectionFailed(format!("Failed to parse customer info: {}", e))
        })?;

        // Use customer info username for JFS paths (may differ from login token username)
        if let Some(ref u) = customer.username {
            if !u.is_empty() && *u != self.username {
                jotta_log(&format!("JFS username from customer info: {} (token had: {})", u, self.username));
                self.username = u.clone();
            } else {
                jotta_log(&format!("Authenticated as: {}", u));
            }
        }

        // Step 5: Auto-discover device and mountpoint
        self.discover_device_mountpoint().await?;

        // Step 6: Navigate to initial path
        self.current_path = "/".to_string();
        if let Some(ref initial) = self.config.initial_path {
            let initial = initial.trim().to_string();
            if !initial.is_empty() && initial != "/" {
                self.current_path = Self::normalize_path(&initial);
            }
        }

        self.connected = true;
        jotta_log(&format!("Connected (device={}, mountpoint={})", self.config.device, self.config.mountpoint));
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.connected = false;
        self.current_path = "/".to_string();
        self.access_token.clear();
        self.refresh_token.clear();
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> {
        Ok(self.current_path.clone())
    }

    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        let new_path = if path.starts_with('/') {
            Self::normalize_path(path)
        } else if path == ".." {
            let mut parts: Vec<&str> = self.current_path.split('/').filter(|s| !s.is_empty()).collect();
            parts.pop();
            if parts.is_empty() {
                "/".to_string()
            } else {
                format!("/{}", parts.join("/"))
            }
        } else {
            let base = self.current_path.trim_end_matches('/');
            format!("{}/{}", base, path)
        };

        // Verify directory exists by listing it
        let url = self.jfs_url(&new_path);
        let resp = self.get_with_retry(&url).await?;
        if !resp.status().is_success() {
            return Err(ProviderError::NotFound(format!("Directory not found: {}", new_path)));
        }

        self.current_path = new_path;
        Ok(())
    }

    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        self.cd("..").await
    }

    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let resolved = self.resolve_path(path);
        let url = self.jfs_url(&resolved);

        let resp = self.get_with_retry(&url).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            if status.as_u16() == 404 {
                return Err(ProviderError::NotFound(format!("Path not found: {}", resolved)));
            }
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "List {} failed ({}): {}", resolved, status, sanitize_api_error(&body)
            )));
        }

        let xml = resp.text().await.map_err(|e| {
            ProviderError::ServerError(format!("Failed to read response: {}", e))
        })?;

        jotta_log(&format!("List XML for '{}' ({} bytes): {}", resolved, xml.len(), &xml[..xml.len().min(2000)]));

        let entries = Self::parse_folder_xml(&xml, &resolved);
        jotta_log(&format!("Parsed {} entries (dirs={}, files={})",
            entries.len(),
            entries.iter().filter(|e| e.is_dir).count(),
            entries.iter().filter(|e| !e.is_dir).count(),
        ));
        Ok(entries)
    }

    async fn download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let resolved = self.resolve_path(remote_path);
        let url = format!("{}?mode=bin", self.jfs_url(&resolved));

        let resp = self.get_with_retry(&url).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::TransferFailed(format!(
                "Download {} failed ({}): {}", resolved, status, sanitize_api_error(&body)
            )));
        }

        let total_size = resp.content_length().unwrap_or(0);
        let mut file = tokio::fs::File::create(local_path).await.map_err(|e| {
            ProviderError::TransferFailed(format!("Create local file failed: {}", e))
        })?;

        use tokio::io::AsyncWriteExt;
        let mut stream = resp.bytes_stream();
        use futures_util::StreamExt;
        let mut downloaded: u64 = 0;

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| {
                ProviderError::TransferFailed(format!("Download stream error: {}", e))
            })?;
            file.write_all(&chunk).await.map_err(|e| {
                ProviderError::TransferFailed(format!("Write failed: {}", e))
            })?;
            downloaded += chunk.len() as u64;
            if let Some(ref cb) = progress {
                cb(downloaded, total_size);
            }
        }

        file.flush().await.map_err(|e| {
            ProviderError::TransferFailed(format!("Flush failed: {}", e))
        })?;

        jotta_log(&format!("Downloaded {} ({} bytes)", resolved, downloaded));
        Ok(())
    }

    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let resolved = self.resolve_path(remote_path);
        let data = tokio::fs::read(local_path).await.map_err(|e| {
            ProviderError::TransferFailed(format!("Read local file failed: {}", e))
        })?;

        let total_size = data.len() as u64;

        // Calculate MD5 for deduplication
        use md5::{Md5, Digest};
        let mut hasher = Md5::new();
        hasher.update(&data);
        let md5_hash = format!("{:x}", hasher.finalize());

        // Get file modification time in Jottacloud format: "2006-01-02-T15:04:05Z" (extra dash before T)
        let modified_time = tokio::fs::metadata(local_path).await
            .ok()
            .and_then(|m| m.modified().ok())
            .map(|t| {
                let dt: chrono::DateTime<chrono::Utc> = t.into();
                dt.format("%Y-%m-%d-T%H:%M:%SZ").to_string()
            })
            .unwrap_or_else(|| chrono::Utc::now().format("%Y-%m-%d-T%H:%M:%SZ").to_string());

        // Direct upload to up.jottacloud.com (rclone-compatible method)
        // POST https://up.jottacloud.com/jfs/{user}/{device}/{mountpoint}/{path}
        let clean = resolved.trim_start_matches('/');
        // URL-encode each path segment to handle special characters
        let encoded_path: String = clean.split('/')
            .map(|s| urlencoding::encode(s).into_owned())
            .collect::<Vec<_>>()
            .join("/");
        let upload_url = format!(
            "https://up.jottacloud.com/jfs/{}/{}/{}/{}",
            urlencoding::encode(&self.username),
            urlencoding::encode(&self.config.device),
            urlencoding::encode(&self.config.mountpoint),
            encoded_path
        );

        jotta_log(&format!("Upload URL: {}", upload_url));

        // Extract filename for multipart
        let filename = resolved.rsplit('/').next().unwrap_or("file").to_string();

        self.refresh_if_needed().await?;

        // Upload as multipart/form-data with "file" field (rclone-compatible)
        let file_part = reqwest::multipart::Part::bytes(data)
            .file_name(filename)
            .mime_str("application/octet-stream")
            .map_err(|e| ProviderError::TransferFailed(format!("Multipart error: {}", e)))?;
        let form = reqwest::multipart::Form::new()
            .part("file", file_part);

        let resp = self.client.post(&upload_url)
            .header(AUTHORIZATION, self.auth_header())
            .header("JMd5", &md5_hash)
            .header("JSize", total_size.to_string())
            .header("JCreated", &modified_time)
            .header("JModified", &modified_time)
            .multipart(form)
            .send()
            .await
            .map_err(|e| ProviderError::TransferFailed(format!("Upload failed: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            jotta_log(&format!("Upload error response: {}", &body[..body.len().min(1000)]));
            return Err(ProviderError::TransferFailed(format!(
                "Upload failed ({}): {}", status, sanitize_api_error(&body)
            )));
        }

        if let Some(ref cb) = progress {
            cb(total_size, total_size);
        }

        jotta_log(&format!("Uploaded {} ({} bytes)", resolved, total_size));
        Ok(())
    }

    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let resolved = self.resolve_path(path);
        let url = format!("{}?mkDir=true", self.jfs_url(&resolved));

        let resp = self.post_with_retry(&url, "application/octet-stream", vec![]).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Mkdir {} failed ({}): {}", resolved, status, sanitize_api_error(&body)
            )));
        }

        jotta_log(&format!("Created directory: {}", resolved));
        Ok(())
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        let resolved = self.resolve_path(path);
        // Hard delete (?rm=true) — removes immediately without going to trash
        let url = format!("{}?rm=true", self.jfs_url(&resolved));

        let resp = self.post_with_retry(&url, "application/octet-stream", vec![]).await?;

        if !resp.status().is_success() {
            // Try directory hard delete
            let url_dir = format!("{}?rmDir=true", self.jfs_url(&resolved));
            let resp_dir = self.post_with_retry(&url_dir, "application/octet-stream", vec![]).await?;
            if !resp_dir.status().is_success() {
                let status = resp_dir.status();
                let body = resp_dir.text().await.unwrap_or_default();
                return Err(ProviderError::ServerError(format!(
                    "Delete {} failed ({}): {}", resolved, status, sanitize_api_error(&body)
                )));
            }
        }

        jotta_log(&format!("Deleted: {}", resolved));
        Ok(())
    }

    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let resolved_from = self.resolve_path(from);
        let resolved_to = self.resolve_path(to);

        // Use move operation for rename
        let to_jfs = format!("/{}/{}/{}/{}", self.username, self.config.device, self.config.mountpoint,
            resolved_to.trim_start_matches('/'));
        let url = format!("{}?mv={}", self.jfs_url(&resolved_from), urlencoding::encode(&to_jfs));

        let resp = self.post_with_retry(&url, "application/octet-stream", vec![]).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Rename {} → {} failed ({}): {}", resolved_from, resolved_to, status, sanitize_api_error(&body)
            )));
        }

        jotta_log(&format!("Renamed {} → {}", resolved_from, resolved_to));
        Ok(())
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        let resolved = self.resolve_path(path);
        let url = self.jfs_url(&resolved);

        let resp = self.get_with_retry(&url).await?;

        if !resp.status().is_success() {
            return Err(ProviderError::NotFound(format!("Path not found: {}", resolved)));
        }

        let xml = resp.text().await.map_err(|e| {
            ProviderError::ServerError(format!("Failed to read response: {}", e))
        })?;

        // Check if response is a folder or file
        let (_, name) = Self::split_path(&resolved);
        let is_dir = xml.contains("<folders>") || xml.contains("<folder ");

        if is_dir {
            Ok(RemoteEntry {
                name,
                path: resolved,
                is_dir: true,
                size: 0,
                modified: None,
                permissions: None,
                owner: None,
                group: None,
                is_symlink: false,
                link_target: None,
                metadata: HashMap::new(),
                mime_type: None,
            })
        } else {
            // Try to parse as file listing (parent folder containing the file)
            let entries = Self::parse_folder_xml(&xml, &resolved);
            entries.into_iter().next().ok_or_else(|| {
                // Return basic entry if parsing yields nothing
                ProviderError::NotFound(format!("Could not stat: {}", resolved))
            })
        }
    }

    async fn find(&mut self, path: &str, pattern: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let resolved = self.resolve_path(path);
        // Use recursive listing and filter by pattern
        let url = format!("{}?mode=list", self.jfs_url(&resolved));

        let resp = self.get_with_retry(&url).await?;

        if !resp.status().is_success() {
            return Ok(vec![]);
        }

        let xml = resp.text().await.unwrap_or_default();
        let all_entries = Self::parse_folder_xml(&xml, &resolved);

        let pattern_lower = pattern.to_lowercase();
        Ok(all_entries.into_iter().filter(|e| {
            e.name.to_lowercase().contains(&pattern_lower)
        }).collect())
    }

    async fn storage_info(&mut self) -> Result<StorageInfo, ProviderError> {
        let url = format!("{}/account/v1/customer", API_BASE);
        let resp = self.get_with_retry(&url).await?;

        if !resp.status().is_success() {
            return Err(ProviderError::ServerError("Failed to get storage info".to_string()));
        }

        let customer: CustomerInfo = resp.json().await.map_err(|e| {
            ProviderError::ServerError(format!("Parse customer info failed: {}", e))
        })?;

        let used = customer.usage.max(0) as u64;
        let total = customer.quota.max(0) as u64;
        let free = if total > used { total - used } else { 0 };

        Ok(StorageInfo {
            used,
            total,
            free,
        })
    }

    async fn create_share_link(&mut self, path: &str, _expires_in_secs: Option<u64>) -> Result<String, ProviderError> {
        let resolved = self.resolve_path(path);
        let url = format!("{}?mode=enableShare", self.jfs_url(&resolved));

        let resp = self.post_with_retry(&url, "application/octet-stream", vec![]).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Share link failed ({}): {}", status, sanitize_api_error(&body)
            )));
        }

        // Parse XML response for publicURI
        let xml = resp.text().await.unwrap_or_default();
        // Look for <publicURI> tag
        if let Some(start) = xml.find("<publicURI>") {
            if let Some(end) = xml[start..].find("</publicURI>") {
                let uri = &xml[start + 11..start + end];
                return Ok(format!("https://www.jottacloud.com{}", uri));
            }
        }

        Err(ProviderError::ServerError("Share link created but publicURI not found in response".to_string()))
    }

    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        let resolved = self.resolve_path(remote_path);
        let url = format!("{}?mode=bin", self.jfs_url(&resolved));

        let resp = self.get_with_retry(&url).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::TransferFailed(format!(
                "Download {} failed ({}): {}", resolved, status, sanitize_api_error(&body)
            )));
        }

        let bytes = resp.bytes().await.map_err(|e| {
            ProviderError::TransferFailed(format!("Failed to read response bytes: {}", e))
        })?;

        Ok(bytes.to_vec())
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        self.delete(path).await
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        // Jottacloud delete with dlDir=true removes recursively
        self.delete(path).await
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
        // REST API doesn't need keep-alive
        Ok(())
    }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        Ok(format!(
            "Jottacloud — User: {}, Device: {}, Mountpoint: {}",
            self.username, self.config.device, self.config.mountpoint
        ))
    }

    fn supports_find(&self) -> bool { true }
    fn supports_share_links(&self) -> bool { true }
    fn supports_versions(&self) -> bool { false }
}

// ─── Jottacloud-specific methods (trash management) ──────────────────────

impl JottacloudProvider {
    /// Build JFS URL for the trash: /jfs/{username}/Trash/{path}
    fn trash_url(&self, path: &str) -> String {
        let clean = path.trim_start_matches('/');
        if clean.is_empty() {
            format!("{}/{}/Trash", JFS_BASE, self.username)
        } else {
            format!("{}/{}/Trash/{}", JFS_BASE, self.username, clean)
        }
    }

    /// Move a file/folder to Jottacloud Trash (soft delete).
    /// POST /jfs/{...}/{path}?dl=true (file) or ?dlDir=true (directory)
    pub async fn move_to_trash(&mut self, path: &str) -> Result<(), ProviderError> {
        let resolved = Self::normalize_path(path);
        let url = format!("{}?dl=true", self.jfs_url(&resolved));
        jotta_log(&format!("Moving to trash: {}", resolved));

        let resp = self.post_with_retry(&url, "application/octet-stream", vec![]).await?;

        if !resp.status().is_success() {
            // Try directory soft delete
            let url_dir = format!("{}?dlDir=true", self.jfs_url(&resolved));
            let resp_dir = self.post_with_retry(&url_dir, "application/octet-stream", vec![]).await?;
            if !resp_dir.status().is_success() {
                let status = resp_dir.status();
                let body = resp_dir.text().await.unwrap_or_default();
                return Err(ProviderError::ServerError(format!(
                    "Move to trash {} failed ({}): {}", resolved, status, sanitize_api_error(&body)
                )));
            }
        }

        jotta_log(&format!("Moved to trash: {}", resolved));
        Ok(())
    }

    /// List items in Jottacloud Trash.
    /// Trash is at /jfs/{username}/Trash (device-less, mountpoint-less).
    pub async fn list_trash(&mut self) -> Result<Vec<RemoteEntry>, ProviderError> {
        let url = self.trash_url("");
        jotta_log(&format!("Listing trash: {}", url));

        let resp = self.get_with_retry(&url).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            if status.as_u16() == 404 {
                return Ok(Vec::new()); // Empty trash
            }
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "List trash failed ({}): {}", status, sanitize_api_error(&body)
            )));
        }

        let xml = resp.text().await.unwrap_or_default();
        jotta_log(&format!("Trash XML ({} bytes): {}", xml.len(), &xml[..xml.len().min(2000)]));

        // Parse trash listing — include ALL items (even "deleted" ones, since they ARE trash)
        let entries = Self::parse_trash_xml(&xml);
        jotta_log(&format!("Trash: {} items", entries.len()));
        Ok(entries)
    }

    /// Restore an item from trash to its original location.
    /// POST /jfs/{username}/Trash/{path}?restore=true
    pub async fn restore_from_trash(&mut self, path: &str) -> Result<(), ProviderError> {
        let clean = path.trim_start_matches('/');
        let url = format!("{}?restore=true", self.trash_url(clean));
        jotta_log(&format!("Restoring from trash: {}", url));

        let resp = self.post_with_retry(&url, "application/octet-stream", vec![]).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Restore from trash failed ({}): {}", status, sanitize_api_error(&body)
            )));
        }

        jotta_log(&format!("Restored from trash: {}", clean));
        Ok(())
    }

    /// Permanently delete an item from trash.
    /// POST /jfs/{username}/Trash/{path}?rm=true
    pub async fn permanent_delete_from_trash(&mut self, path: &str) -> Result<(), ProviderError> {
        let clean = path.trim_start_matches('/');
        let url = format!("{}?rm=true", self.trash_url(clean));
        jotta_log(&format!("Permanent delete from trash: {}", url));

        let resp = self.post_with_retry(&url, "application/octet-stream", vec![]).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Permanent delete failed ({}): {}", status, sanitize_api_error(&body)
            )));
        }

        jotta_log(&format!("Permanently deleted from trash: {}", clean));
        Ok(())
    }

    /// Whether this provider supports trash operations.
    #[allow(dead_code)]
    pub fn supports_trash(&self) -> bool { true }

    /// Parse trash XML listing. Unlike regular listing, we include all items
    /// regardless of deleted status (they ARE trash items).
    fn parse_trash_xml(xml: &str) -> Vec<RemoteEntry> {
        use quick_xml::events::Event;
        use quick_xml::Reader;

        let mut entries = Vec::new();
        let mut reader = Reader::from_str(xml);
        reader.config_mut().trim_text(true);
        let mut buf = Vec::new();

        let mut depth: u32 = 0;
        let mut root_depth: Option<u32> = None;
        let mut in_folders_section = false;
        let mut folders_section_depth: u32 = 0;
        let mut child_folder_depth: Option<u32> = None;

        let mut in_file = false;
        let mut in_revision = false;
        let mut current_name = String::new();
        let mut current_size: u64 = 0;
        let mut current_modified = String::new();
        let mut current_state = String::new();
        let mut current_tag = String::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => {
                    let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                    depth += 1;

                    match tag.as_str() {
                        "folder" | "mountPoint" | "trashcan" if root_depth.is_none() => {
                            root_depth = Some(depth);
                        }
                        "folders" if root_depth == Some(depth - 1) && child_folder_depth.is_none() => {
                            in_folders_section = true;
                            folders_section_depth = depth;
                        }
                        "folder" if child_folder_depth.is_none() && (
                            in_folders_section ||
                            (root_depth.is_some() && depth == root_depth.unwrap() + 1)
                        ) => {
                            let mut name = String::new();
                            for attr in e.attributes().flatten() {
                                if attr.key.as_ref() == b"name" {
                                    name = String::from_utf8_lossy(&attr.value).to_string();
                                }
                            }
                            if !name.is_empty() {
                                entries.push(RemoteEntry {
                                    name: name.clone(),
                                    path: format!("/{}", name),
                                    is_dir: true,
                                    size: 0,
                                    modified: None,
                                    permissions: None,
                                    owner: None,
                                    group: None,
                                    is_symlink: false,
                                    link_target: None,
                                    metadata: HashMap::new(),
                                    mime_type: None,
                                });
                            }
                            child_folder_depth = Some(depth);
                        }
                        "file" if !in_file && child_folder_depth.is_none() => {
                            in_file = true;
                            current_name.clear();
                            current_size = 0;
                            current_modified.clear();
                            current_state.clear();
                            for attr in e.attributes().flatten() {
                                if attr.key.as_ref() == b"name" {
                                    current_name = String::from_utf8_lossy(&attr.value).to_string();
                                }
                            }
                        }
                        "currentRevision" if in_file => { in_revision = true; }
                        _ => { current_tag = tag; }
                    }
                }
                Ok(Event::Empty(ref e)) => {
                    let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                    if tag == "folder" && child_folder_depth.is_none() && (
                        in_folders_section ||
                        (root_depth.is_some() && depth == root_depth.unwrap())
                    ) {
                        let mut name = String::new();
                        for attr in e.attributes().flatten() {
                            if attr.key.as_ref() == b"name" {
                                name = String::from_utf8_lossy(&attr.value).to_string();
                            }
                        }
                        if !name.is_empty() {
                            entries.push(RemoteEntry {
                                name: name.clone(),
                                path: format!("/{}", name),
                                is_dir: true,
                                size: 0,
                                modified: None,
                                permissions: None,
                                owner: None,
                                group: None,
                                is_symlink: false,
                                link_target: None,
                                metadata: HashMap::new(),
                                mime_type: None,
                            });
                        }
                    }
                }
                Ok(Event::End(ref e)) => {
                    let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                    match tag.as_str() {
                        "folders" if in_folders_section && depth == folders_section_depth => {
                            in_folders_section = false;
                        }
                        "folder" if child_folder_depth == Some(depth) => {
                            child_folder_depth = None;
                        }
                        "file" if in_file => {
                            in_file = false;
                            in_revision = false;
                            // In trash, show all files (not just COMPLETED)
                            if !current_name.is_empty() {
                                entries.push(RemoteEntry {
                                    name: current_name.clone(),
                                    path: format!("/{}", current_name),
                                    is_dir: false,
                                    size: current_size,
                                    modified: if current_modified.is_empty() { None } else { Some(current_modified.clone()) },
                                    permissions: None,
                                    owner: None,
                                    group: None,
                                    is_symlink: false,
                                    link_target: None,
                                    metadata: HashMap::new(),
                                    mime_type: None,
                                });
                            }
                        }
                        "currentRevision" => { in_revision = false; }
                        _ => {}
                    }
                    depth = depth.saturating_sub(1);
                    current_tag.clear();
                }
                Ok(Event::Text(ref e)) => {
                    let text = String::from_utf8_lossy(e.as_ref()).trim().to_string();
                    if in_revision && in_file {
                        match current_tag.as_str() {
                            "size" => { current_size = text.parse().unwrap_or(0); }
                            "state" => { current_state = text; }
                            "modified" | "updated" => {
                                if current_modified.is_empty() {
                                    current_modified = Self::parse_jotta_time(&text);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                Ok(Event::Eof) => break,
                Err(_) => break,
                _ => {}
            }
            buf.clear();
        }

        entries
    }
}
