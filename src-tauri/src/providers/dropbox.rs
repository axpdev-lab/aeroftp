//! Dropbox Storage Provider
//!
//! Implements StorageProvider for Dropbox using the Dropbox API v2.
//! Uses OAuth2 for authentication.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use async_trait::async_trait;
use reqwest::header::{HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::Deserialize;
use std::collections::HashMap;
use tracing::info;

use super::{
    oauth2::{OAuth2Manager, OAuthConfig, OAuthProvider},
    sanitize_api_error, LockInfo, MultipartHandle, ProviderConfig, ProviderError, ProviderType,
    RemoteEntry, ShareLinkCapabilities, ShareLinkInfo, ShareLinkOptions, ShareLinkResult,
    StorageInfo, StorageProvider, UploadedPart,
};

/// Dropbox API endpoints
const API_BASE: &str = "https://api.dropboxapi.com/2";
const CONTENT_BASE: &str = "https://content.dropboxapi.com/2";

/// Threshold above which legacy `upload()` switches to the upload session
/// path. The trait runner's multipart graph is gated on the same boundary
/// via `multipart_threshold` so single small files keep the legacy single-PUT
/// path and only large files fan out into `UploadPart` nodes.
const DROPBOX_SESSION_THRESHOLD: u64 = 150 * 1024 * 1024;

/// Trait-side chunk size. Aligns with the legacy 128 MiB but smaller so the
/// runner emits a larger number of `UploadPart` nodes the AIMD controller can
/// actually schedule in parallel. 8 MiB stays well under Dropbox's 150 MiB
/// per-chunk cap.
const DROPBOX_MULTIPART_PART_SIZE: u64 = 8 * 1024 * 1024;

/// Opaque metadata threaded through `MultipartHandle.upload_id` for the
/// Dropbox concurrent-session multipart trait. Carries the session id plus
/// `total_size` / `part_size` (so `upload_part` rebuilds the byte offset
/// from `part_number` alone) and the normalised destination path needed by
/// the `upload_session/finish` commit body.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
struct DropboxMultipartMeta {
    session_id: String,
    total: u64,
    part: u64,
    path: String,
}

impl DropboxMultipartMeta {
    fn encode(&self) -> String {
        serde_json::to_string(self).expect("DropboxMultipartMeta is always serializable")
    }

    fn decode(s: &str) -> Result<Self, ProviderError> {
        serde_json::from_str(s)
            .map_err(|e| ProviderError::Other(format!("Invalid Dropbox multipart handle: {}", e)))
    }
}

/// The runner uses `preferred_chunk_size` verbatim as the per-part byte
/// length, so the handle simply mirrors the advertised value. Dropbox's
/// concurrent session accepts any chunk size from 1 byte up to 150 MiB,
/// so the 8 MiB hint is the only contract we need to enforce here.
fn dropbox_runner_part_size(total_size: u64) -> u64 {
    if total_size == 0 {
        return 0;
    }
    DROPBOX_MULTIPART_PART_SIZE
}

/// T-DEBT-05 S1-T02c: Dropbox throttles by returning 429 with the hint in
/// the JSON body's `retry_after` field (float seconds). The `Retry-After`
/// header is sometimes present too; we prefer the body field as the
/// authoritative source per Dropbox API docs and fall back to the header
/// when the body cannot be parsed.
fn dropbox_is_rate_limited(status: u16, body: &str) -> bool {
    if status == 429 {
        return true;
    }
    // Dropbox occasionally surfaces throttles as 409 with an explicit error
    // tag. The tag is the authoritative marker.
    body.contains("too_many_requests") || body.contains("too_many_write_operations")
}

/// Compute the marker substring for a Dropbox rate-limit response. Prefers
/// the JSON body's top-level `retry_after` field; falls back to the
/// `Retry-After` header.
fn dropbox_retry_marker_tail(
    status: u16,
    body: &str,
    retry_header: Option<&str>,
) -> Option<String> {
    if !dropbox_is_rate_limited(status, body) {
        return None;
    }
    let hint = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|json| super::retry_after::parse_retry_after_dropbox_value(&json))
        .or_else(|| {
            retry_header
                .and_then(super::retry_after::parse_retry_after_seconds)
        })?;
    Some(crate::transfer_dag::adaptive::embed_retry_after_marker(
        hint.as_secs(),
    ))
}


/// Dropbox file metadata
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct DropboxMetadata {
    #[serde(rename = ".tag")]
    tag: String,
    name: String,
    path_lower: Option<String>,
    path_display: Option<String>,
    id: Option<String>,
    rev: Option<String>,
    #[serde(default)]
    size: u64,
    client_modified: Option<String>,
    server_modified: Option<String>,
    /// Dropbox content hash: SHA-256 over the SHA-256 of each 4 MiB
    /// block, then concatenated. This is NOT a plain file SHA-256, so
    /// it is exposed under a dedicated `dropbox` key and never aliased
    /// to `sha256`.
    #[serde(default)]
    content_hash: Option<String>,
}

/// List folder response
#[derive(Debug, Deserialize)]
struct ListFolderResult {
    entries: Vec<DropboxMetadata>,
    cursor: String,
    has_more: bool,
}

/// Dropbox provider configuration
#[derive(Debug, Clone)]
pub struct DropboxConfig {
    pub app_key: String,
    pub app_secret: String,
}

impl DropboxConfig {
    pub fn new(app_key: &str, app_secret: &str) -> Self {
        Self {
            app_key: app_key.to_string(),
            app_secret: app_secret.to_string(),
        }
    }

    #[allow(dead_code)]
    pub fn from_provider_config(config: &ProviderConfig) -> Result<Self, ProviderError> {
        let app_key = config
            .extra
            .get("app_key")
            .or_else(|| config.extra.get("client_id"))
            .ok_or_else(|| ProviderError::Other("Missing app_key".to_string()))?;
        let app_secret = config
            .extra
            .get("app_secret")
            .or_else(|| config.extra.get("client_secret"))
            .ok_or_else(|| ProviderError::Other("Missing app_secret".to_string()))?;

        Ok(Self::new(app_key, app_secret))
    }
}

/// Dropbox Storage Provider
pub struct DropboxProvider {
    config: DropboxConfig,
    oauth_manager: OAuth2Manager,
    client: reqwest::Client,
    connected: bool,
    current_path: String,
    /// Authenticated user email
    account_email: Option<String>,
    /// Server profile identifier owning these OAuth tokens. Empty when the
    /// caller has not bound a profile (legacy singleton key path). Issue #214.
    profile_id: String,
}

impl DropboxProvider {
    pub fn new(config: DropboxConfig) -> Self {
        Self {
            config,
            oauth_manager: OAuth2Manager::new(),
            client: reqwest::Client::builder()
                .user_agent(crate::providers::AEROFTP_USER_AGENT)
                .connect_timeout(std::time::Duration::from_secs(30))
                .read_timeout(std::time::Duration::from_secs(1800))
                .build()
                .unwrap_or_default(),
            connected: false,
            current_path: "".to_string(), // Dropbox root is ""
            account_email: None,
            profile_id: String::new(),
        }
    }

    /// Bind this provider to a server profile so OAuth tokens are stored
    /// under the per-profile vault key. Issue #214.
    pub fn with_profile_id(mut self, profile_id: impl Into<String>) -> Self {
        self.profile_id = profile_id.into();
        self
    }

    /// Get OAuth config
    fn oauth_config(&self) -> OAuthConfig {
        OAuthConfig::dropbox(&self.config.app_key, &self.config.app_secret)
            .with_profile_id(&self.profile_id)
    }

    /// Get authorization header
    async fn auth_header(&self) -> Result<HeaderValue, ProviderError> {
        use secrecy::ExposeSecret;
        let token = self
            .oauth_manager
            .get_valid_token(&self.oauth_config())
            .await?;
        HeaderValue::from_str(&format!("Bearer {}", token.expose_secret()))
            .map_err(|e| ProviderError::Other(format!("Invalid token: {}", e)))
    }

    /// Check if authenticated
    pub fn is_authenticated(&self) -> bool {
        self.oauth_manager
            .has_tokens(OAuthProvider::Dropbox, &self.profile_id)
    }

