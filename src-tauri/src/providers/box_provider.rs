//! Box Cloud Storage Provider
//!
//! Implements StorageProvider for Box using the Box API v2.
//! Uses OAuth2 PKCE for authentication.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use async_trait::async_trait;
use chrono;
use reqwest::header::{HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::Deserialize;
use std::collections::HashMap;
use tracing::info;

use super::types::BoxConfig;
use super::{
    oauth2::{OAuth2Manager, OAuthConfig},
    sanitize_api_error, FileVersion, MultipartHandle, ProviderError, ProviderType, RemoteEntry,
    ShareLinkCapabilities, ShareLinkInfo, ShareLinkOptions, ShareLinkResult, StorageInfo,
    StorageProvider, UploadedPart,
};

/// Box API endpoints
const API_BASE: &str = "https://api.box.com/2.0";

/// Box requires multipart upload sessions for files >= 20 MiB. Below this the
/// simple `/files/content` POST is faster. The trait runner uses the same
/// threshold so single-PUT files keep the legacy path.
const BOX_SESSION_THRESHOLD: u64 = 20 * 1024 * 1024;

/// Opaque metadata threaded through `MultipartHandle.upload_id` for the Box
/// chunked-upload-v2 trait. Box is unusual in two ways:
///
/// - The server picks the `part_size` (returned by
///   `POST /files/upload_sessions`), so we cannot derive it from the
///   advertised hint — we MUST round-trip the server value.
/// - The `commit` call needs every part's `{part_id, offset, size, sha1}`
///   receipt plus a whole-file SHA-1 `Digest` header.
///
/// Storing the per-part receipts on the handle would balloon it, so the
/// trait method shape pushes them through `UploadedPart.etag` (encoded as
/// JSON, since Box's receipt is more than just an ETag) and rebuilds the
/// commit list at `complete_multipart_upload` time. The whole-file SHA-1
/// is recomputed from disk to keep `upload_part` stateless.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
struct BoxMultipartMeta {
    session_id: String,
    upload_part_url: String,
    commit_url: String,
    total: u64,
    part: u64,
    local_path: String,
}

impl BoxMultipartMeta {
    fn encode(&self) -> String {
        serde_json::to_string(self).expect("BoxMultipartMeta is always serializable")
    }

    fn decode(s: &str) -> Result<Self, ProviderError> {
        serde_json::from_str(s)
            .map_err(|e| ProviderError::Other(format!("Invalid Box multipart handle: {}", e)))
    }
}

/// Per-part receipt embedded inside `UploadedPart.etag` so
/// `complete_multipart_upload` can reconstruct the Box commit body. Box's
/// `part` JSON object has the shape `{part_id, offset, size, sha1}`.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
struct BoxPartReceipt {
    part_id: String,
    offset: u64,
    size: u64,
    sha1: String,
}

/// T-DEBT-05 S1-T02e: Box returns 429 with a standard `Retry-After` header
/// (seconds form). Box does not use a JSON body field for the hint; the
/// header is the authoritative source. Box may also return 503 in
/// maintenance windows, which is congestion the same way OneDrive's 503
/// is.
fn box_is_rate_limited(status: u16) -> bool {
    status == 429 || status == 503
}

/// Compute the marker substring for a Box rate-limit response. Pure-fn
/// so the test exercises the header-parsing branches without any HTTP
/// scaffolding.
fn box_retry_marker_tail(status: u16, retry_header: Option<&str>) -> Option<String> {
    if !box_is_rate_limited(status) {
        return None;
    }
    let hint = super::retry_after::parse_retry_after_seconds(retry_header.unwrap_or(""))?;
    Some(crate::transfer_dag::adaptive::embed_retry_after_marker(
        hint.as_secs(),
    ))
}

/// Build a `ProviderError::Other` from a failed Box HTTP response,
/// appending the Retry-After marker when the response is a throttle
/// signal. The `context_prefix` is prepended verbatim.
async fn box_error_from_response(
    response: reqwest::Response,
    context_prefix: &str,
) -> ProviderError {
    let status = response.status();
    let retry_header = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let text = response.text().await.unwrap_or_default();
    let mut msg = format!(
        "{} {}: {}",
        context_prefix,
        status,
        sanitize_api_error(&text)
    );
    if let Some(tail) = box_retry_marker_tail(status.as_u16(), retry_header.as_deref()) {
        msg.push_str(&tail);
    }
    ProviderError::Other(msg)
}
const UPLOAD_BASE: &str = "https://upload.box.com/api/2.0";

/// Box watermark info (returned by fields=watermark_info)
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct BoxWatermarkInfo {
    is_watermarked: Option<bool>,
}

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
    watermark_info: Option<BoxWatermarkInfo>,
    #[serde(default)]
    tags: Vec<String>,
    /// Server-computed SHA-1 (Box returns it on file objects when
    /// requested via `fields=sha1`). Absent for folders.
    #[serde(default)]
    sha1: Option<String>,
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
    #[serde(default)]
    effective_access: Option<String>,
    #[serde(default)]
    unshared_at: Option<String>,
    #[serde(default)]
    is_password_enabled: Option<bool>,
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

/// Box trash item (includes trashed_at)
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct BoxTrashItem {
    #[serde(rename = "type")]
    item_type: String,
    id: String,
    name: String,
    size: Option<u64>,
    modified_at: Option<String>,
    trashed_at: Option<String>,
}

/// Box trash items collection
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct BoxTrashCollection {
    entries: Vec<BoxTrashItem>,
    total_count: u64,
    offset: u64,
    limit: u64,
}

/// Box comment
#[derive(Debug, Deserialize, serde::Serialize)]
pub struct BoxComment {
    id: String,
    message: Option<String>,
    created_at: Option<String>,
    created_by: Option<BoxCommentUser>,
}

/// Box comment user
#[derive(Debug, Deserialize, serde::Serialize)]
pub struct BoxCommentUser {
    name: Option<String>,
    login: Option<String>,
}

/// Box comment collection
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct BoxCommentCollection {
    entries: Vec<BoxComment>,
}

/// Box collaboration
#[derive(Debug, Deserialize, serde::Serialize)]
pub struct BoxCollaboration {
    id: String,
    role: Option<String>,
    status: Option<String>,
    accessible_by: Option<BoxCollabUser>,
    created_at: Option<String>,
}

/// Box collaboration user
#[derive(Debug, Deserialize, serde::Serialize)]
pub struct BoxCollabUser {
    #[serde(rename = "type")]
    user_type: Option<String>,
    id: Option<String>,
    name: Option<String>,
    login: Option<String>,
}

/// Box collaboration collection
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct BoxCollabCollection {
    entries: Vec<BoxCollaboration>,
}

/// Box folder lock
#[derive(Debug, Deserialize, serde::Serialize)]
pub struct BoxFolderLock {
    id: String,
}

/// Box folder lock collection
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct BoxFolderLockCollection {
    entries: Vec<BoxFolderLock>,
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
    /// Server profile identifier owning these OAuth tokens. Empty when the
    /// caller has not bound a profile (legacy singleton key path). Issue #214.
    profile_id: String,
}

impl BoxProvider {
    pub fn new(config: BoxConfig) -> Self {
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
            current_path: "/".to_string(),
            current_folder_id: "0".to_string(),
            account_email: None,
            id_cache: {
                let mut m = HashMap::new();
                m.insert("/".to_string(), "0".to_string());
                m
            },
            profile_id: String::new(),
        }
    }

    /// Bind this provider to a server profile so OAuth tokens are stored
    /// under the per-profile vault key. Issue #214.
    pub fn with_profile_id(mut self, profile_id: impl Into<String>) -> Self {
        self.profile_id = profile_id.into();
        self
    }

    /// Get access token from OAuth manager (returns SecretString for memory zeroization)
    async fn get_token(&self) -> Result<secrecy::SecretString, ProviderError> {
        let config = OAuthConfig::box_cloud(&self.config.client_id, &self.config.client_secret)
            .with_profile_id(&self.profile_id);
        self.oauth_manager
            .get_valid_token(&config)
            .await
            .map_err(|e| ProviderError::AuthenticationFailed(format!("Box token error: {}", e)))
    }

    /// Get authorization header value
    fn bearer_header(token: &secrecy::SecretString) -> Result<HeaderValue, ProviderError> {
        use secrecy::ExposeSecret;
        HeaderValue::from_str(&format!("Bearer {}", token.expose_secret()))
            .map_err(|e| ProviderError::Other(format!("Invalid Bearer header: {}", e)))
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
            let url = format!(
                "{}/folders/{}/items?fields=name,type,id&limit=1000",
                API_BASE, current_id
            );
            let resp = self
                .client
                .get(&url)
                .header(AUTHORIZATION, Self::bearer_header(&token)?)
                .send()
                .await
                .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

            if !resp.status().is_success() {
                return Err(ProviderError::NotFound(format!("Path not found: {}", path)));
            }

            let items: BoxItemCollection = resp
                .json()
                .await
                .map_err(|e| ProviderError::ParseError(e.to_string()))?;

            // Box returns names in their stored ENCODED form (a name a user
            // could not otherwise create round-trips via reversible encoding),
            // so match the encoded leaf. The `id_cache` and error text keep the
            // user's original spelling.
            let encoded_part = crate::restricted_chars::encode_leaf(ProviderType::Box, part);
            let found = items
                .entries
                .iter()
                .find(|item| item.name == encoded_part && item.item_type == "folder");

            match found {
                Some(folder) => {
                    current_id = folder.id.clone();
                    self.id_cache.insert(built_path.clone(), current_id.clone());
                }
                None => {
                    return Err(ProviderError::NotFound(format!(
                        "Folder not found: {}",
                        part
                    )))
                }
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

        let url = format!(
            "{}/folders/{}/items?fields=name,type,id&limit=1000",
            API_BASE, parent_id
        );
        let resp = self
            .client
            .get(&url)
            .header(AUTHORIZATION, Self::bearer_header(&token)?)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        let items: BoxItemCollection = resp
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        let encoded_name = crate::restricted_chars::encode_leaf(ProviderType::Box, file_name);
        items
            .entries
            .iter()
            .find(|item| item.name == encoded_name)
            .map(|item| item.id.clone())
            .ok_or_else(|| ProviderError::NotFound(format!("File not found: {}", file_name)))
    }

    fn normalize_path(path: &str) -> String {
        let p = if path.starts_with('/') {
            path.to_string()
        } else {
            format!("/{}", path)
        };
        if p.len() > 1 {
            p.trim_end_matches('/').to_string()
        } else {
            p
        }
    }

    /// Resolve a path to (id, type) where type is "file" or "folder"
    async fn resolve_item_id_and_type(
        &mut self,
        path: &str,
    ) -> Result<(String, String), ProviderError> {
        let normalized = Self::normalize_path(path);
        let (parent_path, item_name) = match normalized.rfind('/') {
            Some(pos) if pos > 0 => (&normalized[..pos], &normalized[pos + 1..]),
            _ => ("/", normalized.trim_start_matches('/')),
        };

        let parent_id = self.resolve_folder_id(parent_path).await?;
        let token = self.get_token().await?;

        let url = format!(
            "{}/folders/{}/items?fields=name,type,id&limit=1000",
            API_BASE, parent_id
        );
        let resp = self
            .client
            .get(&url)
            .header(AUTHORIZATION, Self::bearer_header(&token)?)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        let items: BoxItemCollection = resp
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        let encoded_name = crate::restricted_chars::encode_leaf(ProviderType::Box, item_name);
        items
            .entries
            .iter()
            .find(|item| item.name == encoded_name)
            .map(|item| (item.id.clone(), item.item_type.clone()))
            .ok_or_else(|| ProviderError::NotFound(format!("Item not found: {}", item_name)))
    }

    // ── Box-Specific Features ───────────────────────────────────────

    /// List items in trash (paginated)
    pub async fn list_trash(&mut self) -> Result<Vec<RemoteEntry>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let mut all_entries = Vec::new();
        let mut offset: u64 = 0;
        const PAGE_LIMIT: u64 = 1000;

        loop {
            let token = self.get_token().await?;
            let url = format!(
                "{}/folders/trash/items?fields=name,type,id,size,modified_at,trashed_at&limit={}&offset={}",
                API_BASE, PAGE_LIMIT, offset
            );
            let resp = self
                .client
                .get(&url)
                .header(AUTHORIZATION, Self::bearer_header(&token)?)
                .send()
                .await
                .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

            if !resp.status().is_success() {
                return Err(box_error_from_response(resp, "List trash failed:").await);
            }

            let page: BoxTrashCollection = resp
                .json()
                .await
                .map_err(|e| ProviderError::ParseError(e.to_string()))?;

            for item in &page.entries {
                let mut metadata = HashMap::new();
                metadata.insert("id".to_string(), item.id.clone());
                metadata.insert("item_type".to_string(), item.item_type.clone());
                if let Some(ref t) = item.trashed_at {
                    metadata.insert("trashed_at".to_string(), t.clone());
                }

                let name = crate::restricted_chars::decode_leaf(ProviderType::Box, &item.name);
                all_entries.push(RemoteEntry {
                    path: format!("/Trash/{}", name),
                    name,
                    is_dir: item.item_type == "folder",
                    size: item.size.unwrap_or(0),
                    modified: item.modified_at.clone(),
                    permissions: None,
                    owner: None,
                    group: None,
                    is_symlink: false,
                    link_target: None,
                    mime_type: None,
                    metadata,
                });
            }

            if page.entries.is_empty() || all_entries.len() as u64 >= page.total_count {
                break;
            }
            offset += page.entries.len() as u64;
        }

        info!("Box: listed {} trash items", all_entries.len());
        Ok(all_entries)
    }

    /// Move files to trash (Box delete = soft delete to trash)
    pub async fn trash_files(&mut self, paths: &[String]) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        for path in paths {
            let (item_id, item_type) = self.resolve_item_id_and_type(path).await?;
            let token = self.get_token().await?;

            let endpoint = if item_type == "folder" {
                "folders"
            } else {
                "files"
            };
            let url = format!("{}/{}/{}", API_BASE, endpoint, item_id);

            let resp = self
                .client
                .delete(&url)
                .header(AUTHORIZATION, Self::bearer_header(&token)?)
                .send()
                .await
                .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

            if !resp.status().is_success() {
                return Err(box_error_from_response(resp, "Trash failed:").await);
            }
            info!("Box: trashed {} {}", item_type, path);
        }
        Ok(())
    }

    /// Restore an item from trash
    pub async fn restore_from_trash(
        &mut self,
        item_id: &str,
        item_type: &str,
    ) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let token = self.get_token().await?;
        let endpoint = if item_type == "folder" {
            "folders"
        } else {
            "files"
        };
        let url = format!("{}/{}/{}", API_BASE, endpoint, item_id);

        let resp = self
            .client
            .post(&url)
            .header(AUTHORIZATION, Self::bearer_header(&token)?)
            .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
            .body("{}")
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(box_error_from_response(resp, "Restore failed:").await);
        }
        info!("Box: restored {} {} from trash", item_type, item_id);
        Ok(())
    }

    /// Permanently delete an item from trash
    pub async fn permanent_delete_from_trash(
        &mut self,
        item_id: &str,
        item_type: &str,
    ) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let token = self.get_token().await?;
        let endpoint = if item_type == "folder" {
            "folders"
        } else {
            "files"
        };
        let url = format!("{}/{}/{}/trash", API_BASE, endpoint, item_id);

        let resp = self
            .client
            .delete(&url)
            .header(AUTHORIZATION, Self::bearer_header(&token)?)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(box_error_from_response(resp, "Permanent delete failed:").await);
        }
        info!(
            "Box: permanently deleted {} {} from trash",
            item_type, item_id
        );
        Ok(())
    }

    /// Move a file or folder to a different parent folder
    pub async fn move_item(
        &mut self,
        from_path: &str,
        to_folder: &str,
    ) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let (item_id, item_type) = self.resolve_item_id_and_type(from_path).await?;
        let target_id = self.resolve_folder_id(to_folder).await?;
        let token = self.get_token().await?;

        let endpoint = if item_type == "folder" {
            "folders"
        } else {
            "files"
        };
        let url = format!("{}/{}/{}", API_BASE, endpoint, item_id);

        let resp = self
            .client
            .put(&url)
            .header(AUTHORIZATION, Self::bearer_header(&token)?)
            .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
            .body(format!(r#"{{"parent":{{"id":"{}"}}}}"#, target_id))
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(box_error_from_response(resp, "Move failed:").await);
        }
        info!("Box: moved {} {} -> {}", item_type, from_path, to_folder);
        Ok(())
    }

    /// List comments on a file
    pub async fn list_comments(&mut self, path: &str) -> Result<Vec<BoxComment>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let file_id = self.resolve_file_id(path).await?;
        let token = self.get_token().await?;

        let url = format!(
            "{}/files/{}/comments?fields=message,created_at,created_by&limit=100",
            API_BASE, file_id
        );
        let resp = self
            .client
            .get(&url)
            .header(AUTHORIZATION, Self::bearer_header(&token)?)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(box_error_from_response(resp, "List comments failed:").await);
        }

        let result: BoxCommentCollection = resp
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        Ok(result.entries)
    }

    /// Add a comment to a file
    pub async fn add_comment(&mut self, path: &str, message: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let file_id = self.resolve_file_id(path).await?;
        let token = self.get_token().await?;

        let url = format!("{}/comments", API_BASE);
        let body = serde_json::json!({
            "item": { "id": file_id, "type": "file" },
            "message": message,
        });

        let resp = self
            .client
            .post(&url)
            .header(AUTHORIZATION, Self::bearer_header(&token)?)
            .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!(
                "Add comment failed: {}",
                sanitize_api_error(&body_text)
            )));
        }
        info!("Box: added comment to {}", path);
        Ok(())
    }

    /// Delete a comment by ID
    pub async fn delete_comment(&mut self, comment_id: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let token = self.get_token().await?;
        let url = format!("{}/comments/{}", API_BASE, comment_id);

        let resp = self
            .client
            .delete(&url)
            .header(AUTHORIZATION, Self::bearer_header(&token)?)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!(
                "Delete comment failed: {}",
                sanitize_api_error(&body)
            )));
        }
        info!("Box: deleted comment {}", comment_id);
        Ok(())
    }

    /// List collaborations on a file or folder
    pub async fn list_collaborations(
        &mut self,
        path: &str,
    ) -> Result<Vec<BoxCollaboration>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let (item_id, item_type) = self.resolve_item_id_and_type(path).await?;
        let token = self.get_token().await?;

        let endpoint = if item_type == "folder" {
            "folders"
        } else {
            "files"
        };
        let url = format!("{}/{}/{}/collaborations", API_BASE, endpoint, item_id);

        let resp = self
            .client
            .get(&url)
            .header(AUTHORIZATION, Self::bearer_header(&token)?)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!(
                "List collaborations failed: {}",
                sanitize_api_error(&body)
            )));
        }

        let result: BoxCollabCollection = resp
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        Ok(result.entries)
    }

    /// Add a collaboration (invite user by email)
    pub async fn add_collaboration(
        &mut self,
        path: &str,
        email: &str,
        role: &str,
    ) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let (item_id, item_type) = self.resolve_item_id_and_type(path).await?;
        let token = self.get_token().await?;

        let url = format!("{}/collaborations", API_BASE);
        let body = serde_json::json!({
            "item": { "id": item_id, "type": item_type },
            "accessible_by": { "type": "user", "login": email },
            "role": role,
        });

        let resp = self
            .client
            .post(&url)
            .header(AUTHORIZATION, Self::bearer_header(&token)?)
            .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!(
                "Add collaboration failed: {}",
                sanitize_api_error(&body_text)
            )));
        }
        info!("Box: added collaboration for {} on {}", email, path);
        Ok(())
    }

    /// Remove a collaboration by ID
    pub async fn remove_collaboration(&mut self, collab_id: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let token = self.get_token().await?;
        let url = format!("{}/collaborations/{}", API_BASE, collab_id);

        let resp = self
            .client
            .delete(&url)
            .header(AUTHORIZATION, Self::bearer_header(&token)?)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!(
                "Remove collaboration failed: {}",
                sanitize_api_error(&body)
            )));
        }
        info!("Box: removed collaboration {}", collab_id);
        Ok(())
    }

    /// Apply watermark to a file
    pub async fn set_watermark(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let file_id = self.resolve_file_id(path).await?;
        let token = self.get_token().await?;

        let url = format!("{}/files/{}/watermark", API_BASE, file_id);
        let body = r#"{"watermark":{"imprint":"default"}}"#;

        let resp = self
            .client
            .put(&url)
            .header(AUTHORIZATION, Self::bearer_header(&token)?)
            .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
            .body(body)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!(
                "Set watermark failed: {}",
                sanitize_api_error(&body_text)
            )));
        }
        info!("Box: watermark applied to {}", path);
        Ok(())
    }

    /// Remove watermark from a file
    pub async fn remove_watermark(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let file_id = self.resolve_file_id(path).await?;
        let token = self.get_token().await?;

        let url = format!("{}/files/{}/watermark", API_BASE, file_id);

        let resp = self
            .client
            .delete(&url)
            .header(AUTHORIZATION, Self::bearer_header(&token)?)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!(
                "Remove watermark failed: {}",
                sanitize_api_error(&body)
            )));
        }
        info!("Box: watermark removed from {}", path);
        Ok(())
    }

    /// Set tags on a file or folder
    pub async fn set_tags(&mut self, path: &str, tags: &[String]) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let (item_id, item_type) = self.resolve_item_id_and_type(path).await?;
        let token = self.get_token().await?;

        let endpoint = if item_type == "folder" {
            "folders"
        } else {
            "files"
        };
        let url = format!("{}/{}/{}", API_BASE, endpoint, item_id);
        let body = serde_json::json!({ "tags": tags });

        let resp = self
            .client
            .put(&url)
            .header(AUTHORIZATION, Self::bearer_header(&token)?)
            .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!(
                "Set tags failed: {}",
                sanitize_api_error(&body_text)
            )));
        }
        info!("Box: tags set on {} -> {:?}", path, tags);
        Ok(())
    }

    /// Lock a folder (prevent move/delete)
    pub async fn lock_folder(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let folder_id = self.resolve_folder_id(path).await?;
        let token = self.get_token().await?;

        let url = format!("{}/folder_locks", API_BASE);
        let body = serde_json::json!({
            "folder": { "type": "folder", "id": folder_id },
            "locked_operations": { "move": true, "delete": true },
        });

        let resp = self
            .client
            .post(&url)
            .header(AUTHORIZATION, Self::bearer_header(&token)?)
            .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!(
                "Lock folder failed: {}",
                sanitize_api_error(&body_text)
            )));
        }
        info!("Box: folder locked {}", path);
        Ok(())
    }

    /// Unlock a folder by lock ID
    pub async fn unlock_folder(&mut self, lock_id: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let token = self.get_token().await?;
        let url = format!("{}/folder_locks/{}", API_BASE, lock_id);

        let resp = self
            .client
            .delete(&url)
            .header(AUTHORIZATION, Self::bearer_header(&token)?)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!(
                "Unlock folder failed: {}",
                sanitize_api_error(&body)
            )));
        }
        info!("Box: folder unlocked (lock {})", lock_id);
        Ok(())
    }

    /// List folder locks
    pub async fn list_folder_locks(
        &mut self,
        path: &str,
    ) -> Result<Vec<BoxFolderLock>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let folder_id = self.resolve_folder_id(path).await?;
        let token = self.get_token().await?;

        let url = format!("{}/folder_locks?folder_id={}", API_BASE, folder_id);

        let resp = self
            .client
            .get(&url)
            .header(AUTHORIZATION, Self::bearer_header(&token)?)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!(
                "List folder locks failed: {}",
                sanitize_api_error(&body)
            )));
        }

        let result: BoxFolderLockCollection = resp
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        Ok(result.entries)
    }

    /// Open a Box chunked upload session for `remote_path` of `total_size`
    /// bytes and return the parsed session metadata. Handles the new-file
    /// vs new-version branch (Box returns `item_name_in_use` and we retry
    /// against `/files/<id>/upload_sessions`). Shared by the legacy
    /// `chunked_upload_session` orchestrator and the shaped-graph trait
    /// path so both agree on session lifecycle.
    async fn open_chunked_upload_session(
        &mut self,
        remote_path: &str,
        total_size: u64,
    ) -> Result<BoxMultipartMeta, ProviderError> {
        #[derive(Deserialize)]
        struct UploadSessionResponse {
            id: String,
            part_size: u64,
            session_endpoints: Option<SessionEndpoints>,
        }
        #[derive(Deserialize)]
        struct SessionEndpoints {
            upload_part: Option<String>,
            commit: Option<String>,
        }

        let normalized = Self::normalize_path(remote_path);
        let (parent_path, file_name) = match normalized.rfind('/') {
            Some(pos) if pos > 0 => (&normalized[..pos], &normalized[pos + 1..]),
            _ => ("/", normalized.trim_start_matches('/')),
        };
        let parent_id = self.resolve_folder_id(parent_path).await?;
        // Store the new leaf under its reversible encoded form.
        let file_name = crate::restricted_chars::encode_leaf(ProviderType::Box, file_name);

        let session_body = serde_json::json!({
            "file_name": file_name,
            "folder_id": parent_id,
            "file_size": total_size,
        });
        let session_url = format!("{}/files/upload_sessions", UPLOAD_BASE);

        let token = self.get_token().await?;
        let response = self
            .client
            .post(&session_url)
            .header(AUTHORIZATION, Self::bearer_header(&token)?)
            .header(CONTENT_TYPE, "application/json")
            .body(session_body.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        let session_resp = if response.status().is_success() {
            response
        } else {
            // Box returns `item_name_in_use` when the file already exists:
            // open a new-version session against the existing file id.
            let text = response.text().await.unwrap_or_default();
            if text.contains("item_name_in_use") {
                let file_id = self.resolve_file_id(remote_path).await?;
                let ver_body = serde_json::json!({ "file_size": total_size });
                let ver_url = format!("{}/files/{}/upload_sessions", UPLOAD_BASE, file_id);
                let token2 = self.get_token().await?;
                let ver_resp = self
                    .client
                    .post(&ver_url)
                    .header(AUTHORIZATION, Self::bearer_header(&token2)?)
                    .header(CONTENT_TYPE, "application/json")
                    .body(ver_body.to_string())
                    .send()
                    .await
                    .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
                if !ver_resp.status().is_success() {
                    let t = ver_resp.text().await.unwrap_or_default();
                    return Err(ProviderError::TransferFailed(format!(
                        "Box upload_sessions (version) failed: {}",
                        sanitize_api_error(&t)
                    )));
                }
                ver_resp
            } else {
                return Err(ProviderError::TransferFailed(format!(
                    "Box upload_sessions failed: {}",
                    sanitize_api_error(&text)
                )));
            }
        };

        let parsed: UploadSessionResponse = session_resp
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        let upload_part_url = parsed
            .session_endpoints
            .as_ref()
            .and_then(|e| e.upload_part.clone())
            .unwrap_or_else(|| format!("{}/files/upload_sessions/{}", UPLOAD_BASE, parsed.id));
        let commit_url = parsed
            .session_endpoints
            .as_ref()
            .and_then(|e| e.commit.clone())
            .unwrap_or_else(|| {
                format!("{}/files/upload_sessions/{}/commit", UPLOAD_BASE, parsed.id)
            });

        Ok(BoxMultipartMeta {
            session_id: parsed.id,
            upload_part_url,
            commit_url,
            total: total_size,
            part: parsed.part_size,
            local_path: String::new(),
        })
    }

    /// Upload data in chunks via a Box upload session, then commit
    #[allow(clippy::type_complexity)]
    async fn chunked_upload_session(
        &self,
        session_resp: reqwest::Response,
        local_path: &str,
        total_size: u64,
        on_progress: Option<std::sync::Arc<std::sync::Mutex<Box<dyn Fn(u64, u64) + Send>>>>,
    ) -> Result<(), ProviderError> {
        use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
        use sha1::{Digest, Sha1};
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

        let session: UploadSession = session_resp
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        let chunk_size = session.part_size as usize;
        let upload_part_url = session
            .session_endpoints
            .as_ref()
            .and_then(|e| e.upload_part.clone())
            .unwrap_or_else(|| format!("{}/files/upload_sessions/{}", UPLOAD_BASE, session.id));
        let commit_url = session
            .session_endpoints
            .as_ref()
            .and_then(|e| e.commit.clone())
            .unwrap_or_else(|| {
                format!(
                    "{}/files/upload_sessions/{}/commit",
                    UPLOAD_BASE, session.id
                )
            });

        // Stream from file handle instead of buffered &[u8]
        let mut file = tokio::fs::File::open(local_path)
            .await
            .map_err(ProviderError::IoError)?;
        let mut parts: Vec<serde_json::Value> = Vec::new();
        let mut offset: u64 = 0;
        let mut whole_sha1 = Sha1::new();

        while offset < total_size {
            let remaining = (total_size - offset) as usize;
            let this_chunk = std::cmp::min(chunk_size, remaining);
            let mut chunk = vec![0u8; this_chunk];
            file.read_exact(&mut chunk)
                .await
                .map_err(ProviderError::IoError)?;

            let chunk_sha1 = {
                let mut h = Sha1::new();
                h.update(&chunk);
                BASE64.encode(h.finalize())
            };
            whole_sha1.update(&chunk);

            let token = self.get_token().await?;
            let end = offset + this_chunk as u64;
            let content_range = format!("bytes {}-{}/{}", offset, end - 1, total_size);

            let resp = self
                .client
                .put(&upload_part_url)
                .header(AUTHORIZATION, Self::bearer_header(&token)?)
                .header(CONTENT_TYPE, "application/octet-stream")
                .header("Content-Range", &content_range)
                .header("Digest", format!("sha={}", chunk_sha1))
                .body(chunk)
                .send()
                .await
                .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

            if !resp.status().is_success() {
                let t = resp.text().await.unwrap_or_default();
                return Err(ProviderError::TransferFailed(format!(
                    "Chunk upload failed: {}",
                    sanitize_api_error(&t)
                )));
            }

            let part_resp: serde_json::Value = resp
                .json()
                .await
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

        let resp = self
            .client
            .post(&commit_url)
            .header(AUTHORIZATION, Self::bearer_header(&token)?)
            .header(CONTENT_TYPE, "application/json")
            .header("Digest", format!("sha={}", file_sha1))
            .json(&commit_body)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() && resp.status().as_u16() != 201 {
            let t = resp.text().await.unwrap_or_default();
            return Err(ProviderError::TransferFailed(format!(
                "Commit failed: {}",
                sanitize_api_error(&t)
            )));
        }

        Ok(())
    }
}

#[async_trait]
impl StorageProvider for BoxProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn provider_type(&self) -> ProviderType {
        ProviderType::Box
    }

    fn display_name(&self) -> String {
        "Box".to_string()
    }

    fn account_email(&self) -> Option<String> {
        self.account_email.clone()
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        // Verify token works
        let token = self.get_token().await?;

        let resp = self
            .client
            .get(format!("{}/users/me", API_BASE))
            .header(AUTHORIZATION, Self::bearer_header(&token)?)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(ProviderError::AuthenticationFailed(
                "Box authentication failed".to_string(),
            ));
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
                "{}/folders/{}/items?fields=name,type,id,size,modified_at,watermark_info,tags,sha1&limit={}&offset={}",
                API_BASE, folder_id, PAGE_LIMIT, offset
            );

            let resp = self
                .client
                .get(&url)
                .header(AUTHORIZATION, Self::bearer_header(&token)?)
                .send()
                .await
                .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                return Err(ProviderError::ServerError(format!(
                    "Box API error {}: {}",
                    status,
                    sanitize_api_error(&body)
                )));
            }

            let page: BoxItemCollection = resp
                .json()
                .await
                .map_err(|e| ProviderError::ParseError(e.to_string()))?;

            let page_count = page.entries.len() as u64;
            all_items.extend(page.entries);

            // Stop if we've fetched all items or no more entries returned
            if all_items.len() as u64 >= page.total_count || page_count == 0 {
                break;
            }
            offset += page_count;
        }

        let entries = all_items
            .into_iter()
            .map(|item| {
                let is_dir = item.item_type == "folder";
                // Decode the stored name to the user's original spelling, and
                // build the path from it so it (and the `id_cache` key below,
                // which `resolve_folder_id` looks up in original form) stay
                // consistent with the rest of the provider.
                let name = crate::restricted_chars::decode_leaf(ProviderType::Box, &item.name);
                let entry_path = if base_path == "/" {
                    format!("/{}", name)
                } else {
                    format!("{}/{}", base_path, name)
                };
                let is_watermarked = item
                    .watermark_info
                    .as_ref()
                    .and_then(|wi| wi.is_watermarked)
                    .unwrap_or(false);

                RemoteEntry {
                    name,
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
                        if is_watermarked {
                            m.insert("watermarked".to_string(), "true".to_string());
                        }
                        if !item.tags.is_empty() {
                            m.insert("box_tags".to_string(), item.tags.join(","));
                        }
                        if !is_dir {
                            if let Some(ref s) = item.sha1 {
                                m.insert("sha1".to_string(), s.to_ascii_lowercase());
                            }
                        }
                        m
                    },
                }
            })
            .collect::<Vec<_>>();

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
            let base = if self.current_path == "/" {
                String::new()
            } else {
                self.current_path.clone()
            };
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

    async fn download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        _progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        use futures_util::StreamExt;

        let file_id = self.resolve_file_id(remote_path).await?;
        let token = self.get_token().await?;

        let url = format!("{}/files/{}/content", API_BASE, file_id);
        let resp = self
            .client
            .get(&url)
            .header(AUTHORIZATION, Self::bearer_header(&token)?)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(ProviderError::TransferFailed(format!(
                "Download failed: {}",
                resp.status()
            )));
        }

        let mut stream = resp.bytes_stream();
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
        let file_id = self.resolve_file_id(remote_path).await?;
        let token = self.get_token().await?;
        let url = format!("{}/files/{}/content", API_BASE, file_id);
        let auth = Self::bearer_header(&token)?;

        super::http_resumable_download(
            local_path,
            |range_header| {
                let mut req = self.client.get(&url).header(AUTHORIZATION, auth.clone());
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
        let file_id = self.resolve_file_id(remote_path).await?;
        let token = self.get_token().await?;

        let url = format!("{}/files/{}/content", API_BASE, file_id);
        let resp = self
            .client
            .get(&url)
            .header(AUTHORIZATION, Self::bearer_header(&token)?)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(ProviderError::TransferFailed(format!(
                "Download failed: {}",
                resp.status()
            )));
        }

        // H2: Size-limited download to prevent OOM on large files
        super::response_bytes_with_limit(resp, super::MAX_DOWNLOAD_TO_BYTES).await
    }

    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let normalized = Self::normalize_path(remote_path);
        let (parent_path, file_name) = match normalized.rfind('/') {
            Some(pos) if pos > 0 => (&normalized[..pos], &normalized[pos + 1..]),
            _ => ("/", normalized.trim_start_matches('/')),
        };
        // Store the new leaf under its reversible encoded form (used by both the
        // chunked-session and the small-file multipart code paths below).
        let file_name = crate::restricted_chars::encode_leaf(ProviderType::Box, file_name);

        let parent_id = self.resolve_folder_id(parent_path).await?;
        let total_size = tokio::fs::metadata(local_path)
            .await
            .map_err(ProviderError::IoError)?
            .len();

        const CHUNKED_THRESHOLD: u64 = 50 * 1024 * 1024; // 50MB

        if total_size > CHUNKED_THRESHOLD {
            // Chunked upload session for large files: stream from file handle
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
            let session_resp = self
                .client
                .post(&session_url)
                .header(AUTHORIZATION, Self::bearer_header(&token)?)
                .header(CONTENT_TYPE, "application/json")
                .body(session_body.to_string())
                .send()
                .await
                .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

            if !session_resp.status().is_success() {
                let text = session_resp.text().await.unwrap_or_default();
                // If conflict, try upload new version session
                if text.contains("item_name_in_use") {
                    let file_id = self.resolve_file_id(remote_path).await?;
                    let token2 = self.get_token().await?;
                    let ver_body = serde_json::json!({"file_size": total_size});
                    let ver_url = format!("{}/files/{}/upload_sessions", UPLOAD_BASE, file_id);
                    let ver_resp = self
                        .client
                        .post(&ver_url)
                        .header(AUTHORIZATION, Self::bearer_header(&token2)?)
                        .header(CONTENT_TYPE, "application/json")
                        .body(ver_body.to_string())
                        .send()
                        .await
                        .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

                    if !ver_resp.status().is_success() {
                        let t = ver_resp.text().await.unwrap_or_default();
                        return Err(ProviderError::TransferFailed(format!(
                            "Upload session failed: {}",
                            sanitize_api_error(&t)
                        )));
                    }

                    return self
                        .chunked_upload_session(ver_resp, local_path, total_size, progress.clone())
                        .await;
                }
                return Err(ProviderError::TransferFailed(format!(
                    "Upload session failed: {}",
                    sanitize_api_error(&text)
                )));
            }

            return self
                .chunked_upload_session(session_resp, local_path, total_size, progress)
                .await;
        }

        // Simple multipart upload for small files (<=50MB, OK to buffer)
        let data = tokio::fs::read(local_path)
            .await
            .map_err(ProviderError::IoError)?;
        let token = self.get_token().await?;
        let attributes = serde_json::json!({
            "name": file_name,
            "parent": {"id": parent_id}
        });

        let form = reqwest::multipart::Form::new()
            .text("attributes", attributes.to_string())
            .part(
                "file",
                reqwest::multipart::Part::bytes(data).file_name(file_name.to_string()),
            );

        let url = format!("{}/files/content", UPLOAD_BASE);
        let resp = self
            .client
            .post(&url)
            .header(AUTHORIZATION, Self::bearer_header(&token)?)
            .multipart(form)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            if body.contains("item_name_in_use") {
                let file_id = self.resolve_file_id(remote_path).await?;
                let data2 = tokio::fs::read(local_path)
                    .await
                    .map_err(ProviderError::IoError)?;
                let token2 = self.get_token().await?;

                let form2 = reqwest::multipart::Form::new().part(
                    "file",
                    reqwest::multipart::Part::bytes(data2).file_name(file_name.to_string()),
                );

                let url2 = format!("{}/files/{}/content", UPLOAD_BASE, file_id);
                let resp2 = self
                    .client
                    .post(&url2)
                    .header(AUTHORIZATION, Self::bearer_header(&token2)?)
                    .multipart(form2)
                    .send()
                    .await
                    .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

                if !resp2.status().is_success() {
                    return Err(ProviderError::TransferFailed(format!(
                        "Upload version failed: {}",
                        resp2.status()
                    )));
                }
            } else {
                return Err(ProviderError::TransferFailed(format!(
                    "Upload failed: {}",
                    sanitize_api_error(&body)
                )));
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
        let folder_name = crate::restricted_chars::encode_leaf(ProviderType::Box, folder_name);

        let parent_id = self.resolve_folder_id(parent_path).await?;
        let token = self.get_token().await?;

        let body = serde_json::json!({
            "name": folder_name,
            "parent": {"id": parent_id}
        });

        let resp = self
            .client
            .post(format!("{}/folders", API_BASE))
            .header(AUTHORIZATION, Self::bearer_header(&token)?)
            .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!(
                "mkdir failed: {}",
                sanitize_api_error(&body)
            )));
        }

        Ok(())
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        let file_id = self.resolve_file_id(path).await?;
        let token = self.get_token().await?;

        let resp = self
            .client
            .delete(format!("{}/files/{}", API_BASE, file_id))
            .header(AUTHORIZATION, Self::bearer_header(&token)?)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() && resp.status().as_u16() != 204 {
            return Err(ProviderError::Other(format!(
                "Delete failed: {}",
                resp.status()
            )));
        }

        Ok(())
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let folder_id = self.resolve_folder_id(path).await?;
        let token = self.get_token().await?;

        let resp = self
            .client
            .delete(format!("{}/folders/{}?recursive=true", API_BASE, folder_id))
            .header(AUTHORIZATION, Self::bearer_header(&token)?)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() && resp.status().as_u16() != 204 {
            return Err(ProviderError::Other(format!(
                "rmdir failed: {}",
                resp.status()
            )));
        }

        // Remove from cache
        let normalized = Self::normalize_path(path);
        self.id_cache.remove(&normalized);

        Ok(())
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        self.rmdir(path).await // Box API handles recursive by default
    }

    async fn delete_permanent(&mut self, path: &str) -> Result<bool, ProviderError> {
        // Box trash listing carries id and item_type ("file"|"folder") in
        // metadata. Match by basename and dispatch to the existing inherent
        // helper. Ok(false) when the trash search returns no match.
        let basename = path
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or(path);
        if basename.is_empty() {
            return Ok(false);
        }
        let trashed = self.list_trash().await?;
        let target = trashed.iter().find(|e| e.name == basename).and_then(|e| {
            let id = e.metadata.get("id")?;
            let kind = e.metadata.get("item_type")?;
            Some((id.clone(), kind.clone()))
        });
        match target {
            Some((id, kind)) => {
                self.permanent_delete_from_trash(&id, &kind).await?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        // Supports both simple rename and cross-folder move+rename
        let token = self.get_token().await?;
        let new_name = std::path::Path::new(to)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| to.to_string());
        // Store the new leaf encoded (used by both the file and folder branches).
        let new_name = crate::restricted_chars::encode_leaf(ProviderType::Box, &new_name);

        // Resolve destination parent folder ID for cross-folder moves
        let from_parent = std::path::Path::new(from)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let to_parent = std::path::Path::new(to)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

        let cross_folder = from_parent != to_parent;

        if let Ok(file_id) = self.resolve_file_id(from).await {
            let mut body = serde_json::json!({"name": new_name});
            if cross_folder {
                let dest_folder_id = if to_parent.is_empty() || to_parent == "/" {
                    "0".to_string()
                } else {
                    self.resolve_folder_id(&to_parent).await?
                };
                body["parent"] = serde_json::json!({"id": dest_folder_id});
            }
            let resp = self
                .client
                .put(format!("{}/files/{}", API_BASE, file_id))
                .header(AUTHORIZATION, Self::bearer_header(&token)?)
                .json(&body)
                .send()
                .await
                .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

            if !resp.status().is_success() {
                let text = resp.text().await.unwrap_or_default();
                return Err(ProviderError::Other(format!("Rename failed: {}", text)));
            }
        } else {
            let folder_id = self.resolve_folder_id(from).await?;
            let mut body = serde_json::json!({"name": new_name});
            if cross_folder {
                let dest_folder_id = if to_parent.is_empty() || to_parent == "/" {
                    "0".to_string()
                } else {
                    self.resolve_folder_id(&to_parent).await?
                };
                body["parent"] = serde_json::json!({"id": dest_folder_id});
            }
            let resp = self
                .client
                .put(format!("{}/folders/{}", API_BASE, folder_id))
                .header(AUTHORIZATION, Self::bearer_header(&token)?)
                .json(&body)
                .send()
                .await
                .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

            if !resp.status().is_success() {
                let text = resp.text().await.unwrap_or_default();
                return Err(ProviderError::Other(format!("Rename failed: {}", text)));
            }
        }

        Ok(())
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        // Try file first
        if let Ok(file_id) = self.resolve_file_id(path).await {
            let token = self.get_token().await?;
            let resp = self
                .client
                .get(format!(
                    "{}/files/{}?fields=name,type,size,modified_at,sha1",
                    API_BASE, file_id
                ))
                .header(AUTHORIZATION, Self::bearer_header(&token)?)
                .send()
                .await
                .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

            let item: BoxItem = resp
                .json()
                .await
                .map_err(|e| ProviderError::ParseError(e.to_string()))?;

            let mut metadata = HashMap::new();
            if let Some(ref s) = item.sha1 {
                metadata.insert("sha1".to_string(), s.to_ascii_lowercase());
            }
            return Ok(RemoteEntry {
                name: crate::restricted_chars::decode_leaf(ProviderType::Box, &item.name),
                path: Self::normalize_path(path),
                is_dir: false,
                size: item.size.unwrap_or(0),
                modified: item.modified_at,
                permissions: None,
                owner: None,
                group: None,
                is_symlink: false,
                link_target: None,
                mime_type: None,
                metadata,
            });
        }

        // Try folder
        let folder_id = self.resolve_folder_id(path).await?;
        let token = self.get_token().await?;
        let resp = self
            .client
            .get(format!(
                "{}/folders/{}?fields=name,type,size,modified_at",
                API_BASE, folder_id
            ))
            .header(AUTHORIZATION, Self::bearer_header(&token)?)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        let item: BoxItem = resp
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        Ok(RemoteEntry {
            name: crate::restricted_chars::decode_leaf(ProviderType::Box, &item.name),
            path: Self::normalize_path(path),
            is_dir: true,
            size: 0,
            modified: item.modified_at,
            permissions: None,
            owner: None,
            group: None,
            is_symlink: false,
            link_target: None,
            mime_type: None,
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

    fn supports_checksum(&self) -> bool {
        true
    }

    /// Server-computed SHA-1 from the Box file metadata (one files.get
    /// call, no content download).
    async fn checksum(&mut self, path: &str) -> Result<HashMap<String, String>, ProviderError> {
        let entry = self.stat(path).await?;
        let mut out = HashMap::new();
        if let Some(v) = entry.metadata.get("sha1") {
            out.insert("sha1".to_string(), v.clone());
        }
        Ok(out)
    }

    async fn size(&mut self, path: &str) -> Result<u64, ProviderError> {
        let entry = self.stat(path).await?;
        Ok(entry.size)
    }

    async fn keep_alive(&mut self) -> Result<(), ProviderError> {
        Ok(())
    }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        Ok("Box Cloud Storage (api.box.com)".to_string())
    }

    // Storage info
    async fn storage_info(&mut self) -> Result<StorageInfo, ProviderError> {
        let token = self.get_token().await?;
        let resp = self
            .client
            .get(format!(
                "{}/users/me?fields=space_amount,space_used",
                API_BASE
            ))
            .header(AUTHORIZATION, Self::bearer_header(&token)?)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        let user: BoxUser = resp
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        Ok(StorageInfo {
            total: user.space_amount,
            used: user.space_used,
            free: user.space_amount.saturating_sub(user.space_used),
            versioning_bytes: None,
        })
    }

    // Share links
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

    async fn list_share_links(&mut self, path: &str) -> Result<Vec<ShareLinkInfo>, ProviderError> {
        let file_id = self.resolve_file_id(path).await?;
        let token = self.get_token().await?;

        let resp = self
            .client
            .get(format!("{}/files/{}?fields=shared_link", API_BASE, file_id))
            .header(AUTHORIZATION, Self::bearer_header(&token)?)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!(
                "Failed to get file shared link: {}",
                sanitize_api_error(&text)
            )));
        }

        let item: BoxItemWithLink = resp
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        if let Some(link) = item.shared_link {
            Ok(vec![ShareLinkInfo {
                id: file_id.clone(),
                url: link.url,
                created_at: None,
                expires_at: link.unshared_at,
                password_protected: link.is_password_enabled.unwrap_or(false),
                permissions: link.effective_access,
            }])
        } else {
            Ok(vec![])
        }
    }

    async fn create_share_link(
        &mut self,
        path: &str,
        options: ShareLinkOptions,
    ) -> Result<ShareLinkResult, ProviderError> {
        let file_id = self.resolve_file_id(path).await?;
        let token = self.get_token().await?;

        // Box API: shared_link with access, password, unshared_at, permissions
        // password and unshared_at require paid Box account
        let mut shared_link = serde_json::json!({"access": "open"});
        if let Some(ref pw) = options.password {
            shared_link["password"] = serde_json::json!(pw);
        }
        if let Some(secs) = options.expires_in_secs {
            let expires_at = chrono::Utc::now() + chrono::Duration::seconds(secs as i64);
            shared_link["unshared_at"] =
                serde_json::json!(expires_at.format("%Y-%m-%dT%H:%M:%S-00:00").to_string());
        }
        if let Some(ref perms) = options.permissions {
            let can_edit = perms == "edit";
            shared_link["permissions"] = serde_json::json!({
                "can_download": true,
                "can_edit": can_edit
            });
        }
        let body = serde_json::json!({"shared_link": shared_link});

        let resp = self
            .client
            .put(format!("{}/files/{}?fields=shared_link", API_BASE, file_id))
            .header(AUTHORIZATION, Self::bearer_header(&token)?)
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        let item: BoxItemWithLink = resp
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        let url = item
            .shared_link
            .map(|l| l.url)
            .ok_or_else(|| ProviderError::Other("Failed to create share link".to_string()))?;

        Ok(ShareLinkResult {
            url,
            password: None,
            expires_at: None,
        })
    }

    async fn remove_share_link(&mut self, path: &str) -> Result<(), ProviderError> {
        let file_id = self.resolve_file_id(path).await?;
        let token = self.get_token().await?;

        let body = serde_json::json!({"shared_link": null});
        let _resp = self
            .client
            .put(format!("{}/files/{}", API_BASE, file_id))
            .header(AUTHORIZATION, Self::bearer_header(&token)?)
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        Ok(())
    }

    fn supports_server_copy(&self) -> bool {
        true
    }

    fn supports_server_side_copy(&self) -> bool {
        true
    }

    async fn server_copy(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        // Legacy alias kept so CLI / MCP / provider_commands callers keep
        // working. The real `/files/<id>/copy` + `/folders/<id>/copy`
        // implementation lives on `server_side_copy` (S3-T10 migration, v4.0.0).
        StorageProvider::server_side_copy(self, from, to).await
    }

    async fn server_side_copy(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let token = self.get_token().await?;
        let to_normalized = Self::normalize_path(to);
        let (to_parent, to_name) = match to_normalized.rfind('/') {
            Some(pos) if pos > 0 => (&to_normalized[..pos], &to_normalized[pos + 1..]),
            _ => ("/", to_normalized.trim_start_matches('/')),
        };
        // Store the copy's new leaf encoded (used by file and folder branches).
        let to_name = crate::restricted_chars::encode_leaf(ProviderType::Box, to_name);
        let to_parent_id = self.resolve_folder_id(to_parent).await?;

        // Try file copy first
        if let Ok(file_id) = self.resolve_file_id(from).await {
            let body = serde_json::json!({
                "parent": {"id": to_parent_id},
                "name": to_name
            });
            let resp = self
                .client
                .post(format!("{}/files/{}/copy", API_BASE, file_id))
                .header(AUTHORIZATION, Self::bearer_header(&token)?)
                .json(&body)
                .send()
                .await
                .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

            if !resp.status().is_success() {
                return Err(ProviderError::Other(format!(
                    "Copy failed: {}",
                    resp.status()
                )));
            }
            return Ok(());
        }

        // Try folder copy
        let folder_id = self.resolve_folder_id(from).await?;
        let body = serde_json::json!({
            "parent": {"id": to_parent_id},
            "name": to_name
        });
        let resp = self
            .client
            .post(format!("{}/folders/{}/copy", API_BASE, folder_id))
            .header(AUTHORIZATION, Self::bearer_header(&token)?)
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(ProviderError::Other(format!(
                "Copy failed: {}",
                resp.status()
            )));
        }
        Ok(())
    }

    fn supports_thumbnails(&self) -> bool {
        true
    }

    async fn get_thumbnail(&mut self, path: &str) -> Result<String, ProviderError> {
        let file_id = self.resolve_file_id(path).await?;
        let token = self.get_token().await?;

        let url = format!(
            "{}/files/{}/thumbnail.png?min_height=256&min_width=256",
            API_BASE, file_id
        );
        let resp = self
            .client
            .get(&url)
            .header(AUTHORIZATION, Self::bearer_header(&token)?)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(ProviderError::NotFound(
                "No thumbnail available".to_string(),
            ));
        }

        let bytes = resp
            .bytes()
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
        Ok(format!("data:image/png;base64,{}", BASE64.encode(&bytes)))
    }

    fn supports_versions(&self) -> bool {
        true
    }

    async fn list_versions(&mut self, path: &str) -> Result<Vec<FileVersion>, ProviderError> {
        let file_id = self.resolve_file_id(path).await?;
        let token = self.get_token().await?;

        let url = format!("{}/files/{}/versions", API_BASE, file_id);
        let resp = self
            .client
            .get(&url)
            .header(AUTHORIZATION, Self::bearer_header(&token)?)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(ProviderError::Other(format!(
                "Failed to list versions: {}",
                resp.status()
            )));
        }

        let versions: BoxVersionCollection = resp
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        Ok(versions
            .entries
            .into_iter()
            .map(|v| FileVersion {
                id: v.id,
                modified: v.modified_at,
                size: v.size.unwrap_or(0),
                modified_by: v.modified_by.and_then(|u| u.name),
            })
            .collect())
    }

    async fn download_version(
        &mut self,
        path: &str,
        version_id: &str,
        local_path: &str,
    ) -> Result<(), ProviderError> {
        let file_id = self.resolve_file_id(path).await?;
        let token = self.get_token().await?;

        let url = format!(
            "{}/files/{}/content?version={}",
            API_BASE, file_id, version_id
        );
        let resp = self
            .client
            .get(&url)
            .header(AUTHORIZATION, Self::bearer_header(&token)?)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(ProviderError::TransferFailed(format!(
                "Version download failed: {}",
                resp.status()
            )));
        }

        let bytes = resp
            .bytes()
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        tokio::fs::write(local_path, &bytes)
            .await
            .map_err(ProviderError::IoError)?;

        Ok(())
    }

    async fn restore_version(&mut self, path: &str, version_id: &str) -> Result<(), ProviderError> {
        let file_id = self.resolve_file_id(path).await?;
        let token = self.get_token().await?;

        let body = serde_json::json!({"id": version_id});
        let resp = self
            .client
            .post(format!("{}/files/{}/versions/current", API_BASE, file_id))
            .header(AUTHORIZATION, Self::bearer_header(&token)?)
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(ProviderError::Other(format!(
                "Restore version failed: {}",
                resp.status()
            )));
        }
        Ok(())
    }

    fn supports_find(&self) -> bool {
        true
    }

    async fn find(
        &mut self,
        _path: &str,
        pattern: &str,
    ) -> Result<Vec<RemoteEntry>, ProviderError> {
        let token = self.get_token().await?;

        // Box /search?query= is substring on filename and metadata. Strip
        // glob metachars for the broad server prefilter, then apply the
        // precise glob/substring filter client-side via matches_find_pattern.
        let literal: String = pattern
            .chars()
            .filter(|c| !matches!(c, '*' | '?' | '[' | ']'))
            .collect();
        let server_query = if literal.is_empty() {
            ".".to_string()
        } else {
            literal
        };

        let url = format!(
            "{}/search?query={}&limit=200",
            API_BASE,
            urlencoding::encode(&server_query)
        );
        let resp = self
            .client
            .get(&url)
            .header(AUTHORIZATION, Self::bearer_header(&token)?)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(ProviderError::Other(format!(
                "Search failed: {}",
                resp.status()
            )));
        }

        let results: BoxSearchResult = resp
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        Ok(results
            .entries
            .into_iter()
            .filter_map(|item| {
                // Match the user's pattern against the DECODED name (the stored
                // name is encoded), and surface the decoded name to the user.
                let name = crate::restricted_chars::decode_leaf(ProviderType::Box, &item.name);
                if !super::matches_find_pattern(&name, pattern) {
                    return None;
                }
                Some(RemoteEntry {
                    name,
                    path: String::new(), // Box search doesn't return full path
                    is_dir: item.item_type == "folder",
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
                })
            })
            .collect())
    }

    fn transfer_optimization_hints(&self) -> super::TransferOptimizationHints {
        super::TransferOptimizationHints {
            supports_multipart: true,
            multipart_threshold: BOX_SESSION_THRESHOLD,
            // Box's session response carries the authoritative `part_size`;
            // the runner uses our hint as a planning estimate. 8 MiB
            // matches what most Box sessions return.
            multipart_part_size: 8 * 1024 * 1024,
            multipart_max_parallel: 4,
            supports_resume_download: true,
            supports_resume_upload: true,
            supports_server_checksum: true,
            preferred_checksum_algo: Some("sha1".to_string()),
            ..Default::default()
        }
    }

    // Shaped-graph multipart trait wiring (S2-T04).
    //
    // Box's chunked-upload-v2 is the most complex of the four S2 cloud
    // backends because:
    //   - The server returns the authoritative `part_size` in the session
    //     response. Our advertised hint is only a planning estimate; the
    //     runner uses it via `multipart_part_size` to size the graph, but
    //     `upload_part` MUST respect the server value for `Content-Range`
    //     math. We round-trip the server `part_size` through the handle.
    //   - Every chunk PUT requires a `Digest: sha=<base64-sha1-chunk>`
    //     header; the final `commit` POST requires the whole-file SHA-1
    //     in the same header plus a JSON body listing each part's
    //     `{part_id, offset, size, sha1}` receipt.
    //   - Box accepts parallel chunk PUTs as long as the byte ranges do
    //     not overlap, so `multipart_max_parallel = 4` is honest.
    //
    // Handle encoding stores `(session_id, upload_part_url, commit_url,
    // total, part, local_path)` so `complete_multipart_upload` can stream
    // the file from disk once to compute the whole-file SHA-1. Per-part
    // receipts are pushed through `UploadedPart.etag` as JSON, then
    // decoded and emitted in part-number order at commit time.

    async fn begin_multipart_upload(
        &mut self,
        remote_path: &str,
        total_size: u64,
        _content_type: Option<&str>,
        local_source_path: Option<&str>,
    ) -> Result<MultipartHandle, ProviderError> {
        if total_size == 0 {
            return Err(ProviderError::Other(
                "Box multipart upload requires non-zero total_size".to_string(),
            ));
        }
        // Box's commit body requires the whole-file SHA-1 in a `Digest`
        // header. We stream the file once at commit time, so the runner
        // MUST pin the local source path here.
        let local_source = local_source_path.ok_or_else(|| {
            ProviderError::Other(
                "Box multipart upload requires the local source path (runner did not supply one)"
                    .to_string(),
            )
        })?;
        let mut meta = self
            .open_chunked_upload_session(remote_path, total_size)
            .await?;
        meta.local_path = local_source.to_string();
        // The runner uses the advertised `multipart_part_size` (8 MiB) as
        // the per-part length; the Box session returns its own server-
        // decided `part_size`. They MUST match or upload_part calls will
        // overlap byte ranges. For files < 5 GiB Box returns 8 MiB so
        // this almost always matches; surface a typed error otherwise so
        // the runner aborts cleanly instead of producing corrupt commits.
        const BOX_ADVERTISED_PART_SIZE: u64 = 8 * 1024 * 1024;
        if meta.part != BOX_ADVERTISED_PART_SIZE {
            return Err(ProviderError::TransferFailed(format!(
                "Box server returned part_size {} but the runner expects {}; \
                 cannot proceed without runner re-shaping (out-of-scope for S2)",
                meta.part, BOX_ADVERTISED_PART_SIZE
            )));
        }
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
        use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
        use sha1::{Digest, Sha1};

        if part_number == 0 {
            return Err(ProviderError::Other(
                "Box upload_part requires 1-based part_number".to_string(),
            ));
        }
        if data.is_empty() {
            return Err(ProviderError::Other(
                "Box upload_part received empty data".to_string(),
            ));
        }
        let meta = BoxMultipartMeta::decode(&handle.upload_id)?;
        let offset = (part_number as u64 - 1) * meta.part;
        let end = offset
            .checked_add(data.len() as u64)
            .ok_or_else(|| ProviderError::Other("Box part offset overflow".to_string()))?;
        if end > meta.total {
            return Err(ProviderError::Other(format!(
                "Box part {} exceeds declared total: offset {} + len {} > total {}",
                part_number,
                offset,
                data.len(),
                meta.total
            )));
        }

        let chunk_sha1 = {
            let mut h = Sha1::new();
            h.update(&data);
            BASE64.encode(h.finalize())
        };
        let content_range = format!("bytes {}-{}/{}", offset, end - 1, meta.total);

        let token = self.get_token().await?;
        let resp = self
            .client
            .put(&meta.upload_part_url)
            .header(AUTHORIZATION, Self::bearer_header(&token)?)
            .header(CONTENT_TYPE, "application/octet-stream")
            .header("Content-Range", &content_range)
            .header("Digest", format!("sha={}", chunk_sha1))
            .body(data)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let retry_header = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .map(String::from);
            let text = resp.text().await.unwrap_or_default();
            let mut msg = format!(
                "Box upload_part {} failed (HTTP {}): {}",
                part_number,
                status,
                sanitize_api_error(&text)
            );
            if let Some(tail) = box_retry_marker_tail(status.as_u16(), retry_header.as_deref()) {
                msg.push_str(&tail);
            }
            return Err(ProviderError::TransferFailed(msg));
        }

        let part_resp: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;
        let part_field = part_resp.get("part").ok_or_else(|| {
            ProviderError::ParseError("Box upload_part response missing `part` field".to_string())
        })?;
        let part_id = part_field
            .get("part_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ProviderError::ParseError("Box upload_part response missing part.part_id".into())
            })?
            .to_string();
        let receipt = BoxPartReceipt {
            part_id,
            offset,
            size: end - offset,
            sha1: chunk_sha1,
        };
        Ok(UploadedPart {
            part_number,
            etag: serde_json::to_string(&receipt).expect("BoxPartReceipt always serialises"),
        })
    }

    async fn complete_multipart_upload(
        &mut self,
        handle: MultipartHandle,
        parts: Vec<UploadedPart>,
    ) -> Result<(), ProviderError> {
        use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
        use sha1::{Digest, Sha1};
        use tokio::io::AsyncReadExt;

        let meta = BoxMultipartMeta::decode(&handle.upload_id)?;
        if meta.local_path.is_empty() {
            return Err(ProviderError::TransferFailed(
                "Box complete: local_path missing from multipart handle (was begin called via the trait?)"
                    .to_string(),
            ));
        }
        let expected = meta.total.div_ceil(meta.part).max(1) as usize;
        if parts.len() != expected {
            return Err(ProviderError::TransferFailed(format!(
                "Box complete: expected {} parts, runner committed {}",
                expected,
                parts.len()
            )));
        }

        // Sort by part_number (the runner already does this but the trait
        // spec demands sorted parts, so defensive) and rebuild the Box
        // commit body shape `{parts: [{part_id, offset, size, sha1}, ...]}`.
        let mut sorted = parts;
        sorted.sort_by_key(|p| p.part_number);
        let mut commit_parts: Vec<serde_json::Value> = Vec::with_capacity(sorted.len());
        for part in &sorted {
            let receipt: BoxPartReceipt = serde_json::from_str(&part.etag).map_err(|e| {
                ProviderError::ParseError(format!(
                    "Box commit: cannot decode part {} receipt: {}",
                    part.part_number, e
                ))
            })?;
            commit_parts.push(serde_json::json!({
                "part_id": receipt.part_id,
                "offset": receipt.offset,
                "size": receipt.size,
                "sha1": receipt.sha1,
            }));
        }

        // Whole-file SHA-1: stream the local file once. The handle pinned
        // `local_path` at begin time; the file should not have changed
        // (the runner holds the transfer-core for the whole DAG).
        let mut file = tokio::fs::File::open(&meta.local_path)
            .await
            .map_err(ProviderError::IoError)?;
        let mut hasher = Sha1::new();
        let mut buf = vec![0u8; 1024 * 1024];
        loop {
            let n = file.read(&mut buf).await.map_err(ProviderError::IoError)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        let file_sha1 = BASE64.encode(hasher.finalize());

        let commit_body = serde_json::json!({ "parts": commit_parts });
        let token = self.get_token().await?;
        let resp = self
            .client
            .post(&meta.commit_url)
            .header(AUTHORIZATION, Self::bearer_header(&token)?)
            .header(CONTENT_TYPE, "application/json")
            .header("Digest", format!("sha={}", file_sha1))
            .json(&commit_body)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        let status = resp.status();
        // Box returns 201 Created or 202 Accepted (async commit) on success.
        if !status.is_success() && status.as_u16() != 201 && status.as_u16() != 202 {
            let retry_header = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .map(String::from);
            let text = resp.text().await.unwrap_or_default();
            let mut msg = format!(
                "Box commit failed (HTTP {}): {}",
                status,
                sanitize_api_error(&text)
            );
            if let Some(tail) = box_retry_marker_tail(status.as_u16(), retry_header.as_deref()) {
                msg.push_str(&tail);
            }
            return Err(ProviderError::TransferFailed(msg));
        }
        Ok(())
    }

    async fn abort_multipart_upload(
        &mut self,
        handle: MultipartHandle,
    ) -> Result<(), ProviderError> {
        let meta = BoxMultipartMeta::decode(&handle.upload_id)?;
        // Box's abort endpoint is the session URL with DELETE. Best-effort:
        // ignore errors so the abort cannot mask the original transfer
        // error. Sessions expire server-side after a few days if abort
        // ever silently fails.
        if let Ok(token) = self.get_token().await {
            if let Ok(header) = Self::bearer_header(&token) {
                let abort_url =
                    format!("{}/files/upload_sessions/{}", UPLOAD_BASE, meta.session_id);
                let _ = self
                    .client
                    .delete(&abort_url)
                    .header(AUTHORIZATION, header)
                    .send()
                    .await;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::{ExposeSecret, SecretString};

    #[test]
    fn normalize_path_ensures_leading_slash_and_trims_trailing() {
        assert_eq!(BoxProvider::normalize_path(""), "/");
        assert_eq!(BoxProvider::normalize_path("/"), "/");
        assert_eq!(BoxProvider::normalize_path("foo"), "/foo");
        assert_eq!(BoxProvider::normalize_path("/foo"), "/foo");
        assert_eq!(BoxProvider::normalize_path("/foo/"), "/foo");
        assert_eq!(BoxProvider::normalize_path("/a/b/c/"), "/a/b/c");
    }

    #[test]
    fn normalize_path_preserves_single_root_slash() {
        assert_eq!(BoxProvider::normalize_path("/"), "/");
        // pure root never gets trimmed into empty
        let r = BoxProvider::normalize_path("/");
        assert_eq!(r, "/");
    }

    #[test]
    fn bearer_header_builds_valid_authorization_header() {
        let token = SecretString::from("abc.def.ghi".to_string());
        let header = BoxProvider::bearer_header(&token).unwrap();
        assert_eq!(header.to_str().unwrap(), "Bearer abc.def.ghi");
        // input token still accessible after use (secrecy doesn't consume it)
        assert_eq!(token.expose_secret(), "abc.def.ghi");
    }

    #[test]
    fn bearer_header_rejects_control_chars_in_token() {
        let bad_token = SecretString::from("bad\ntoken".to_string());
        assert!(BoxProvider::bearer_header(&bad_token).is_err());
    }

    // T-DEBT-05 S1-T02e: Box rate-limit detection + marker emission.

    #[test]
    fn box_is_rate_limited_recognises_429_and_503() {
        assert!(box_is_rate_limited(429));
        assert!(box_is_rate_limited(503));
        assert!(!box_is_rate_limited(500));
        assert!(!box_is_rate_limited(404));
        assert!(!box_is_rate_limited(200));
    }

    #[test]
    fn box_retry_marker_tail_emits_marker_on_429_with_header() {
        let tail = box_retry_marker_tail(429, Some("30")).expect("rate-limited + valid hint");
        assert!(tail.contains("retry-after-secs=30"));
    }

    #[test]
    fn box_retry_marker_tail_emits_marker_on_503_with_header() {
        let tail = box_retry_marker_tail(503, Some("120")).expect("503 maintenance");
        assert!(tail.contains("retry-after-secs=120"));
    }

    #[test]
    fn box_retry_marker_tail_returns_none_when_not_rate_limited() {
        assert_eq!(box_retry_marker_tail(500, Some("30")), None);
        assert_eq!(box_retry_marker_tail(404, Some("30")), None);
    }

    #[test]
    fn box_retry_marker_tail_returns_none_without_header() {
        assert_eq!(box_retry_marker_tail(429, None), None);
        assert_eq!(box_retry_marker_tail(429, Some("")), None);
        assert_eq!(box_retry_marker_tail(429, Some("abc")), None);
    }

    // ---------------------------------------------------------------------
    // S2-T04 multipart trait wiring
    // ---------------------------------------------------------------------

    #[test]
    fn box_meta_roundtrip_preserves_session_urls_and_local_path() {
        let meta = BoxMultipartMeta {
            session_id: "F971964745A5CD0C001BB7CE".to_string(),
            upload_part_url:
                "https://upload.box.com/api/2.0/files/upload_sessions/F971964745A5CD0C001BB7CE"
                    .to_string(),
            commit_url:
                "https://upload.box.com/api/2.0/files/upload_sessions/F971964745A5CD0C001BB7CE/commit"
                    .to_string(),
            total: 104_857_600,
            part: 8_388_608,
            local_path: "/tmp/box-test.bin".to_string(),
        };
        let encoded = meta.encode();
        let decoded = BoxMultipartMeta::decode(&encoded).expect("decode");
        assert_eq!(decoded, meta);
    }

    #[test]
    fn box_meta_decode_rejects_garbage() {
        assert!(matches!(
            BoxMultipartMeta::decode("not json").unwrap_err(),
            ProviderError::Other(_)
        ));
    }

    #[test]
    fn box_part_receipt_roundtrip_into_uploaded_part_etag() {
        let receipt = BoxPartReceipt {
            part_id: "8F0966B1".to_string(),
            offset: 16_777_216,
            size: 8_388_608,
            sha1: "abcdef==".to_string(),
        };
        let encoded = serde_json::to_string(&receipt).expect("serialise");
        let decoded: BoxPartReceipt = serde_json::from_str(&encoded).expect("deserialise");
        assert_eq!(decoded, receipt);
    }

    #[test]
    fn box_offset_math_walks_part_sequence_to_total_minus_one() {
        // Box's session-decided `part_size` is whatever the server returns
        // (typically 8 MiB for files in the 20 MiB–5 GiB range). Sanity:
        // every part_number from 1..=ceil(total/part) produces a valid
        // offset inside [0, total) and the last byte equals total-1.
        let total: u64 = 100 * 1024 * 1024;
        let part: u64 = 8 * 1024 * 1024;
        let parts = total.div_ceil(part);
        let mut last_end: i128 = -1;
        for pn in 1..=parts {
            let offset = (pn - 1) * part;
            assert!(offset < total);
            let len = part.min(total - offset);
            let end = offset + len - 1;
            assert!(offset as i128 > last_end);
            last_end = end as i128;
        }
        assert_eq!(last_end as u64, total - 1);
    }
}