    /// Start OAuth flow (called via oauth2_start_auth command)
    #[allow(dead_code)]
    pub async fn start_auth(&self) -> Result<(String, String), ProviderError> {
        self.oauth_manager
            .start_auth_flow(&self.oauth_config())
            .await
    }

    /// Complete OAuth flow (called via oauth2_connect command)
    #[allow(dead_code)]
    pub async fn complete_auth(&self, code: &str, state: &str) -> Result<(), ProviderError> {
        self.oauth_manager
            .complete_auth_flow(&self.oauth_config(), code, state)
            .await?;
        Ok(())
    }

    /// Normalize path for Dropbox API (empty string = root, paths start with /)
    fn normalize_path(&self, path: &str) -> String {
        let path = path.trim_matches('/');
        if path.is_empty() {
            "".to_string()
        } else {
            format!("/{}", path)
        }
    }

    /// Convert Dropbox metadata to RemoteEntry
    fn to_remote_entry(&self, meta: &DropboxMetadata) -> RemoteEntry {
        let is_dir = meta.tag == "folder";
        let path = meta
            .path_display
            .clone()
            .unwrap_or_else(|| meta.path_lower.clone().unwrap_or_default());

        let mut metadata = HashMap::new();
        if let Some(ref rev) = meta.rev {
            metadata.insert("rev".to_string(), rev.clone());
        }
        if !is_dir {
            if let Some(ref h) = meta.content_hash {
                metadata.insert("dropbox".to_string(), h.to_ascii_lowercase());
            }
        }

        RemoteEntry {
            name: meta.name.clone(),
            path,
            is_dir,
            size: meta.size,
            modified: meta.server_modified.clone(),
            permissions: None,
            owner: None,
            group: None,
            is_symlink: false,
            link_target: None,
            mime_type: None,
            metadata,
        }
    }

    /// Make API call with RPC style
    async fn rpc_call<T: serde::de::DeserializeOwned>(
        &self,
        endpoint: &str,
        body: &serde_json::Value,
    ) -> Result<T, ProviderError> {
        let url = format!("{}/{}", API_BASE, endpoint);

        let response = self
            .client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let retry_header = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .map(String::from);
            let text = response.text().await.unwrap_or_default();

            // Check for path not found error
            if text.contains("path/not_found") {
                return Err(ProviderError::NotFound(sanitize_api_error(&text)));
            }

            let mut msg = format!("API error {}: {}", status, sanitize_api_error(&text));
            if let Some(tail) =
                dropbox_retry_marker_tail(status.as_u16(), &text, retry_header.as_deref())
            {
                msg.push_str(&tail);
            }
            return Err(ProviderError::Other(msg));
        }

        response
            .json()
            .await
            .map_err(|e| ProviderError::Other(format!("Parse error: {}", e)))
    }

    /// List deleted files in a folder (includes deleted entries)
    #[allow(dead_code)]
    pub async fn list_deleted(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let norm_path = self.normalize_path(path);

        let body = serde_json::json!({
            "path": norm_path,
            "recursive": false,
            "include_deleted": true,
            "include_has_explicit_shared_members": false,
            "include_mounted_folders": true
        });

        let mut result: ListFolderResult = self.rpc_call("files/list_folder", &body).await?;
        let mut all_entries = result.entries;

        while result.has_more {
            let continue_body = serde_json::json!({
                "cursor": result.cursor
            });
            result = self
                .rpc_call("files/list_folder/continue", &continue_body)
                .await?;
            all_entries.extend(result.entries);
        }

        // Filter to only deleted entries
        let deleted: Vec<RemoteEntry> = all_entries
            .iter()
            .filter(|e| e.tag == "deleted")
            .map(|e| self.to_remote_entry(e))
            .collect();

        info!("Listed {} deleted entries in {}", deleted.len(), path);
        Ok(deleted)
    }

    /// Restore a deleted file. If `rev` is empty, automatically fetches
    /// the latest revision via `files/list_revisions` before restoring.
    pub async fn restore_file(&mut self, path: &str, rev: &str) -> Result<(), ProviderError> {
        let full_path = if path.starts_with('/') {
            self.normalize_path(path)
        } else {
            self.normalize_path(&format!("{}/{}", self.current_path, path))
        };

        let effective_rev = if rev.is_empty() {
            // Fetch latest revision for deleted file
            let rev_body = serde_json::json!({
                "path": full_path,
                "mode": "path",
                "limit": 1
            });
            let rev_result: serde_json::Value =
                self.rpc_call("files/list_revisions", &rev_body).await?;
            rev_result
                .get("entries")
                .and_then(|e| e.as_array())
                .and_then(|a| a.first())
                .and_then(|e| e.get("rev"))
                .and_then(|r| r.as_str())
                .map(|s| s.to_string())
                .ok_or_else(|| {
                    ProviderError::NotFound("No revision found for deleted file".into())
                })?
        } else {
            rev.to_string()
        };

        let body = serde_json::json!({
            "path": full_path,
            "rev": effective_rev
        });

        let _: serde_json::Value = self.rpc_call("files/restore", &body).await?;

        info!("Restored {} to revision {}", path, effective_rev);
        Ok(())
    }

    /// Permanently delete a file (cannot be undone)
    #[allow(dead_code)]
    pub async fn permanent_delete(&mut self, path: &str) -> Result<(), ProviderError> {
        let full_path = if path.starts_with('/') {
            self.normalize_path(path)
        } else {
            self.normalize_path(&format!("{}/{}", self.current_path, path))
        };

        let body = serde_json::json!({
            "path": full_path
        });

        let url = format!("{}/files/permanently_delete", API_BASE);

        let response = self
            .client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let retry_header = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .map(String::from);
            let text = response.text().await.unwrap_or_default();
            let sanitized = sanitize_api_error(&text);
            // Surface the missing-scope case explicitly: tokens issued before
            // we added `files.permanent_delete` to the OAuth scope set will
            // fail here. The user has to disconnect and reconnect once to
            // refresh permissions.
            if text.contains("missing_scope") && text.contains("files.permanent_delete") {
                return Err(ProviderError::Other(
                    "Dropbox token missing 'files.permanent_delete' scope. \
                     Disconnect and reconnect Dropbox once to refresh \
                     permissions, then retry."
                        .to_string(),
                ));
            }
            let mut msg = format!("Permanent delete failed {}: {}", status, sanitized);
            if let Some(tail) =
                dropbox_retry_marker_tail(status.as_u16(), &text, retry_header.as_deref())
            {
                msg.push_str(&tail);
            }
            return Err(ProviderError::Other(msg));
        }

        info!("Permanently deleted: {}", path);
        Ok(())
    }

    /// Get tags for a file
    pub async fn get_tags(
        &mut self,
        paths: &[String],
    ) -> Result<Vec<(String, Vec<String>)>, ProviderError> {
        let entries: Vec<String> = paths
            .iter()
            .map(|p| {
                if p.starts_with('/') {
                    self.normalize_path(p)
                } else {
                    self.normalize_path(&format!("{}/{}", self.current_path, p))
                }
            })
            .collect();

        let body = serde_json::json!({ "paths": entries });
        let url = format!("{}/files/tags/get", API_BASE);

        let response = self
            .client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!(
                "Get tags failed: {}",
                sanitize_api_error(&text)
            )));
        }

        #[derive(Deserialize)]
        struct TagValue {
            #[serde(default)]
            tag_text: Option<String>,
        }
        #[derive(Deserialize)]
        struct PathTag {
            path: String,
            #[serde(default)]
            tags: Vec<TagValue>,
        }
        #[derive(Deserialize)]
        struct PathsToTags {
            paths_to_tags: Vec<PathTag>,
        }

        let result: PathsToTags = response
            .json()
            .await
            .map_err(|e| ProviderError::Other(format!("Parse error: {}", e)))?;

        Ok(result
            .paths_to_tags
            .iter()
            .map(|pt| {
                let tags: Vec<String> = pt.tags.iter().filter_map(|t| t.tag_text.clone()).collect();
                (pt.path.clone(), tags)
            })
            .collect())
    }

    /// Add a tag to a file
    pub async fn add_tag(&mut self, path: &str, tag: &str) -> Result<(), ProviderError> {
        let full_path = if path.starts_with('/') {
            self.normalize_path(path)
        } else {
            self.normalize_path(&format!("{}/{}", self.current_path, path))
        };

        let body = serde_json::json!({
            "path": full_path,
            "tag_text": tag.to_lowercase()
        });

        let url = format!("{}/files/tags/add", API_BASE);

        let response = self
            .client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!(
                "Add tag failed: {}",
                sanitize_api_error(&text)
            )));
        }

        info!("Added tag '{}' to: {}", tag, path);
        Ok(())
    }

    /// Remove a tag from a file
    pub async fn remove_tag(&mut self, path: &str, tag: &str) -> Result<(), ProviderError> {
        let full_path = if path.starts_with('/') {
            self.normalize_path(path)
        } else {
            self.normalize_path(&format!("{}/{}", self.current_path, path))
        };

        let body = serde_json::json!({
            "path": full_path,
            "tag_text": tag.to_lowercase()
        });

        let url = format!("{}/files/tags/remove", API_BASE);

        let response = self
            .client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!(
                "Remove tag failed: {}",
                sanitize_api_error(&text)
            )));
        }

        info!("Removed tag '{}' from: {}", tag, path);
        Ok(())
    }

    /// Set all tags on a file (replaces existing)
    pub async fn set_tags(&mut self, path: &str, tags: &[String]) -> Result<(), ProviderError> {
        let full_path = if path.starts_with('/') {
            self.normalize_path(path)
        } else {
            self.normalize_path(&format!("{}/{}", self.current_path, path))
        };

        // Get current tags
        let current = self.get_tags(std::slice::from_ref(&full_path)).await?;
        let current_tags: Vec<String> = current
            .first()
            .map(|(_, tags)| tags.clone())
            .unwrap_or_default();

        // Remove tags not in new set
        for old_tag in &current_tags {
            if !tags
                .iter()
                .any(|t| t.to_lowercase() == old_tag.to_lowercase())
            {
                self.remove_tag(&full_path, old_tag).await?;
            }
        }

        // Add new tags not in current set
        for new_tag in tags {
            if !current_tags
                .iter()
                .any(|t| t.to_lowercase() == new_tag.to_lowercase())
            {
                self.add_tag(&full_path, new_tag).await?;
            }
        }

        Ok(())
    }

    /// List folder with pagination
    async fn list_folder_all(&self, path: &str) -> Result<Vec<DropboxMetadata>, ProviderError> {
        let path = self.normalize_path(path);

        let body = serde_json::json!({
            "path": path,
            "recursive": false,
            "include_deleted": false,
            "include_has_explicit_shared_members": false,
            "include_mounted_folders": true
        });

        let mut result: ListFolderResult = self.rpc_call("files/list_folder", &body).await?;
        let mut all_entries = result.entries;

        while result.has_more {
            let continue_body = serde_json::json!({
                "cursor": result.cursor
            });
            result = self
                .rpc_call("files/list_folder/continue", &continue_body)
                .await?;
            all_entries.extend(result.entries);
        }

        Ok(all_entries)
    }
}

#[async_trait]
impl StorageProvider for DropboxProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn provider_type(&self) -> ProviderType {
        ProviderType::Dropbox
    }

    fn display_name(&self) -> String {
        "Dropbox".to_string()
    }

    fn account_email(&self) -> Option<String> {
        self.account_email.clone()
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        if !self.is_authenticated() {
            return Err(ProviderError::AuthenticationFailed(
                "Not authenticated. Call start_auth() first.".to_string(),
            ));
        }

        // Validate by getting account info
        let url = format!("{}/users/get_current_account", API_BASE);

        let response = self
            .client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            info!("Dropbox auth failed: HTTP {} - {}", status, body);
            return Err(ProviderError::AuthenticationFailed(format!(
                "Invalid token (HTTP {})",
                status
            )));
        }

        // Parse email from response
        if let Ok(body) = response.json::<serde_json::Value>().await {
            if let Some(email) = body["email"].as_str() {
                self.account_email = Some(email.to_string());
            }
        }

        self.connected = true;
        self.current_path = "".to_string();

        info!("Connected to Dropbox");
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.connected = false;
        info!("Disconnected from Dropbox");
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let list_path = if path == "." || path.is_empty() {
            self.current_path.clone()
        } else if path.starts_with('/') {
            path.trim_matches('/').to_string()
        } else {
            format!("{}/{}", self.current_path.trim_matches('/'), path)
        };

        let entries = self.list_folder_all(&list_path).await?;
        Ok(entries.iter().map(|e| self.to_remote_entry(e)).collect())
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> {
        if self.current_path.is_empty() {
            Ok("/".to_string())
        } else {
            Ok(format!("/{}", self.current_path))
        }
    }

    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        let new_path = if path.starts_with('/') {
            path.trim_matches('/').to_string()
        } else if path == ".." {
            let mut parts: Vec<&str> = self
                .current_path
                .split('/')
                .filter(|s| !s.is_empty())
                .collect();
            parts.pop();
            parts.join("/")
        } else if self.current_path.is_empty() {
            path.to_string()
        } else {
            format!("{}/{}", self.current_path, path)
        };

        // Verify path exists (skip for root)
        if !new_path.is_empty() {
            let db_path = self.normalize_path(&new_path);
            let body = serde_json::json!({
                "path": db_path
            });

            let meta: DropboxMetadata = self.rpc_call("files/get_metadata", &body).await?;
            if meta.tag != "folder" {
                return Err(ProviderError::InvalidPath(format!(
                    "{} is not a folder",
                    path
                )));
            }
        }

        self.current_path = new_path;
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
        use futures_util::StreamExt;

        let path = self.normalize_path(remote_path);

        let arg = serde_json::json!({
            "path": path
        });

        let url = format!("{}/files/download", CONTENT_BASE);

        let response = self
            .client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header("Dropbox-API-Arg", arg.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            if text.contains("path/not_found") {
                return Err(ProviderError::NotFound(sanitize_api_error(&text)));
            }
            return Err(ProviderError::Other(format!(
                "Download failed: {}",
                sanitize_api_error(&text)
            )));
        }

        let mut stream = response.bytes_stream();
        let mut atomic = super::atomic_write::AtomicFile::new(local_path)
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
            atomic
                .write_all(&chunk)
                .await
                .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
        }
        atomic.commit().await.map_err(|e| {
            ProviderError::TransferFailed(format!("Failed to finalize download: {}", e))
        })?;

        info!("Downloaded {} to {}", remote_path, local_path);
        Ok(())
    }

    fn supports_resume(&self) -> bool {
        true
    }

    async fn resume_download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        _offset: u64,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let path = self.normalize_path(remote_path);
        let arg = serde_json::json!({ "path": path });
        let url = format!("{}/files/download", CONTENT_BASE);
        let auth = self.auth_header().await?;
        let arg_str = arg.to_string();

        super::http_resumable_download(
            local_path,
            |range_header| {
                let mut req = self
                    .client
                    .post(&url)
                    .header(AUTHORIZATION, auth.clone())
                    .header("Dropbox-API-Arg", &arg_str);
                if let Some(range) = range_header {
                    req = req.header("Range", range);
                }
                req
            },
            on_progress,
        )
        .await
    }

    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        let path = self.normalize_path(remote_path);

        let arg = serde_json::json!({
            "path": path
        });

        let url = format!("{}/files/download", CONTENT_BASE);

        let response = self
            .client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header("Dropbox-API-Arg", arg.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            if text.contains("path/not_found") {
                return Err(ProviderError::NotFound(sanitize_api_error(&text)));
            }
            return Err(ProviderError::Other(format!(
                "Download failed: {}",
                sanitize_api_error(&text)
            )));
        }

        // H2: Size-limited download to prevent OOM on large files
        super::response_bytes_with_limit(response, super::MAX_DOWNLOAD_TO_BYTES).await
    }

    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        use tokio::io::AsyncReadExt;

        let path = self.normalize_path(remote_path);
        let file_size = tokio::fs::metadata(local_path)
            .await
            .map_err(|e| ProviderError::Other(format!("Metadata error: {}", e)))?
            .len();

        const UPLOAD_SESSION_THRESHOLD: u64 = 150 * 1024 * 1024; // 150MB

        if file_size > UPLOAD_SESSION_THRESHOLD {
            // Upload session for large files: read chunks from file, not all in memory
            const CHUNK_SIZE: u64 = 128 * 1024 * 1024; // 128MB
            let mut file = tokio::fs::File::open(local_path)
                .await
                .map_err(|e| ProviderError::Other(format!("Open error: {}", e)))?;

            // Step 1: Start upload session with first chunk
            let first_chunk_size = std::cmp::min(CHUNK_SIZE, file_size) as usize;
            let mut first_chunk = vec![0u8; first_chunk_size];
            file.read_exact(&mut first_chunk)
                .await
                .map_err(|e| ProviderError::Other(format!("Read error: {}", e)))?;

            let start_url = format!("{}/files/upload_session/start", CONTENT_BASE);
            let start_arg = serde_json::json!({
                "close": first_chunk_size as u64 >= file_size
            });

            let start_resp = self
                .client
                .post(&start_url)
                .header(AUTHORIZATION, self.auth_header().await?)
                .header(CONTENT_TYPE, "application/octet-stream")
                .header("Dropbox-API-Arg", start_arg.to_string())
                .body(first_chunk)
                .send()
                .await
                .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

            if !start_resp.status().is_success() {
                let text = start_resp.text().await.unwrap_or_default();
                return Err(ProviderError::Other(format!(
                    "Upload session start failed: {}",
                    sanitize_api_error(&text)
                )));
            }

            #[derive(Deserialize)]
            struct SessionStart {
                session_id: String,
            }
            let session: SessionStart = start_resp
                .json()
                .await
                .map_err(|e| ProviderError::Other(format!("Parse error: {}", e)))?;

            if let Some(ref progress) = on_progress {
                progress(first_chunk_size as u64, file_size);
            }

            // Step 2: Append remaining chunks (read from file, not memory)
            let mut offset = first_chunk_size as u64;
            while offset < file_size {
                let chunk_size = std::cmp::min(CHUNK_SIZE, file_size - offset) as usize;
                let mut chunk = vec![0u8; chunk_size];
                file.read_exact(&mut chunk)
                    .await
                    .map_err(|e| ProviderError::Other(format!("Read error: {}", e)))?;
                let is_last = offset + chunk_size as u64 >= file_size;

                let append_url = format!("{}/files/upload_session/append_v2", CONTENT_BASE);
                let append_arg = serde_json::json!({
                    "cursor": {
                        "session_id": session.session_id,
                        "offset": offset
                    },
                    "close": is_last
                });

                let append_resp = self
                    .client
                    .post(&append_url)
                    .header(AUTHORIZATION, self.auth_header().await?)
                    .header(CONTENT_TYPE, "application/octet-stream")
                    .header("Dropbox-API-Arg", append_arg.to_string())
                    .body(chunk)
                    .send()
                    .await
                    .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

                if !append_resp.status().is_success() {
                    let text = append_resp.text().await.unwrap_or_default();
                    return Err(ProviderError::Other(format!(
                        "Upload session append failed: {}",
                        sanitize_api_error(&text)
                    )));
                }

                offset += chunk_size as u64;

                if let Some(ref progress) = on_progress {
                    progress(offset, file_size);
                }
            }

            // Step 3: Finish session
            let finish_url = format!("{}/files/upload_session/finish", CONTENT_BASE);
            let finish_arg = serde_json::json!({
                "cursor": {
                    "session_id": session.session_id,
                    "offset": file_size
                },
                "commit": {
                    "path": path,
                    "mode": "overwrite",
                    "autorename": false,
                    "mute": false
                }
            });

            let finish_resp = self
                .client
                .post(&finish_url)
                .header(AUTHORIZATION, self.auth_header().await?)
                .header(CONTENT_TYPE, "application/octet-stream")
                .header("Dropbox-API-Arg", finish_arg.to_string())
                .send()
                .await
                .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

            if !finish_resp.status().is_success() {
                let text = finish_resp.text().await.unwrap_or_default();
                return Err(ProviderError::Other(format!(
                    "Upload session finish failed: {}",
                    sanitize_api_error(&text)
                )));
            }
        } else {
            // Simple upload: stream file content without loading into memory
            let file = tokio::fs::File::open(local_path)
                .await
                .map_err(|e| ProviderError::Other(format!("Open error: {}", e)))?;
            let stream = tokio_util::io::ReaderStream::new(file);
            let body = reqwest::Body::wrap_stream(stream);

            let arg = serde_json::json!({
                "path": path,
                "mode": "overwrite",
                "autorename": false,
                "mute": false
            });

            let url = format!("{}/files/upload", CONTENT_BASE);

            let response = self
                .client
                .post(&url)
                .header(AUTHORIZATION, self.auth_header().await?)
                .header(CONTENT_TYPE, "application/octet-stream")
                .header("Dropbox-API-Arg", arg.to_string())
                .header("Content-Length", file_size.to_string())
                .body(body)
                .send()
                .await
                .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

            if !response.status().is_success() {
                let text = response.text().await.unwrap_or_default();
                return Err(ProviderError::Other(format!(
                    "Upload failed: {}",
                    sanitize_api_error(&text)
                )));
            }
        }

        info!("Uploaded {} to {}", local_path, remote_path);
        Ok(())
    }

    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let full_path = if path.starts_with('/') {
            self.normalize_path(path)
        } else {
            self.normalize_path(&format!("{}/{}", self.current_path, path))
        };

        let body = serde_json::json!({
            "path": full_path,
            "autorename": false
        });

        let _: serde_json::Value = self.rpc_call("files/create_folder_v2", &body).await?;

        info!("Created folder: {}", path);
        Ok(())
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        let full_path = if path.starts_with('/') {
            self.normalize_path(path)
        } else {
            self.normalize_path(&format!("{}/{}", self.current_path, path))
        };

        let body = serde_json::json!({
            "path": full_path
        });

        let _: serde_json::Value = self.rpc_call("files/delete_v2", &body).await?;

        info!("Deleted: {}", path);
        Ok(())
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        self.delete(path).await
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        // Dropbox delete removes folders with contents
        self.delete(path).await
    }

    async fn delete_permanent(&mut self, path: &str) -> Result<bool, ProviderError> {
        self.permanent_delete(path).await.map(|_| true)
    }

    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let from_path = if from.starts_with('/') {
            self.normalize_path(from)
        } else {
            self.normalize_path(&format!("{}/{}", self.current_path, from))
        };

        let to_path = if to.starts_with('/') {
            self.normalize_path(to)
        } else {
            self.normalize_path(&format!("{}/{}", self.current_path, to))
        };

        let body = serde_json::json!({
            "from_path": from_path,
            "to_path": to_path
        });

        let _: serde_json::Value = self.rpc_call("files/move_v2", &body).await?;

        info!("Renamed {} to {}", from, to);
        Ok(())
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        let full_path = if path.starts_with('/') {
            self.normalize_path(path)
        } else {
            self.normalize_path(&format!("{}/{}", self.current_path, path))
        };

        let body = serde_json::json!({
            "path": full_path
        });

        let meta: DropboxMetadata = self.rpc_call("files/get_metadata", &body).await?;
        Ok(self.to_remote_entry(&meta))
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

    fn supports_checksum(&self) -> bool {
        true
    }

    /// Dropbox `content_hash` from the file metadata (one API call, no
    /// content download). Exposed under the dedicated `dropbox` key:
    /// it is a SHA-256-of-block-SHA-256 scheme, not a plain file
    /// digest, so it is never reported as md5/sha1/sha256.
    async fn checksum(&mut self, path: &str) -> Result<HashMap<String, String>, ProviderError> {
        let entry = self.stat(path).await?;
        let mut out = HashMap::new();
        if let Some(v) = entry.metadata.get("dropbox") {
            out.insert("dropbox".to_string(), v.clone());
        }
        Ok(out)
    }

    async fn keep_alive(&mut self) -> Result<(), ProviderError> {
        Ok(())
    }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        Ok("Dropbox API v2".to_string())
    }

    fn supports_server_copy(&self) -> bool {
        true
    }

    fn supports_server_side_copy(&self) -> bool {
        true
    }

    async fn server_copy(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        // Legacy alias kept so CLI / MCP / provider_commands callers keep
        // working. The real `files/copy_v2` implementation lives on
        // `server_side_copy` (S3-T10 migration, v4.0.0).
        StorageProvider::server_side_copy(self, from, to).await
    }

    async fn server_side_copy(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let from_path = if from.starts_with('/') {
            self.normalize_path(from)
        } else {
            self.normalize_path(&format!("{}/{}", self.current_path, from))
        };

        let to_path = if to.starts_with('/') {
            self.normalize_path(to)
        } else {
            self.normalize_path(&format!("{}/{}", self.current_path, to))
        };

        let body = serde_json::json!({
            "from_path": from_path,
            "to_path": to_path
        });

        let _: serde_json::Value = self.rpc_call("files/copy_v2", &body).await?;

        info!("Copied {} to {}", from, to);
        Ok(())
    }

    fn supports_find(&self) -> bool {
        true
    }

    async fn find(&mut self, path: &str, pattern: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let search_path = self.normalize_path(path);

        // Dropbox search_v2 treats the query as a substring on filename.content,
        // so glob metacharacters confuse it. Send the literal core to the
        // server for a broad prefilter and apply the precise match client-side
        // via matches_find_pattern (glob via globset, substring fallback).
        let literal: String = pattern
            .chars()
            .filter(|c| !matches!(c, '*' | '?' | '[' | ']'))
            .collect();
        let server_query = if literal.is_empty() {
            ".".to_string() // any character, broadest possible prefilter
        } else {
            literal
        };

        let body = serde_json::json!({
            "query": server_query,
            "options": {
                "path": if search_path.is_empty() { "" } else { search_path.as_str() },
                "max_results": 200,
                "file_status": "active"
            }
        });

        let url = format!("{}/files/search_v2", API_BASE);

        let response = self
            .client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!(
                "Search failed: {}",
                sanitize_api_error(&text)
            )));
        }

        #[derive(Deserialize)]
        struct MatchMetadata {
            metadata: DropboxMetadata,
        }
        #[derive(Deserialize)]
        struct SearchMatch {
            metadata: MatchMetadata,
        }
        #[derive(Deserialize)]
        struct SearchResult {
            matches: Vec<SearchMatch>,
        }

        let result: SearchResult = response
            .json()
            .await
            .map_err(|e| ProviderError::Other(format!("Parse error: {}", e)))?;

        Ok(result
            .matches
            .iter()
            .map(|m| self.to_remote_entry(&m.metadata.metadata))
            .filter(|e| super::matches_find_pattern(&e.name, pattern))
            .collect())
    }

    async fn storage_info(&mut self) -> Result<StorageInfo, ProviderError> {
        let url = format!("{}/users/get_space_usage", API_BASE);

        let response = self
            .client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!(
                "Space usage failed: {}",
                sanitize_api_error(&text)
            )));
        }

        #[derive(Deserialize)]
        struct Allocation {
            allocated: u64,
        }
        #[derive(Deserialize)]
        struct SpaceUsage {
            used: u64,
            allocation: Allocation,
        }

        let usage: SpaceUsage = response
            .json()
            .await
            .map_err(|e| ProviderError::Other(format!("Parse error: {}", e)))?;

        Ok(StorageInfo {
            used: usage.used,
            total: usage.allocation.allocated,
            free: usage.allocation.allocated.saturating_sub(usage.used),
        })
    }

    fn supports_share_links(&self) -> bool {
        true
    }

    fn share_link_capabilities(&self) -> ShareLinkCapabilities {
        ShareLinkCapabilities {
            supports_expiration: true,
            supports_password: true,
            supports_permissions: true,
            available_permissions: vec!["view".into(), "edit".into()],
            supports_list_links: true,
            supports_revoke: true,
        }
    }

    async fn create_share_link(
        &mut self,
        path: &str,
        options: ShareLinkOptions,
    ) -> Result<ShareLinkResult, ProviderError> {
        let full_path = if path.starts_with('/') {
            self.normalize_path(path)
        } else {
            self.normalize_path(&format!("{}/{}", self.current_path, path))
        };

        // Try to create a shared link
        // Dropbox API: access "viewer"/"editor", password (Pro+ only), expires (Pro+ only)
        let access = match options.permissions.as_deref() {
            Some("edit") => "editor",
            _ => "viewer",
        };
        let mut settings = serde_json::json!({
            "access": access,
            "allow_download": true,
            "audience": "public",
            "requested_visibility": "public"
        });
        if let Some(secs) = options.expires_in_secs {
            let expires_at = chrono::Utc::now() + chrono::Duration::seconds(secs as i64);
            settings["expires"] =
                serde_json::Value::String(expires_at.format("%Y-%m-%dT%H:%M:%SZ").to_string());
        }
        if let Some(ref pw) = options.password {
            settings["link_password"] = serde_json::json!(pw);
            settings["requested_visibility"] = serde_json::json!("password");
        }
        let body = serde_json::json!({
            "path": full_path,
            "settings": settings
        });

        // Dropbox API: sharing/create_shared_link_with_settings
        let url = format!("{}/sharing/create_shared_link_with_settings", API_BASE);

        let response = self
            .client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        let status = response.status();
        let text = response.text().await.unwrap_or_default();

        // If link already exists, get it instead
        if status == 409 && text.contains("shared_link_already_exists") {
            // Get existing link
            let body = serde_json::json!({
                "path": full_path,
                "direct_only": false
            });

            let url = format!("{}/sharing/list_shared_links", API_BASE);

            let response = self
                .client
                .post(&url)
                .header(AUTHORIZATION, self.auth_header().await?)
                .header(CONTENT_TYPE, "application/json")
                .body(body.to_string())
                .send()
                .await
                .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

            if !response.status().is_success() {
                return Err(ProviderError::Other(
                    "Failed to get existing share link".to_string(),
                ));
            }

            #[derive(Deserialize)]
            struct SharedLink {
                url: String,
            }
            #[derive(Deserialize)]
            struct SharedLinksResult {
                links: Vec<SharedLink>,
            }

            let result: SharedLinksResult = response
                .json()
                .await
                .map_err(|e| ProviderError::Other(format!("Failed to parse response: {}", e)))?;

            if let Some(link) = result.links.first() {
                info!("Retrieved existing share link for {}: {}", path, link.url);
                return Ok(ShareLinkResult {
                    url: link.url.clone(),
                    password: None,
                    expires_at: None,
                });
            }

            return Err(ProviderError::Other(
                "No existing share link found".to_string(),
            ));
        }

        if !status.is_success() {
            if text.contains("missing_scope") {
                return Err(ProviderError::AuthenticationFailed(
                    "Dropbox token missing 'sharing.write' scope. Please disconnect and reconnect Dropbox to refresh permissions.".to_string()
                ));
            }
            return Err(ProviderError::Other(format!(
                "Failed to create share link: {} - {}",
                status,
                sanitize_api_error(&text)
            )));
        }

        #[derive(Deserialize)]
        struct CreateLinkResponse {
            url: String,
        }

        let result: CreateLinkResponse = serde_json::from_str(&text)
            .map_err(|e| ProviderError::Other(format!("Failed to parse response: {}", e)))?;

        info!("Created share link for {}: {}", path, result.url);
        Ok(ShareLinkResult {
            url: result.url,
            password: None,
            expires_at: None,
        })
    }

    async fn list_share_links(&mut self, path: &str) -> Result<Vec<ShareLinkInfo>, ProviderError> {
        let full_path = if path.starts_with('/') {
            self.normalize_path(path)
        } else {
            self.normalize_path(&format!("{}/{}", self.current_path, path))
        };

        let body = serde_json::json!({
            "path": full_path,
            "direct_only": false
        });

        let url = format!("{}/sharing/list_shared_links", API_BASE);
        let response = self
            .client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!(
                "Failed to list share links: {}",
                sanitize_api_error(&text)
            )));
        }

        #[derive(Deserialize)]
        struct DropboxSharedLink {
            #[serde(default)]
            id: Option<String>,
            url: String,
            #[serde(default)]
            expires: Option<String>,
            #[serde(default)]
            link_permissions: Option<serde_json::Value>,
        }
        #[derive(Deserialize)]
        struct ListResult {
            links: Vec<DropboxSharedLink>,
        }

        let result: ListResult = response
            .json()
            .await
            .map_err(|e| ProviderError::Other(format!("Failed to parse response: {}", e)))?;

        let links = result
            .links
            .into_iter()
            .map(|link| {
                let permissions = link
                    .link_permissions
                    .as_ref()
                    .and_then(|lp| lp.get("resolved_visibility"))
                    .and_then(|v| v.get(".tag"))
                    .and_then(|t| t.as_str())
                    .map(|s| s.to_string());
                ShareLinkInfo {
                    id: link.id.unwrap_or_else(|| link.url.clone()),
                    url: link.url,
                    created_at: None,
                    expires_at: link.expires,
                    password_protected: link
                        .link_permissions
                        .as_ref()
                        .and_then(|lp| lp.get("resolved_visibility"))
                        .and_then(|v| v.get(".tag"))
                        .and_then(|t| t.as_str())
                        .map(|s| s == "password")
                        .unwrap_or(false),
                    permissions,
                }
            })
            .collect();

        Ok(links)
    }

    async fn remove_share_link(&mut self, path: &str) -> Result<(), ProviderError> {
        let full_path = if path.starts_with('/') {
            self.normalize_path(path)
        } else {
            self.normalize_path(&format!("{}/{}", self.current_path, path))
        };

        // First get the existing share link URL
        let body = serde_json::json!({
            "path": full_path,
            "direct_only": false
        });

        let url = format!("{}/sharing/list_shared_links", API_BASE);
        let response = self
            .client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            return Err(ProviderError::Other(
                "Failed to list share links".to_string(),
            ));
        }

        #[derive(Deserialize)]
        struct SharedLink {
            url: String,
        }
        #[derive(Deserialize)]
        struct SharedLinksResult {
            links: Vec<SharedLink>,
        }

        let result: SharedLinksResult = response
            .json()
            .await
            .map_err(|e| ProviderError::Other(format!("Failed to parse response: {}", e)))?;

        let link_url = result
            .links
            .first()
            .ok_or_else(|| ProviderError::Other("No share link found to remove".to_string()))?;

        // Revoke the shared link
        let body = serde_json::json!({
            "url": link_url.url
        });

        let url = format!("{}/sharing/revoke_shared_link", API_BASE);
        let response = self
            .client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!(
                "Failed to revoke share link: {}",
                sanitize_api_error(&text)
            )));
        }

        info!("Revoked share link for {}", path);
        Ok(())
    }

    fn supports_versions(&self) -> bool {
        true
    }

    async fn list_versions(
        &mut self,
        path: &str,
    ) -> Result<Vec<super::FileVersion>, ProviderError> {
        let full_path = if path.starts_with('/') {
            self.normalize_path(path)
        } else {
            self.normalize_path(&format!("{}/{}", self.current_path, path))
        };

        let body = serde_json::json!({
            "path": full_path,
            "limit": 100
        });

        let url = format!("{}/files/list_revisions", API_BASE);

        let response = self
            .client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!(
                "List revisions failed: {}",
                sanitize_api_error(&text)
            )));
        }

        #[derive(Deserialize)]
        struct RevisionEntry {
            rev: String,
            size: u64,
            server_modified: String,
        }
        #[derive(Deserialize)]
        struct RevisionList {
            entries: Vec<RevisionEntry>,
        }

        let list: RevisionList = response
            .json()
            .await
            .map_err(|e| ProviderError::Other(format!("Parse error: {}", e)))?;

        Ok(list
            .entries
            .iter()
            .map(|r| super::FileVersion {
                id: r.rev.clone(),
                modified: Some(r.server_modified.clone()),
                size: r.size,
                modified_by: None,
            })
            .collect())
    }

    async fn download_version(
        &mut self,
        path: &str,
        version_id: &str,
        local_path: &str,
    ) -> Result<(), ProviderError> {
        let full_path = if path.starts_with('/') {
            self.normalize_path(path)
        } else {
            self.normalize_path(&format!("{}/{}", self.current_path, path))
        };

        let arg = serde_json::json!({
            "path": format!("rev:{}", version_id)
        });

        let url = format!("{}/files/download", CONTENT_BASE);

        let response = self
            .client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header("Dropbox-API-Arg", arg.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::TransferFailed(format!(
                "Download revision failed: {}",
                sanitize_api_error(&text)
            )));
        }

        let bytes = response
            .bytes()
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        let _ = full_path; // Used for path resolution
        tokio::fs::write(local_path, &bytes)
            .await
            .map_err(ProviderError::IoError)?;

        Ok(())
    }

    async fn restore_version(&mut self, path: &str, version_id: &str) -> Result<(), ProviderError> {
        let full_path = if path.starts_with('/') {
            self.normalize_path(path)
        } else {
            self.normalize_path(&format!("{}/{}", self.current_path, path))
        };

        let body = serde_json::json!({
            "path": full_path,
            "rev": version_id
        });

        let _: serde_json::Value = self.rpc_call("files/restore", &body).await?;
        info!("Restored {} to revision {}", path, version_id);
        Ok(())
    }

    fn supports_thumbnails(&self) -> bool {
        true
    }

    async fn get_thumbnail(&mut self, path: &str) -> Result<String, ProviderError> {
        let full_path = if path.starts_with('/') {
            self.normalize_path(path)
        } else {
            self.normalize_path(&format!("{}/{}", self.current_path, path))
        };

        let arg = serde_json::json!({
            "path": full_path,
            "format": "jpeg",
            "size": "w256h256"
        });

        let url = format!("{}/files/get_thumbnail_v2", CONTENT_BASE);

        let response = self
            .client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header("Dropbox-API-Arg", arg.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            return Err(ProviderError::NotFound(
                "No thumbnail available".to_string(),
            ));
        }

        let bytes = response
            .bytes()
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        // Return as base64 data URI
        Ok(format!(
            "data:image/jpeg;base64,{}",
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes)
        ))
    }

    fn supports_locking(&self) -> bool {
        true
    }

    async fn lock_file(&mut self, path: &str, _timeout: u64) -> Result<LockInfo, ProviderError> {
        let full_path = if path.starts_with('/') {
            self.normalize_path(path)
        } else {
            self.normalize_path(&format!("{}/{}", self.current_path, path))
        };

        let body = serde_json::json!({
            "entries": [{
                ".tag": "path_lookup",
                "path": full_path
            }]
        });

        let url = format!("{}/files/lock_file_batch", API_BASE);
        let response = self
            .client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!(
                "Lock failed: {}",
                sanitize_api_error(&text)
            )));
        }

        #[derive(Deserialize)]
        struct LockContent {
            #[serde(default)]
            lock_holder_account_id: Option<String>,
        }
        #[derive(Deserialize)]
        struct LockEntry {
            #[serde(default)]
            lock: Option<LockContent>,
        }
        #[derive(Deserialize)]
        struct LockResult {
            #[serde(rename = ".tag")]
            tag: String,
            #[serde(flatten)]
            entry: Option<LockEntry>,
        }
        #[derive(Deserialize)]
        struct LockBatchResponse {
            entries: Vec<LockResult>,
        }

        let result: LockBatchResponse = response
            .json()
            .await
            .map_err(|e| ProviderError::Other(format!("Parse error: {}", e)))?;

        if let Some(entry) = result.entries.first() {
            if entry.tag == "success" {
                let owner = entry
                    .entry
                    .as_ref()
                    .and_then(|e| e.lock.as_ref())
                    .and_then(|l| l.lock_holder_account_id.clone());
                return Ok(LockInfo {
                    token: full_path.clone(),
                    owner,
                    timeout: 0,
                    exclusive: true,
                });
            }
        }

        Err(ProviderError::Other(
            "Lock failed: no success entry".to_string(),
        ))
    }

    async fn unlock_file(&mut self, path: &str, _lock_token: &str) -> Result<(), ProviderError> {
        let full_path = if path.starts_with('/') {
            self.normalize_path(path)
        } else {
            self.normalize_path(&format!("{}/{}", self.current_path, path))
        };

        let body = serde_json::json!({
            "entries": [{
                ".tag": "path_lookup",
                "path": full_path
            }]
        });

        let url = format!("{}/files/unlock_file_batch", API_BASE);
        let response = self
            .client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!(
                "Unlock failed: {}",
                sanitize_api_error(&text)
            )));
        }

        info!("Unlocked file: {}", path);
        Ok(())
    }

    fn transfer_optimization_hints(&self) -> super::TransferOptimizationHints {
        super::TransferOptimizationHints {
            supports_multipart: true,
            multipart_threshold: DROPBOX_SESSION_THRESHOLD,
            multipart_part_size: DROPBOX_MULTIPART_PART_SIZE,
            // Concurrent upload sessions allow parallel `append_v2` calls at
            // distinct byte offsets. Aligned with the 4-slot budget the AIMD
            // controller defaults to so the chunk fan-out actually parallelises.
            multipart_max_parallel: 4,
            supports_resume_download: true,
            supports_resume_upload: true,
            ..Default::default()
        }
    }

    // Shaped-graph multipart trait wiring (S2-T02).
    //
    // Dropbox's "concurrent" upload session (per the v2 HTTP API) is the
    // natural fit for the trait's `upload_part` semantics:
    //   1. `upload_session/start` with body=empty + `session_type=concurrent`
    //      and `close: false` returns a durable `session_id`. The trait
    //      handle stores it together with `total_size` / `part_size` so
    //      `upload_part` can derive the per-call byte offset.
    //   2. `upload_session/append_v2` accepts parts in any order as long
    //      as `cursor.offset` aligns with the actual byte position.
    //   3. `upload_session/finish` with body=empty + `cursor.offset =
    //      total_size` + a `commit` block finalises the file at the
    //      destination path.
    //   4. Dropbox has no explicit abort. Sessions GC after 48h, so the
    //      abort method is a no-op (best-effort `Ok(())`).
    //
    // Concurrent mode requires the start + finish calls to have empty
    // bodies, which is why every chunk goes through `append_v2` instead
    // of being attached to the boundary calls.

    async fn begin_multipart_upload(
        &mut self,
        remote_path: &str,
        total_size: u64,
        _content_type: Option<&str>,
        _local_source_path: Option<&str>,
    ) -> Result<MultipartHandle, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        if total_size == 0 {
            return Err(ProviderError::Other(
                "Dropbox multipart upload requires non-zero total_size".to_string(),
            ));
        }

        let normalised = self.normalize_path(remote_path);
        let start_url = format!("{}/files/upload_session/start", CONTENT_BASE);
        let start_arg = serde_json::json!({
            "session_type": { ".tag": "concurrent" },
            "close": false
        });

        let auth = self.auth_header().await?;
        let response = self
            .client
            .post(&start_url)
            .header(AUTHORIZATION, auth)
            .header(CONTENT_TYPE, "application/octet-stream")
            .header("Dropbox-API-Arg", start_arg.to_string())
            // Concurrent session start MUST have an empty body per the
            // Dropbox API contract.
            .body(Vec::<u8>::new())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let retry_header = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .map(String::from);
            let text = response.text().await.unwrap_or_default();
            let mut msg = format!(
                "Dropbox upload_session/start failed (HTTP {}): {}",
                status,
                sanitize_api_error(&text)
            );
            if let Some(tail) =
                dropbox_retry_marker_tail(status.as_u16(), &text, retry_header.as_deref())
            {
                msg.push_str(&tail);
            }
            return Err(ProviderError::Other(msg));
        }

        #[derive(Deserialize)]
        struct SessionStart {
            session_id: String,
        }
        let session: SessionStart = response
            .json()
            .await
            .map_err(|e| ProviderError::Other(format!("Parse error: {}", e)))?;

        let meta = DropboxMultipartMeta {
            session_id: session.session_id,
            total: total_size,
            part: dropbox_runner_part_size(total_size),
            path: normalised,
        };
        Ok(MultipartHandle {
            upload_id: meta.encode(),
            remote_path: remote_path.to_string(),
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
        if part_number == 0 {
            return Err(ProviderError::Other(
                "Dropbox upload_part requires 1-based part_number".to_string(),
            ));
        }
        if data.is_empty() {
            return Err(ProviderError::Other(
                "Dropbox upload_part received empty data".to_string(),
            ));
        }
        let meta = DropboxMultipartMeta::decode(&handle.upload_id)?;
        let offset = (part_number as u64 - 1) * meta.part;
        let end = offset
            .checked_add(data.len() as u64)
            .ok_or_else(|| ProviderError::Other("Dropbox part offset overflow".to_string()))?;
        if end > meta.total {
            return Err(ProviderError::Other(format!(
                "Dropbox part {} exceeds declared total: offset {} + len {} > total {}",
                part_number,
                offset,
                data.len(),
                meta.total
            )));
        }

        let append_url = format!("{}/files/upload_session/append_v2", CONTENT_BASE);
        let append_arg = serde_json::json!({
            "cursor": {
                "session_id": meta.session_id,
                "offset": offset
            },
            "close": false
        });

        let auth = self.auth_header().await?;
        let response = self
            .client
            .post(&append_url)
            .header(AUTHORIZATION, auth)
            .header(CONTENT_TYPE, "application/octet-stream")
            .header("Dropbox-API-Arg", append_arg.to_string())
            .body(data)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let retry_header = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .map(String::from);
            let text = response.text().await.unwrap_or_default();
            let mut msg = format!(
                "Dropbox upload_part {} failed (HTTP {}): {}",
                part_number,
                status,
                sanitize_api_error(&text)
            );
            if let Some(tail) =
                dropbox_retry_marker_tail(status.as_u16(), &text, retry_header.as_deref())
            {
                msg.push_str(&tail);
            }
            return Err(ProviderError::Other(msg));
        }

        Ok(UploadedPart {
            part_number,
            etag: String::new(),
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
        let meta = DropboxMultipartMeta::decode(&handle.upload_id)?;
        let expected = meta.total.div_ceil(meta.part).max(1) as usize;
        if parts.len() != expected {
            return Err(ProviderError::TransferFailed(format!(
                "Dropbox complete: expected {} parts, runner committed {}",
                expected,
                parts.len()
            )));
        }

        // Concurrent upload sessions MUST be closed explicitly before
        // `finish` is invoked. The protocol path: send a no-op
        // `append_v2` with `close: true` and an empty body at the
        // closing offset; then call `finish`. Without this Dropbox
        // returns 409 `concurrent_session_not_closed/`.
        let close_url = format!("{}/files/upload_session/append_v2", CONTENT_BASE);
        let close_arg = serde_json::json!({
            "cursor": {
                "session_id": meta.session_id,
                "offset": meta.total
            },
            "close": true
        });
        let auth_close = self.auth_header().await?;
        let close_response = self
            .client
            .post(&close_url)
            .header(AUTHORIZATION, auth_close)
            .header(CONTENT_TYPE, "application/octet-stream")
            .header("Dropbox-API-Arg", close_arg.to_string())
            .body(Vec::<u8>::new())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;
        if !close_response.status().is_success() {
            let status = close_response.status();
            let retry_header = close_response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .map(String::from);
            let text = close_response.text().await.unwrap_or_default();
            let mut msg = format!(
                "Dropbox upload_session/append_v2 close=true failed (HTTP {}): {}",
                status,
                sanitize_api_error(&text)
            );
            if let Some(tail) =
                dropbox_retry_marker_tail(status.as_u16(), &text, retry_header.as_deref())
            {
                msg.push_str(&tail);
            }
            return Err(ProviderError::Other(msg));
        }

        let finish_url = format!("{}/files/upload_session/finish", CONTENT_BASE);
        let finish_arg = serde_json::json!({
            "cursor": {
                "session_id": meta.session_id,
                "offset": meta.total
            },
            "commit": {
                "path": meta.path,
                "mode": "overwrite",
                "autorename": false,
                "mute": false
            }
        });

        let auth = self.auth_header().await?;
        let response = self
            .client
            .post(&finish_url)
            .header(AUTHORIZATION, auth)
            .header(CONTENT_TYPE, "application/octet-stream")
            .header("Dropbox-API-Arg", finish_arg.to_string())
            .body(Vec::<u8>::new())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let retry_header = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .map(String::from);
            let text = response.text().await.unwrap_or_default();
            let mut msg = format!(
                "Dropbox upload_session/finish failed (HTTP {}): {}",
                status,
                sanitize_api_error(&text)
            );
            if let Some(tail) =
                dropbox_retry_marker_tail(status.as_u16(), &text, retry_header.as_deref())
            {
                msg.push_str(&tail);
            }
            return Err(ProviderError::Other(msg));
        }
        Ok(())
    }

    async fn abort_multipart_upload(
        &mut self,
        _handle: MultipartHandle,
    ) -> Result<(), ProviderError> {
        // Dropbox has no explicit abort endpoint. Concurrent upload
        // sessions GC after 48h on the server side. Treat the call as a
        // best-effort no-op so the runner's failure-path abort cannot
        // mask the original transfer error.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_provider() -> DropboxProvider {
        DropboxProvider::new(DropboxConfig::new("app-key", "app-secret"))
    }

    #[test]
    fn normalize_path_returns_empty_for_root_and_slash_prefix_for_rest() {
        let p = test_provider();
        assert_eq!(p.normalize_path(""), "");
        assert_eq!(p.normalize_path("/"), "");
        assert_eq!(p.normalize_path("///"), "");
        assert_eq!(p.normalize_path("foo"), "/foo");
        assert_eq!(p.normalize_path("/foo"), "/foo");
        assert_eq!(p.normalize_path("/foo/"), "/foo");
        assert_eq!(p.normalize_path("foo/bar"), "/foo/bar");
    }

    #[test]
    fn normalize_path_trims_multiple_slashes_at_both_ends() {
        let p = test_provider();
        assert_eq!(p.normalize_path("///foo///"), "/foo");
    }

    #[test]
    fn oauth_config_uses_provider_configured_keys() {
        let p = test_provider();
        let oauth = p.oauth_config();
        // keys have been threaded through to OAuthConfig
        assert_eq!(oauth.client_id, "app-key");
    }

    fn make_provider_config() -> ProviderConfig {
        ProviderConfig {
            name: "test-dropbox".to_string(),
            provider_type: ProviderType::Dropbox,
            host: String::new(),
            port: None,
            username: None,
            password: None,
            initial_path: None,
            extra: Default::default(),
        }
    }

    #[test]
    fn dropbox_config_from_provider_config_prefers_app_key_over_client_id() {
        let mut cfg = make_provider_config();
        cfg.extra
            .insert("app_key".to_string(), "primary".to_string());
        cfg.extra
            .insert("client_id".to_string(), "fallback".to_string());
        cfg.extra
            .insert("app_secret".to_string(), "secret".to_string());
        let out = DropboxConfig::from_provider_config(&cfg).unwrap();
        assert_eq!(out.app_key, "primary");
        assert_eq!(out.app_secret, "secret");
    }

    #[test]
    fn dropbox_config_from_provider_config_falls_back_to_client_id() {
        let mut cfg = make_provider_config();
        cfg.extra
            .insert("client_id".to_string(), "fallback".to_string());
        cfg.extra
            .insert("client_secret".to_string(), "s".to_string());
        let out = DropboxConfig::from_provider_config(&cfg).unwrap();
        assert_eq!(out.app_key, "fallback");
        assert_eq!(out.app_secret, "s");
    }

    // T-DEBT-05 S1-T02c: Dropbox rate-limit detection + marker emission.

    #[test]
    fn dropbox_is_rate_limited_recognises_429() {
        assert!(dropbox_is_rate_limited(429, ""));
        assert!(dropbox_is_rate_limited(429, "anything"));
    }

    #[test]
    fn dropbox_is_rate_limited_recognises_409_with_too_many_requests_tag() {
        let body = r#"{"error":{".tag":"too_many_requests"},"retry_after":30}"#;
        assert!(dropbox_is_rate_limited(409, body));
        let body_w = r#"{"error":{".tag":"too_many_write_operations"},"retry_after":15}"#;
        assert!(dropbox_is_rate_limited(409, body_w));
    }

    #[test]
    fn dropbox_is_rate_limited_rejects_non_throttle() {
        assert!(!dropbox_is_rate_limited(500, ""));
        assert!(!dropbox_is_rate_limited(409, r#"{"error":{".tag":"path/conflict"}}"#));
        assert!(!dropbox_is_rate_limited(200, "any"));
    }

    #[test]
    fn dropbox_retry_marker_tail_prefers_body_field() {
        // Body wins over header when both are present.
        let body = r#"{"error":{".tag":"too_many_requests"},"retry_after":45}"#;
        let tail = dropbox_retry_marker_tail(429, body, Some("10")).expect("body field wins");
        assert!(tail.contains("retry-after-secs=45"));
    }

    #[test]
    fn dropbox_retry_marker_tail_falls_back_to_header_when_body_unparseable() {
        let body = "not even JSON";
        let tail = dropbox_retry_marker_tail(429, body, Some("12")).expect("header fallback");
        assert!(tail.contains("retry-after-secs=12"));
    }

    #[test]
    fn dropbox_retry_marker_tail_returns_none_when_not_rate_limited() {
        // 500 with both signals: not a throttle.
        let body = r#"{"retry_after":30}"#;
        assert_eq!(dropbox_retry_marker_tail(500, body, Some("30")), None);
    }

    #[test]
    fn dropbox_retry_marker_tail_returns_none_when_no_hint_anywhere() {
        // 429 but the body field is missing and the header is absent: marker
        // absent → executor falls back to default cooldown.
        assert_eq!(
            dropbox_retry_marker_tail(429, r#"{"error":{}}"#, None),
            None
        );
    }

    // ---------------------------------------------------------------------
    // S2-T02 multipart trait wiring
    // ---------------------------------------------------------------------

    #[test]
    fn dropbox_meta_roundtrip_preserves_session_total_part_path() {
        let meta = DropboxMultipartMeta {
            session_id: "AAAAAAAAAAdwxe5XXXXXX".to_string(),
            total: 524_288_000,
            part: 8_323_073,
            path: "/aeroftp-test/sample.bin".to_string(),
        };
        let encoded = meta.encode();
        let decoded = DropboxMultipartMeta::decode(&encoded).expect("decode");
        assert_eq!(decoded, meta);
    }

    #[test]
    fn dropbox_meta_decode_rejects_garbage() {
        let err = DropboxMultipartMeta::decode("not json").unwrap_err();
        assert!(matches!(err, ProviderError::Other(_)));
    }

    #[test]
    fn dropbox_runner_part_size_returns_constant_chunk_size_for_nonzero_total() {
        // The runner uses `preferred_chunk_size` verbatim, so the helper
        // mirrors `DROPBOX_MULTIPART_PART_SIZE` for any non-zero total.
        // Zero is the sentinel for an empty file (rejected at the trait
        // boundary before the runner ever sees it).
        assert_eq!(dropbox_runner_part_size(0), 0);
        for &total in &[
            1u64,
            DROPBOX_MULTIPART_PART_SIZE - 1,
            DROPBOX_MULTIPART_PART_SIZE,
            DROPBOX_MULTIPART_PART_SIZE + 1,
            DROPBOX_SESSION_THRESHOLD,
            500 * 1024 * 1024,
            10 * 1024 * 1024 * 1024,
        ] {
            assert_eq!(dropbox_runner_part_size(total), DROPBOX_MULTIPART_PART_SIZE);
        }
    }

    #[test]
    fn dropbox_offset_math_covers_500_mib_with_8mib_chunks() {
        // With the runner forwarding `preferred_chunk_size = 8 MiB`,
        // 500 MiB fans into 63 parts: 62 full 8 MiB chunks + a 4 MiB tail.
        let total: u64 = 500 * 1024 * 1024;
        let part = DROPBOX_MULTIPART_PART_SIZE;
        let parts = total.div_ceil(part);
        assert_eq!(parts, 63);
        let mut seen_end: i128 = -1;
        for pn in 1..=parts {
            let offset = (pn - 1) * part;
            let len = part.min(total - offset);
            let end = offset + len - 1;
            assert!(offset as i128 > seen_end);
            seen_end = end as i128;
        }
        assert_eq!(seen_end as u64, total - 1);
    }
}
