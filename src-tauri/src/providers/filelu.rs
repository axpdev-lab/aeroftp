//! FileLu Cloud Storage Provider
//!
//! Implements StorageProvider for FileLu using the REST API.
//! Authentication: API key passed as query parameter `key=`.
//! No OAuth flow required — user generates API key from account settings.
//!
//! API Base: https://filelu.com/api
//! Folders: identified by `fld_id` (u64), root = 0
//! Files: identified by `file_code` (String)
//! Upload: 2-step — get upload server URL, then multipart POST
//! Download: get direct link, then stream

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

use async_trait::async_trait;
use reqwest::multipart;
use secrecy::ExposeSecret;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;
use tracing::info;

use super::{
    send_with_retry, FileLuConfig, HttpRetryConfig, ProviderError, ProviderType, RemoteEntry,
    ShareLinkOptions, ShareLinkResult, StorageInfo, StorageProvider,
};

const API_BASE: &str = "https://filelu.com/api";
const API_V2_BASE: &str = "https://filelu.com/apiv2";
/// Maximum number of cached path entries to prevent unbounded memory growth.
const PATH_CACHE_MAX: usize = 10_000;
/// Maximum pages to retrieve per listing (100 items/page → 10 000 items max)
const MAX_LIST_PAGES: u32 = 100;

fn filelu_log(msg: &str) {
    info!("[FILELU] {}", msg);
}

// ─── API Response Types ──────────────────────────────────────────────────

/// Generic API response wrapper used by FileLu for all endpoints
#[derive(Debug, Deserialize)]
struct ApiResponse<T> {
    status: Option<u32>,
    msg: Option<String>,
    result: Option<T>,
}

/// Account information returned by /account/info
#[derive(Debug, Deserialize)]
struct AccountInfo {
    email: Option<String>,
    #[serde(default, deserialize_with = "deserialize_opt_u64")]
    storage_used: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_opt_u64")]
    storage_left: Option<u64>,
}

/// A folder entry returned by /folder/list
#[derive(Debug, Deserialize)]
struct FolderEntry {
    fld_id: u64,
    name: Option<String>,
    /// Folder token used for password-protecting the folder
    code: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_opt_boolish",
        alias = "has_password",
        alias = "is_password",
        alias = "password_protected",
        alias = "fld_password_protected"
    )]
    password_protected: Option<bool>,
}

/// A deleted file entry returned by /files/deleted
#[derive(Debug, Deserialize, serde::Serialize, Clone)]
pub struct DeletedFileEntry {
    pub file_code: Option<String>,
    pub name: Option<String>,
    pub deleted: Option<String>,
    pub deleted_ago_sec: Option<u64>,
}

/// A file entry returned by /folder/list
/// FileLu API fields: name, size (int or string), uploaded (date string)
#[derive(Debug, Deserialize)]
struct FileEntry {
    file_code: Option<String>,
    name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_size")]
    size: u64,
    uploaded: Option<String>,
    /// Content hash returned by FileLu API — used for sync comparison
    /// since FileLu does not preserve original file mtime on upload.
    hash: Option<String>,
    /// Folder ID this file belongs to (used to filter file/list which may return cross-folder results)
    /// FileLu API returns this as string "0" or number 0 — use flexible deserializer
    #[serde(default, deserialize_with = "deserialize_opt_u64")]
    fld_id: Option<u64>,
    direct_link: Option<String>,
    link: Option<String>,
    #[serde(default, deserialize_with = "deserialize_opt_boolish")]
    only_me: Option<bool>,
    #[serde(
        rename = "public",
        default,
        deserialize_with = "deserialize_opt_boolish"
    )]
    is_public: Option<bool>,
    #[serde(
        default,
        deserialize_with = "deserialize_opt_boolish",
        alias = "has_password",
        alias = "is_password",
        alias = "password_protected",
        alias = "file_password_protected"
    )]
    password_protected: Option<bool>,
}

/// File listing response from file/list endpoint (includes size, hash)
#[derive(Debug, Deserialize)]
struct FileListResult {
    #[serde(default)]
    files: Vec<FileEntry>,
}

/// Folder listing response from folder/list (subfolders only)
#[derive(Debug, Deserialize)]
struct FolderListResult {
    #[serde(default)]
    files: Vec<FileEntry>,
    #[serde(default)]
    folders: Vec<FolderEntry>,
}

/// Top-level upload server response from /upload/server
/// NOTE: sess_id is at response root, result is a plain URL string.
#[derive(Debug, Deserialize)]
struct UploadServerResponse {
    status: Option<u32>,
    msg: Option<String>,
    sess_id: Option<String>,
    result: Option<String>, // the upload URL
}

#[derive(Debug, Deserialize)]
struct StatusOnlyResponse {
    status: Option<u32>,
    msg: Option<String>,
}

/// Upload response entry returned by upload CGI endpoint
#[derive(Debug, Deserialize)]
struct UploadResultEntry {
    file_code: Option<String>,
    file_status: Option<String>,
}

/// Direct download link response from /file/direct_link
#[derive(Debug, Deserialize)]
struct DirectLinkResult {
    url: Option<String>,
}

/// Folder create response
#[derive(Debug, Deserialize)]
struct FolderCreateResult {
    fld_id: Option<u64>,
}

/// Deserializer for `size` which FileLu sometimes returns as string, sometimes as integer.
fn deserialize_size<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    use serde::de::Unexpected;
    let v = serde_json::Value::deserialize(d)?;
    match v {
        serde_json::Value::Number(n) => Ok(n.as_u64().unwrap_or(0)),
        serde_json::Value::String(s) => s
            .parse::<u64>()
            .map_err(|_| serde::de::Error::invalid_value(Unexpected::Str(&s), &"a numeric string")),
        _ => Ok(0),
    }
}

/// Deserializer for optional numeric fields that may arrive as number, string or null.
fn deserialize_opt_u64<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    match v {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::Number(n) => {
            if let Some(value) = n.as_u64() {
                Ok(Some(value))
            } else if let Some(value) = n.as_f64() {
                if value.is_finite() && value >= 0.0 {
                    Ok(Some(value.trunc() as u64))
                } else {
                    Ok(None)
                }
            } else {
                Ok(None)
            }
        }
        serde_json::Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else if let Ok(value) = trimmed.parse::<u64>() {
                Ok(Some(value))
            } else if let Ok(value) = trimmed.parse::<f64>() {
                if value.is_finite() && value >= 0.0 {
                    Ok(Some(value.trunc() as u64))
                } else {
                    Ok(None)
                }
            } else {
                Ok(None)
            }
        }
        _ => Ok(None),
    }
}

/// Deserializer for optional bool fields encoded as bool/number/string.
fn deserialize_opt_boolish<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<bool>, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    match v {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::Bool(b) => Ok(Some(b)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(Some(i != 0))
            } else if let Some(f) = n.as_f64() {
                Ok(Some(f != 0.0))
            } else {
                Ok(None)
            }
        }
        serde_json::Value::String(s) => {
            let normalized = s.trim().to_ascii_lowercase();
            match normalized.as_str() {
                "1" | "true" | "yes" | "on" | "private" | "only_me" => Ok(Some(true)),
                "0" | "false" | "no" | "off" | "public" | "sharing" => Ok(Some(false)),
                _ => Ok(None),
            }
        }
        _ => Ok(None),
    }
}

// ─── Path Cache ──────────────────────────────────────────────────────────

/// One entry in the path cache (file or directory)
#[derive(Debug, Clone)]
struct CacheEntry {
    is_dir: bool,
    fld_id: u64,               // Valid when is_dir = true
    fld_token: Option<String>, // folder token for password protection
    file_code: String,         // Valid when is_dir = false
    size: u64,
    modified: Option<String>,
}

// ─── Provider ────────────────────────────────────────────────────────────

pub struct FileLuProvider {
    config: FileLuConfig,
    client: reqwest::Client,
    connected: bool,
    /// Current working directory (virtual path, e.g. "/Work/Projects")
    current_path: String,
    /// fld_id corresponding to current_path (0 = root)
    current_fld_id: u64,
    /// Cache: virtual path → entry metadata
    path_cache: HashMap<String, CacheEntry>,
}

impl FileLuProvider {
    pub fn new(config: FileLuConfig) -> Self {
        let client = reqwest::Client::builder()
            .user_agent(crate::providers::AEROFTP_USER_AGENT)
            .connect_timeout(std::time::Duration::from_secs(30))
            .read_timeout(std::time::Duration::from_secs(300))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            config,
            client,
            connected: false,
            current_path: "/".to_string(),
            current_fld_id: 0,
            path_cache: HashMap::new(),
        }
    }

    // ─── Helpers ─────────────────────────────────────────────────────────

    fn api_key(&self) -> &str {
        self.config.api_key.expose_secret()
    }

    fn api_url(&self, endpoint: &str) -> String {
        format!("{}/{}?key={}", API_BASE, endpoint, self.api_key())
    }

    /// Build URL for v2 path-based API endpoints
    fn api_v2_url(&self, endpoint: &str, params: &[(&str, &str)]) -> String {
        let mut url = format!("{}/{}?key={}", API_V2_BASE, endpoint, self.api_key());
        for (k, v) in params {
            url.push('&');
            url.push_str(k);
            url.push('=');
            for ch in v.chars() {
                match ch {
                    'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' | '/' => url.push(ch),
                    ' ' => url.push('+'),
                    _ => {
                        for byte in ch.to_string().as_bytes() {
                            url.push_str(&format!("%{:02X}", byte));
                        }
                    }
                }
            }
        }
        url
    }

    fn api_url_with(&self, endpoint: &str, params: &[(&str, &str)]) -> String {
        let mut url = format!("{}/{}?key={}", API_BASE, endpoint, self.api_key());
        for (k, v) in params {
            url.push('&');
            url.push_str(k);
            url.push('=');
            // URL-encode the value
            for ch in v.chars() {
                match ch {
                    'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => url.push(ch),
                    ' ' => url.push('+'),
                    c => {
                        for byte in c.to_string().as_bytes() {
                            url.push('%');
                            url.push_str(&format!("{:02X}", byte));
                        }
                    }
                }
            }
        }
        url
    }

    fn cache_insert(&mut self, path: String, entry: CacheEntry) {
        if self.path_cache.len() >= PATH_CACHE_MAX {
            filelu_log(&format!(
                "path_cache reached {} entries, evicting all",
                self.path_cache.len()
            ));
            self.path_cache.clear();
        }
        self.path_cache.insert(path, entry);
    }

    fn normalize_path(path: &str) -> String {
        let normalized = path.trim().replace('\\', "/");
        if normalized.is_empty() {
            return "/".to_string();
        }

        let mut segments: Vec<&str> = Vec::new();
        for segment in normalized.split('/') {
            match segment {
                "" | "." => {}
                ".." => {
                    let _ = segments.pop();
                }
                _ => segments.push(segment),
            }
        }

        if segments.is_empty() {
            "/".to_string()
        } else {
            format!("/{}", segments.join("/"))
        }
    }

    fn resolve_path(&self, path: &str) -> String {
        if path.starts_with('/') {
            Self::normalize_path(path)
        } else {
            let base = self.current_path.trim_end_matches('/').to_string();
            Self::normalize_path(&format!("{}/{}", base, path))
        }
    }

    async fn get_with_retry(&self, url: &str) -> Result<reqwest::Response, ProviderError> {
        let request = self
            .client
            .get(url)
            .build()
            .map_err(|e| ProviderError::ConnectionFailed(format!("Build GET failed: {}", e)))?;
        send_with_retry(&self.client, request, &HttpRetryConfig::default())
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("GET failed: {}", e)))
    }

    #[allow(dead_code)]
    async fn post_form_with_retry(
        &self,
        url: &str,
        body: String,
    ) -> Result<reqwest::Response, ProviderError> {
        let request = self
            .client
            .post(url)
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(body)
            .build()
            .map_err(|e| ProviderError::ConnectionFailed(format!("Build POST failed: {}", e)))?;
        send_with_retry(&self.client, request, &HttpRetryConfig::default())
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("POST failed: {}", e)))
    }

    async fn parse_api<T: for<'de> serde::Deserialize<'de>>(
        resp: reqwest::Response,
    ) -> Result<T, ProviderError> {
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| ProviderError::NetworkError(format!("Failed to read response: {}", e)))?;

        if !status.is_success() {
            return Err(ProviderError::ServerError(format!(
                "HTTP {}: {}",
                status, text
            )));
        }

        let api_resp: ApiResponse<T> = serde_json::from_str(&text).map_err(|e| {
            ProviderError::ParseError(format!(
                "JSON parse error: {}. Body: {}",
                e,
                &text[..text.len().min(200)]
            ))
        })?;

        match api_resp.status {
            Some(s) if s != 200 => {
                let msg = api_resp.msg.unwrap_or_else(|| format!("API error {}", s));
                Err(ProviderError::ServerError(msg))
            }
            _ => api_resp.result.ok_or_else(|| {
                ProviderError::ParseError("API response missing 'result' field".to_string())
            }),
        }
    }

    async fn ensure_api_ok(resp: reqwest::Response) -> Result<(), ProviderError> {
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| ProviderError::NetworkError(format!("Failed to read response: {}", e)))?;

        if !status.is_success() {
            return Err(ProviderError::ServerError(format!(
                "HTTP {}: {}",
                status, text
            )));
        }

        let parsed: StatusOnlyResponse = serde_json::from_str(&text).map_err(|e| {
            ProviderError::ParseError(format!(
                "JSON parse error: {}. Body: {}",
                e,
                &text[..text.len().min(200)]
            ))
        })?;

        if let Some(s) = parsed.status {
            if s != 200 {
                return Err(ProviderError::ServerError(
                    parsed.msg.unwrap_or_else(|| format!("API error {}", s)),
                ));
            }
        }

        Ok(())
    }

    fn build_clone_name(source_name: &str, n: usize) -> String {
        let (base, ext) = match source_name.rsplit_once('.') {
            Some((b, e)) if !b.is_empty() && !e.is_empty() => (b.to_string(), format!(".{}", e)),
            _ => (source_name.to_string(), String::new()),
        };

        if n == 1 {
            format!("{}_copy{}", base, ext)
        } else {
            format!("{}_copy{}{}", base, n, ext)
        }
    }

    /// Preferred listing path: FileLu v2 `folder/list` by `folder_path`.
    /// Support confirmed on 2026-03-28: the response includes both files and folders,
    /// plus file `hash` and `direct_link` metadata.
    async fn list_folder_v2(&self, folder_path: &str) -> Result<FolderListResult, ProviderError> {
        let path = if folder_path.is_empty() || folder_path == "/" {
            "/".to_string()
        } else {
            folder_path.to_string()
        };
        let per_page = "100";
        let mut all_files: Vec<FileEntry> = Vec::new();
        let mut all_folders: Vec<FolderEntry> = Vec::new();

        for page in 1..=MAX_LIST_PAGES {
            let page_str = page.to_string();
            let url = self.api_v2_url(
                "folder/list",
                &[
                    ("folder_path", &path),
                    ("per_page", per_page),
                    ("page", &page_str),
                ],
            );
            let resp = self.get_with_retry(&url).await?;
            let result = Self::parse_api::<FolderListResult>(resp).await?;
            let file_count = result.files.len();
            let folder_count = result.folders.len();
            all_files.extend(result.files);
            all_folders.extend(result.folders);
            if file_count < 100 && folder_count < 100 {
                break;
            }
        }

        Ok(FolderListResult {
            files: all_files,
            folders: all_folders,
        })
    }

    /// Legacy fallback path kept as a defensive compatibility path.
    /// Uses v1 `file/list` by `fld_id` for files and v2 `folder/list` by `folder_path` for folders.
    async fn list_folder_legacy_hybrid(
        &self,
        folder_path: &str,
        fld_id: u64,
    ) -> Result<FolderListResult, ProviderError> {
        let fld_id_str = fld_id.to_string();
        let path = if folder_path.is_empty() || folder_path == "/" {
            "/".to_string()
        } else {
            folder_path.to_string()
        };
        let per_page = "100";

        let files_fut = async {
            let mut all_files: Vec<FileEntry> = Vec::new();
            for page in 1..=MAX_LIST_PAGES {
                let page_str = page.to_string();
                let url = self.api_url_with(
                    "file/list",
                    &[
                        ("fld_id", &fld_id_str),
                        ("per_page", per_page),
                        ("page", &page_str),
                    ],
                );
                let resp = self.get_with_retry(&url).await?;
                let result = Self::parse_api::<FileListResult>(resp).await?;
                let count = result.files.len();
                all_files.extend(
                    result
                        .files
                        .into_iter()
                        .filter(|f| f.fld_id.is_none_or(|id| id == fld_id)),
                );
                if count < 100 {
                    break;
                }
            }
            Ok::<_, ProviderError>(all_files)
        };

        let folders_fut = async {
            let mut all_folders: Vec<FolderEntry> = Vec::new();
            for page in 1..=MAX_LIST_PAGES {
                let page_str = page.to_string();
                let url = self.api_v2_url(
                    "folder/list",
                    &[
                        ("folder_path", &path),
                        ("per_page", per_page),
                        ("page", &page_str),
                    ],
                );
                let resp = self.get_with_retry(&url).await?;
                let result = Self::parse_api::<FolderListResult>(resp).await?;
                let count = result.folders.len();
                all_folders.extend(result.folders);
                if count < 100 {
                    break;
                }
            }
            Ok::<_, ProviderError>(all_folders)
        };

        let (files_result, folders_result) = tokio::join!(files_fut, folders_fut);

        Ok(FolderListResult {
            files: files_result?,
            folders: folders_result?,
        })
    }

    /// Legacy v1 list — kept for operations that still need fld_id (upload, mkdir)
    #[allow(dead_code)]
    async fn list_folder_by_id(&self, fld_id: u64) -> Result<FolderListResult, ProviderError> {
        let fld_id_str = fld_id.to_string();
        let per_page = "100";
        let mut all_files: Vec<FileEntry> = Vec::new();
        let mut all_folders: Vec<FolderEntry> = Vec::new();

        for page in 1..=MAX_LIST_PAGES {
            let page_str = page.to_string();
            let url = self.api_url_with(
                "file/list",
                &[
                    ("fld_id", &fld_id_str),
                    ("per_page", per_page),
                    ("page", &page_str),
                ],
            );
            let resp = self.get_with_retry(&url).await?;
            let result = Self::parse_api::<FileListResult>(resp).await?;
            let count = result.files.len();
            all_files.extend(
                result
                    .files
                    .into_iter()
                    .filter(|f| f.fld_id.is_none_or(|id| id == fld_id)),
            );
            if count < 100 {
                break;
            }
        }

        for page in 1..=MAX_LIST_PAGES {
            let page_str = page.to_string();
            let url = self.api_url_with(
                "folder/list",
                &[
                    ("fld_id", &fld_id_str),
                    ("per_page", per_page),
                    ("page", &page_str),
                ],
            );
            let resp = self.get_with_retry(&url).await?;
            let result = Self::parse_api::<FolderListResult>(resp).await?;
            let count = result.folders.len();
            all_folders.extend(result.folders);
            if count < 100 {
                break;
            }
        }

        Ok(FolderListResult {
            files: all_files,
            folders: all_folders,
        })
    }

    async fn populate_cache_for(
        &mut self,
        parent_path: &str,
        parent_fld_id: u64,
    ) -> Result<Vec<RemoteEntry>, ProviderError> {
        let result = match self.list_folder_v2(parent_path).await {
            Ok(result) => result,
            Err(error) => {
                filelu_log(&format!(
                    "v2 folder/list failed for '{}', falling back to legacy hybrid listing: {}",
                    parent_path, error
                ));
                self.list_folder_legacy_hybrid(parent_path, parent_fld_id)
                    .await?
            }
        };
        let mut entries: Vec<RemoteEntry> = Vec::new();

        let parent_norm = Self::normalize_path(parent_path);
        let prefix = if parent_norm == "/" {
            String::new()
        } else {
            parent_norm.clone()
        };

        for folder in &result.folders {
            let name = folder.name.clone().unwrap_or_else(|| "unnamed".to_string());
            let child_norm = Self::normalize_path(&format!("{}/{}", prefix, name));

            self.cache_insert(
                child_norm.clone(),
                CacheEntry {
                    is_dir: true,
                    fld_id: folder.fld_id,
                    fld_token: folder.code.clone(),
                    file_code: String::new(),
                    size: 0,
                    modified: None,
                },
            );

            let mut metadata = HashMap::new();
            if let Some(is_password_protected) = folder.password_protected {
                metadata.insert(
                    "filelu_password_protected".to_string(),
                    is_password_protected.to_string(),
                );
            }

            entries.push(RemoteEntry {
                name,
                path: child_norm,
                is_dir: true,
                size: 0,
                modified: None,
                permissions: None,
                owner: None,
                group: None,
                is_symlink: false,
                link_target: None,
                mime_type: None,
                metadata,
            });
        }

        for file in &result.files {
            let name = file.name.clone().unwrap_or_else(|| "unnamed".to_string());
            let code = file.file_code.clone().unwrap_or_default();
            let child_norm = Self::normalize_path(&format!("{}/{}", prefix, name));

            self.cache_insert(
                child_norm.clone(),
                CacheEntry {
                    is_dir: false,
                    fld_id: parent_fld_id,
                    fld_token: None,
                    file_code: code,
                    size: file.size,
                    modified: file.uploaded.clone(),
                },
            );

            let mime = Path::new(&name)
                .extension()
                .and_then(|e| e.to_str())
                .map(|ext| mime_from_ext(ext).to_string());

            let mut metadata = HashMap::new();
            if let Some(is_password_protected) = file.password_protected {
                metadata.insert(
                    "filelu_password_protected".to_string(),
                    is_password_protected.to_string(),
                );
            }
            if let Some(ref h) = file.hash {
                metadata.insert("content_hash".to_string(), h.clone());
            }
            if let Some(ref direct_link) = file.direct_link {
                metadata.insert("filelu_direct_link".to_string(), direct_link.clone());
            }
            if let Some(ref public_link) = file.link {
                metadata.insert("filelu_public_link".to_string(), public_link.clone());
            }
            if let Some(is_public) = file.is_public {
                metadata.insert("filelu_public".to_string(), is_public.to_string());
            }

            entries.push(RemoteEntry {
                name,
                path: child_norm,
                is_dir: false,
                size: file.size,
                modified: file.uploaded.clone(),
                permissions: file.only_me.map(|is_private| {
                    if is_private {
                        "private".to_string()
                    } else {
                        "public".to_string()
                    }
                }),
                owner: None,
                group: None,
                is_symlink: false,
                link_target: None,
                mime_type: mime,
                metadata,
            });
        }

        Ok(entries)
    }

    async fn resolve_path_entry(&mut self, path: &str) -> Result<CacheEntry, ProviderError> {
        let norm = Self::normalize_path(path);

        if norm == "/" {
            return Ok(CacheEntry {
                is_dir: true,
                fld_id: 0,
                fld_token: None,
                file_code: String::new(),
                size: 0,
                modified: None,
            });
        }

        if let Some(entry) = self.path_cache.get(&norm) {
            return Ok(entry.clone());
        }

        // Walk segment by segment from root
        let segments: Vec<&str> = norm.trim_start_matches('/').split('/').collect();
        let mut current_fld_id: u64 = 0;
        let mut current_virtual = String::new();

        for (i, seg) in segments.iter().enumerate() {
            let parent_virtual = if current_virtual.is_empty() {
                "/".to_string()
            } else {
                current_virtual.clone()
            };

            self.populate_cache_for(&parent_virtual, current_fld_id)
                .await?;

            let child_virtual = Self::normalize_path(&format!("{}/{}", parent_virtual, seg));
            let entry = self
                .path_cache
                .get(&child_virtual)
                .cloned()
                .ok_or_else(|| {
                    ProviderError::NotFound(format!("Path not found: {}", child_virtual))
                })?;

            if entry.is_dir {
                current_fld_id = entry.fld_id;
            } else if i < segments.len() - 1 {
                return Err(ProviderError::InvalidPath(format!(
                    "Not a directory: {}",
                    child_virtual
                )));
            } else {
                return Ok(entry);
            }

            current_virtual = child_virtual;
        }

        self.path_cache
            .get(&norm)
            .cloned()
            .ok_or_else(|| ProviderError::NotFound(format!("Directory not found: {}", norm)))
    }

    async fn resolve_fld_id(&mut self, path: &str) -> Result<u64, ProviderError> {
        let entry = self.resolve_path_entry(path).await?;
        if entry.is_dir {
            Ok(entry.fld_id)
        } else {
            Err(ProviderError::InvalidPath(format!(
                "Not a directory: {}",
                path
            )))
        }
    }

    async fn resolve_file_code(&mut self, path: &str) -> Result<String, ProviderError> {
        let entry = self.resolve_path_entry(path).await?;
        if entry.is_dir {
            Err(ProviderError::InvalidPath(format!(
                "Path is a directory: {}",
                path
            )))
        } else {
            Ok(entry.file_code)
        }
    }

    fn invalidate_cache_under(&mut self, parent: &str) {
        let prefix = Self::normalize_path(parent);
        let prefix_slash = if prefix == "/" {
            "/".to_string()
        } else {
            format!("{}/", prefix)
        };
        self.path_cache
            .retain(|k, _| !k.starts_with(&prefix_slash) && k != &prefix);
    }

    /// Get direct download URL — v2 path-based (file_path), fallback to v1 (file_code)
    async fn get_direct_url_v2(&self, file_path: &str) -> Result<String, ProviderError> {
        let url = self.api_v2_url("file/direct_link", &[("file_path", file_path)]);
        let resp = self.get_with_retry(&url).await?;
        let result = Self::parse_api::<DirectLinkResult>(resp).await?;
        result
            .url
            .ok_or_else(|| ProviderError::TransferFailed("No download URL returned".to_string()))
    }

    /// Legacy v1 get direct URL by file_code — kept for trash operations
    #[allow(dead_code)]
    async fn get_direct_url(&mut self, file_code: &str) -> Result<String, ProviderError> {
        let body = format!("file_code={}&key={}", file_code, self.api_key());
        let url = format!("{}/file/direct_link", API_BASE);
        let resp = self.post_form_with_retry(&url, body).await?;
        let result = Self::parse_api::<DirectLinkResult>(resp).await?;
        result
            .url
            .ok_or_else(|| ProviderError::TransferFailed("No download URL returned".to_string()))
    }

    // ─── FileLu-Specific Public Methods ──────────────────────────────────

    /// Set or unset a file password. Pass empty string to remove password.
    pub async fn set_file_password(
        &mut self,
        path: &str,
        password: &str,
    ) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let norm = self.resolve_path(path);
        let file_code = self.resolve_file_code(&norm).await?;
        let url = self.api_url_with(
            "file/set_password",
            &[("file_code", &file_code), ("file_password", password)],
        );
        self.get_with_retry(&url).await?;
        Ok(())
    }

    /// Toggle file visibility. `only_me=true` → private, `false` → public.
    pub async fn set_file_privacy(
        &mut self,
        path: &str,
        only_me: bool,
    ) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let norm = self.resolve_path(path);
        let file_code = self.resolve_file_code(&norm).await?;
        let flag = if only_me { "1" } else { "0" };
        let url = self.api_url_with(
            "file/only_me",
            &[("file_code", &file_code), ("only_me", flag)],
        );
        self.get_with_retry(&url).await?;
        Ok(())
    }

    /// Clone (server-side copy) a file. Returns the URL of the cloned file.
    pub async fn clone_file(&mut self, path: &str) -> Result<String, ProviderError> {
        if !self.connected {
            self.connect().await?;
        }
        let norm = self.resolve_path(path);
        let file_code = self.resolve_file_code(&norm).await?;

        let source_name = norm.rsplit('/').next().unwrap_or("file").to_string();
        let source_parent = match norm.rfind('/') {
            Some(0) => "/".to_string(),
            Some(idx) => norm[..idx].to_string(),
            None => "/".to_string(),
        };
        let source_parent_fld_id = self.resolve_fld_id(&source_parent).await?;

        let url = self.api_url_with("file/clone", &[("file_code", &file_code)]);
        let resp = self.get_with_retry(&url).await?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| ProviderError::NetworkError(format!("clone_file read failed: {}", e)))?;
        if !status.is_success() {
            return Err(ProviderError::ServerError(format!(
                "HTTP {}: {}",
                status, body
            )));
        }

        let value: serde_json::Value = serde_json::from_str(&body).map_err(|e| {
            ProviderError::ParseError(format!(
                "clone_file parse error: {}. Body: {}",
                e,
                &body[..body.len().min(200)]
            ))
        })?;

        let status_code = value.get("status").and_then(|s| s.as_i64()).unwrap_or(200);
        if status_code != 200 {
            let msg = value
                .get("msg")
                .and_then(|m| m.as_str())
                .unwrap_or("clone failed");
            return Err(ProviderError::ServerError(msg.to_string()));
        }

        let result = value.get("result");
        let mut new_code = result
            .and_then(|r| r.get("filecode").or_else(|| r.get("file_code")))
            .and_then(|c| c.as_str())
            .unwrap_or_default()
            .to_string();
        let mut out_url = result
            .and_then(|r| r.get("url"))
            .and_then(|u| u.as_str())
            .unwrap_or_default()
            .to_string();

        // Some API variants may return an array [{file_code: ...}] without wrapped result.
        if new_code.is_empty() {
            if let Some(arr) = value.as_array() {
                if let Some(first) = arr.first() {
                    if let Some(code) = first.get("file_code").and_then(|c| c.as_str()) {
                        new_code = code.to_string();
                    }
                }
            }
        }

        // FileLu clone defaults to root on some accounts: force same source folder.
        if !new_code.is_empty() && source_parent_fld_id != 0 {
            let fld_id_str = source_parent_fld_id.to_string();
            let set_folder_url = self.api_url_with(
                "file/set_folder",
                &[("file_code", &new_code), ("fld_id", &fld_id_str)],
            );
            let set_folder_resp = self.get_with_retry(&set_folder_url).await?;
            Self::ensure_api_ok(set_folder_resp).await?;
            self.invalidate_cache_under(&source_parent);
        }

        // Keep cloned file path-unique in AeroFTP model (FileLu may allow same-name duplicates).
        if !new_code.is_empty() {
            for n in 1..=20 {
                let candidate_name = Self::build_clone_name(&source_name, n);
                let rename_url = self.api_url_with(
                    "file/rename",
                    &[("file_code", &new_code), ("name", &candidate_name)],
                );
                let rename_resp = self.get_with_retry(&rename_url).await?;
                if Self::ensure_api_ok(rename_resp).await.is_ok() {
                    self.invalidate_cache_under(&source_parent);
                    break;
                }
            }
        }

        if out_url.is_empty() && !new_code.is_empty() {
            out_url = format!("https://filelu.com/{}", new_code);
        }
        Ok(out_url)
    }

    /// Set or unset a folder password via its fld_token.
    /// Returns Err if the folder token is not available (folder not yet shared).
    pub async fn set_folder_password(
        &mut self,
        path: &str,
        password: &str,
    ) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let norm = self.resolve_path(path);
        let entry = self.resolve_path_entry(&norm).await?;
        if !entry.is_dir {
            return Err(ProviderError::InvalidPath(
                "Path is not a folder".to_string(),
            ));
        }
        let token = entry.fld_token.ok_or_else(|| {
            ProviderError::ServerError(
                "Folder token unavailable — enable folder sharing first".to_string(),
            )
        })?;
        let url = self.api_url_with(
            "folder/set_password",
            &[("fld_token", &token), ("fld_password", password)],
        );
        self.get_with_retry(&url).await?;
        Ok(())
    }

    /// Configure folder settings: filedrop (others can upload) and public visibility.
    pub async fn set_folder_settings(
        &mut self,
        path: &str,
        filedrop: bool,
        is_public: bool,
    ) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let norm = self.resolve_path(path);
        let fld_id = self.resolve_fld_id(&norm).await?;
        let fd = if filedrop { "1" } else { "0" };
        let pub_ = if is_public { "1" } else { "0" };
        let url = self.api_url_with(
            "folder/setting",
            &[
                ("fld_id", &fld_id.to_string()),
                ("filedrop", fd),
                ("fld_public", pub_),
            ],
        );
        self.get_with_retry(&url).await?;
        Ok(())
    }

    /// List all deleted files (trash).
    pub async fn list_deleted_files(&mut self) -> Result<Vec<DeletedFileEntry>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let url = self.api_url("files/deleted");
        let resp = self.get_with_retry(&url).await?;
        let entries = Self::parse_api::<Vec<DeletedFileEntry>>(resp).await?;
        Ok(entries)
    }

    /// Restore a file from trash by file_code.
    pub async fn restore_deleted_file(&mut self, file_code: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let url = self.api_url_with(
            "file/restore",
            &[("file_code", file_code), ("restore", "1")],
        );
        self.get_with_retry(&url).await?;
        Ok(())
    }

    /// Restore a folder from trash by fld_id.
    pub async fn restore_deleted_folder(&mut self, fld_id: u64) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let url = self.api_url_with("folder/restore", &[("fld_id", &fld_id.to_string())]);
        self.get_with_retry(&url).await?;
        Ok(())
    }

    /// Upload a file from a remote URL into the given destination folder.
    /// Returns the file_code of the newly created file.
    pub async fn remote_url_upload(
        &mut self,
        remote_url: &str,
        dest_path: &str,
    ) -> Result<String, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let norm = self.resolve_path(dest_path);
        let fld_id = self.resolve_fld_id(&norm).await?;
        // URL-encode the remote URL ourselves via api_url_with
        let url = self.api_url_with(
            "upload/url",
            &[("url", remote_url), ("fld_id", &fld_id.to_string())],
        );
        let resp = self.get_with_retry(&url).await?;
        // Response is an array: [{"file_code":"..."}]
        let text = resp.text().await.map_err(|e| {
            ProviderError::NetworkError(format!("remote_url_upload read failed: {}", e))
        })?;
        #[derive(Deserialize)]
        struct RemoteUploadEntry {
            file_code: Option<String>,
        }
        let entries: Vec<RemoteUploadEntry> = serde_json::from_str(&text).map_err(|e| {
            ProviderError::ParseError(format!("remote_url_upload parse error: {}", e))
        })?;
        let code = entries
            .into_iter()
            .next()
            .and_then(|e| e.file_code)
            .unwrap_or_default();
        self.invalidate_cache_under(&norm);
        Ok(code)
    }

    /// Permanently delete a file from trash.
    /// API: file/permanent_delete?key=xxx&file_code=xxx
    pub async fn permanent_delete_file(&mut self, file_code: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let url = self.api_url_with("file/permanent_delete", &[("file_code", file_code)]);
        let resp = self.get_with_retry(&url).await?;
        Self::ensure_api_ok(resp).await
    }
}

// ─── StorageProvider Trait ───────────────────────────────────────────────

#[async_trait]
impl StorageProvider for FileLuProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn provider_type(&self) -> ProviderType {
        ProviderType::FileLu
    }

    fn display_name(&self) -> String {
        "FileLu".to_string()
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        filelu_log("Connecting via API key…");
        let url = self.api_url("account/info");
        let resp = self.get_with_retry(&url).await?;
        let info = Self::parse_api::<AccountInfo>(resp).await?;

        let email = info.email.unwrap_or_else(|| "FileLu user".to_string());
        filelu_log(&format!("Connected as {}", email));
        self.connected = true;

        if let Some(ref init) = self.config.initial_path.clone() {
            if !init.is_empty() && init != "/" {
                if let Ok(fld_id) = self.resolve_fld_id(init).await {
                    self.current_path = Self::normalize_path(init);
                    self.current_fld_id = fld_id;
                }
            }
        }

        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.connected = false;
        self.path_cache.clear();
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let norm = self.resolve_path(path);
        let fld_id = self.resolve_fld_id(&norm).await?;
        self.populate_cache_for(&norm, fld_id).await
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> {
        Ok(self.current_path.clone())
    }

    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let norm = self.resolve_path(path);
        let fld_id = self.resolve_fld_id(&norm).await?;
        self.current_path = norm;
        self.current_fld_id = fld_id;
        Ok(())
    }

    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        if self.current_path == "/" {
            return Ok(());
        }
        let parent = match self.current_path.rfind('/') {
            Some(0) => "/".to_string(),
            Some(idx) => self.current_path[..idx].to_string(),
            None => "/".to_string(),
        };
        let fld_id = self.resolve_fld_id(&parent).await?;
        self.current_path = parent;
        self.current_fld_id = fld_id;
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
        // v2: download by path — no file_code resolution needed
        let norm = self.resolve_path(remote_path);
        let direct_url = self.get_direct_url_v2(&norm).await?;

        let resp = self.get_with_retry(&direct_url).await?;
        let total_size = resp.content_length().unwrap_or(0);
        let mut atomic = super::atomic_write::AtomicFile::new(local_path)
            .await
            .map_err(ProviderError::IoError)?;

        use futures_util::StreamExt;
        let mut downloaded: u64 = 0;
        let mut stream = resp.bytes_stream();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| {
                ProviderError::TransferFailed(format!("Download chunk error: {}", e))
            })?;
            atomic
                .write_all(&chunk)
                .await
                .map_err(ProviderError::IoError)?;
            downloaded += chunk.len() as u64;
            if let Some(ref cb) = on_progress {
                cb(downloaded, total_size);
            }
        }
        atomic.commit().await.map_err(ProviderError::IoError)?;
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
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let norm = self.resolve_path(remote_path);
        let direct_url = self.get_direct_url_v2(&norm).await?;

        // FileLu CDN URLs don't need auth headers
        super::http_resumable_download(
            local_path,
            |range_header| {
                let mut req = self.client.get(&direct_url);
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
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        // v2: download by path — no file_code resolution needed
        let norm = self.resolve_path(remote_path);
        let direct_url = self.get_direct_url_v2(&norm).await?;

        let resp = self.get_with_retry(&direct_url).await?;

        if let Some(cl) = resp.content_length() {
            if cl > super::MAX_DOWNLOAD_TO_BYTES {
                return Err(ProviderError::TransferFailed(format!(
                    "File too large ({:.1} MB) for in-memory download",
                    cl as f64 / 1_048_576.0
                )));
            }
        }

        resp.bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| ProviderError::TransferFailed(format!("Download failed: {}", e)))
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

        let norm = self.resolve_path(remote_path);
        let (dest_dir, filename) = match norm.rfind('/') {
            Some(0) => ("/".to_string(), norm[1..].to_string()),
            Some(idx) => (norm[..idx].to_string(), norm[idx + 1..].to_string()),
            None => ("/".to_string(), norm.clone()),
        };
        if filename.is_empty() {
            return Err(ProviderError::InvalidPath(
                "Upload path must include filename".to_string(),
            ));
        }

        let fld_id = self.resolve_fld_id(&dest_dir).await?;

        // Delete existing file with same name to prevent duplicates
        // (FileLu creates a new file_code on every upload, never overwrites)
        if let Ok(existing) = self.resolve_path_entry(&norm).await {
            if !existing.is_dir && !existing.file_code.is_empty() {
                let del_url = self.api_url_with(
                    "file/remove",
                    &[("file_code", &existing.file_code), ("remove", "1")],
                );
                let _ = self.get_with_retry(&del_url).await;
                self.invalidate_cache_under(&dest_dir);
            }
        }

        // Step 1: Get upload server
        // FileLu response: { status, sess_id, result: "<upload_url>", msg }
        // sess_id is at root (NOT inside result), result is a plain URL string.
        let server_url = self.api_url_with("upload/server", &[("fld_id", &fld_id.to_string())]);
        let resp = self.get_with_retry(&server_url).await?;
        let text = resp.text().await.map_err(|e| {
            ProviderError::NetworkError(format!("Failed to read upload server response: {}", e))
        })?;
        let server_info: UploadServerResponse = serde_json::from_str(&text).map_err(|e| {
            ProviderError::ParseError(format!(
                "Upload server JSON error: {}. Body: {}",
                e,
                &text[..text.len().min(200)]
            ))
        })?;
        if let Some(s) = server_info.status {
            if s != 200 {
                let msg = server_info
                    .msg
                    .unwrap_or_else(|| format!("Upload server error {}", s));
                return Err(ProviderError::ServerError(msg));
            }
        }
        let sess_id = server_info.sess_id.ok_or_else(|| {
            ProviderError::TransferFailed("Upload server returned no session ID".to_string())
        })?;
        let upload_url = server_info.result.ok_or_else(|| {
            ProviderError::TransferFailed("Upload server returned no URL".to_string())
        })?;

        // Step 2: Stream file — avoid reading entire file into memory (OOM on large files)
        let total_size = tokio::fs::metadata(local_path)
            .await
            .map_err(ProviderError::IoError)?
            .len();

        if let Some(ref cb) = on_progress {
            cb(0, total_size);
        }

        let file = tokio::fs::File::open(local_path)
            .await
            .map_err(ProviderError::IoError)?;
        let stream = tokio_util::io::ReaderStream::new(file);
        let body = reqwest::Body::wrap_stream(stream);

        // Step 3: Upload via multipart with streaming body
        let part = multipart::Part::stream_with_length(body, total_size)
            .file_name(filename.clone())
            .mime_str("application/octet-stream")
            .map_err(|e| ProviderError::TransferFailed(format!("Multipart error: {}", e)))?;

        let form = multipart::Form::new()
            .text("sess_id", sess_id)
            .text("utype", "prem")
            .text("fld_id", fld_id.to_string())
            .part("file_0", part);

        let request = self
            .client
            .post(&upload_url)
            .multipart(form)
            .build()
            .map_err(|e| {
                ProviderError::TransferFailed(format!("Build upload request failed: {}", e))
            })?;

        let resp = send_with_retry(&self.client, request, &HttpRetryConfig::default())
            .await
            .map_err(|e| ProviderError::TransferFailed(format!("Upload failed: {}", e)))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::TransferFailed(format!(
                "Upload HTTP error: {}",
                body
            )));
        }

        let upload_body = resp.text().await.map_err(|e| {
            ProviderError::TransferFailed(format!("Failed to read upload response: {}", e))
        })?;
        let upload_results: Vec<UploadResultEntry> =
            serde_json::from_str(&upload_body).map_err(|e| {
                ProviderError::ParseError(format!(
                    "Upload result JSON error: {}. Body: {}",
                    e,
                    &upload_body[..upload_body.len().min(200)]
                ))
            })?;
        let uploaded_file_code = upload_results
            .into_iter()
            .find(|entry| {
                entry
                    .file_status
                    .as_deref()
                    .map(|s| s.eq_ignore_ascii_case("OK"))
                    .unwrap_or(true)
                    && entry
                        .file_code
                        .as_ref()
                        .map(|c| !c.is_empty())
                        .unwrap_or(false)
            })
            .and_then(|entry| entry.file_code)
            .ok_or_else(|| {
                ProviderError::TransferFailed(
                    "Upload completed but file_code was not returned".to_string(),
                )
            })?;

        // FileLu upload endpoint may place files in root depending on account/API behavior.
        // Force destination folder explicitly when target is not root.
        if fld_id != 0 {
            let fld_id_str = fld_id.to_string();
            let set_folder_url = self.api_url_with(
                "file/set_folder",
                &[("file_code", &uploaded_file_code), ("fld_id", &fld_id_str)],
            );
            let set_folder_resp = self.get_with_retry(&set_folder_url).await?;
            Self::ensure_api_ok(set_folder_resp).await?;
        }

        if let Some(ref cb) = on_progress {
            cb(total_size, total_size);
        }

        self.invalidate_cache_under(&dest_dir);
        filelu_log(&format!("Uploaded: {}", filename));
        Ok(())
    }

    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let norm = self.resolve_path(path);

        // Check if folder already exists — FileLu creates duplicates on every mkdir call
        if self.resolve_fld_id(&norm).await.is_ok() {
            return Ok(()); // Already exists, skip creation
        }

        let (parent_path, folder_name) = match norm.rfind('/') {
            Some(0) => ("/".to_string(), norm[1..].to_string()),
            Some(idx) => (norm[..idx].to_string(), norm[idx + 1..].to_string()),
            None => ("/".to_string(), norm.clone()),
        };
        if folder_name.is_empty() {
            return Err(ProviderError::InvalidPath(
                "Folder name cannot be empty".to_string(),
            ));
        }

        let parent_fld_id = self.resolve_fld_id(&parent_path).await?;
        let url = self.api_url_with(
            "folder/create",
            &[
                ("parent_id", &parent_fld_id.to_string()),
                ("name", &folder_name),
            ],
        );
        let resp = self.get_with_retry(&url).await?;
        let result = Self::parse_api::<FolderCreateResult>(resp).await?;
        let new_fld_id = result
            .fld_id
            .ok_or_else(|| ProviderError::ServerError("mkdir: no fld_id returned".to_string()))?;

        self.cache_insert(
            norm,
            CacheEntry {
                is_dir: true,
                fld_id: new_fld_id,
                fld_token: None,
                file_code: String::new(),
                size: 0,
                modified: None,
            },
        );
        self.invalidate_cache_under(&parent_path);
        Ok(())
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let norm = self.resolve_path(path);
        let entry = self.resolve_path_entry(&norm).await?;

        // Delete: move to trash (soft-delete). Permanent delete only from TrashManager.
        if entry.is_dir {
            let url = self.api_url_with("folder/delete", &[("fld_id", &entry.fld_id.to_string())]);
            let resp = self.get_with_retry(&url).await?;
            Self::ensure_api_ok(resp).await?;
        } else {
            // v1 file/remove with remove=1. FileLu API has no soft-delete endpoint —
            // file/remove without remove=1 returns "Invalid option". Permanent delete is
            // the only API-supported delete. Trash is web-UI only on FileLu.
            let url = self.api_url_with(
                "file/remove",
                &[("file_code", &entry.file_code), ("remove", "1")],
            );
            let resp = self.get_with_retry(&url).await?;
            Self::ensure_api_ok(resp).await?;
        }

        let parent = norm
            .rfind('/')
            .map(|i| {
                if i == 0 {
                    "/".to_string()
                } else {
                    norm[..i].to_string()
                }
            })
            .unwrap_or_else(|| "/".to_string());
        self.path_cache.remove(&norm);
        self.invalidate_cache_under(&norm);
        self.invalidate_cache_under(&parent);
        Ok(())
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        self.delete(path).await
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        self.delete(path).await
    }

    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        if !self.connected {
            self.connect().await?;
        }
        let norm_from = self.resolve_path(from);
        let norm_to = self.resolve_path(to);
        let new_name = norm_to.rsplit('/').next().unwrap_or("").to_string();
        if new_name.is_empty() {
            return Err(ProviderError::InvalidPath(
                "New name cannot be empty".to_string(),
            ));
        }

        let from_parent = norm_from
            .rfind('/')
            .map(|i| {
                if i == 0 {
                    "/".to_string()
                } else {
                    norm_from[..i].to_string()
                }
            })
            .unwrap_or_else(|| "/".to_string());
        let to_parent = norm_to
            .rfind('/')
            .map(|i| {
                if i == 0 {
                    "/".to_string()
                } else {
                    norm_to[..i].to_string()
                }
            })
            .unwrap_or_else(|| "/".to_string());

        let entry = self.resolve_path_entry(&norm_from).await?;
        let old_name = norm_from.rsplit('/').next().unwrap_or("").to_string();

        // v2: path-based rename and move
        if from_parent == to_parent {
            // Pure rename — same directory
            if entry.is_dir {
                let url = self.api_v2_url(
                    "folder/rename",
                    &[("folder_path", &norm_from), ("name", &new_name)],
                );
                let resp = self.get_with_retry(&url).await?;
                Self::ensure_api_ok(resp).await?;
            } else {
                let url = self.api_v2_url(
                    "file/rename",
                    &[("file_path", &norm_from), ("name", &new_name)],
                );
                let resp = self.get_with_retry(&url).await?;
                Self::ensure_api_ok(resp).await?;
            }
        } else {
            // Cross-directory move — v2 path-based
            if entry.is_dir {
                // Folder move: not yet available in v2 — fallback to v1
                let dest_fld_id = self.resolve_fld_id(&to_parent).await?;
                let url = self.api_url_with(
                    "folder/move",
                    &[
                        ("fld_id", &entry.fld_id.to_string()),
                        ("dest_fld_id", &dest_fld_id.to_string()),
                    ],
                );
                self.get_with_retry(&url).await?;
                if new_name != old_name {
                    // After move, rename at new location via v2
                    let moved_path = format!("{}/{}", to_parent.trim_end_matches('/'), old_name);
                    let url = self.api_v2_url(
                        "folder/rename",
                        &[("folder_path", &moved_path), ("name", &new_name)],
                    );
                    let resp = self.get_with_retry(&url).await?;
                    Self::ensure_api_ok(resp).await?;
                }
            } else {
                // File move: v2 set_folder by path
                let dest_folder = format!("{}/", to_parent.trim_end_matches('/'));
                let url = self.api_v2_url(
                    "file/set_folder",
                    &[
                        ("file_path", &norm_from),
                        ("destination_folder_path", &dest_folder),
                    ],
                );
                let resp = self.get_with_retry(&url).await?;
                Self::ensure_api_ok(resp).await?;
                if new_name != old_name {
                    // After move, rename at new location via v2
                    let moved_path = format!("{}/{}", to_parent.trim_end_matches('/'), old_name);
                    let url = self.api_v2_url(
                        "file/rename",
                        &[("file_path", &moved_path), ("name", &new_name)],
                    );
                    let resp = self.get_with_retry(&url).await?;
                    Self::ensure_api_ok(resp).await?;
                }
            }
        }

        self.path_cache.remove(&norm_from);
        self.invalidate_cache_under(&norm_from);
        self.invalidate_cache_under(&from_parent);
        self.invalidate_cache_under(&to_parent);
        Ok(())
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let norm = self.resolve_path(path);

        if norm == "/" {
            return Ok(RemoteEntry::directory("/".to_string(), "/".to_string()));
        }

        let entry = self.resolve_path_entry(&norm).await?;
        let name = norm.rsplit('/').next().unwrap_or("").to_string();

        if entry.is_dir {
            Ok(RemoteEntry {
                name,
                path: norm,
                is_dir: true,
                size: 0,
                modified: entry.modified,
                permissions: None,
                owner: None,
                group: None,
                is_symlink: false,
                link_target: None,
                mime_type: None,
                metadata: HashMap::new(),
            })
        } else {
            let mime = Path::new(&name)
                .extension()
                .and_then(|e| e.to_str())
                .map(|ext| mime_from_ext(ext).to_string());
            Ok(RemoteEntry {
                name,
                path: norm,
                is_dir: false,
                size: entry.size,
                modified: entry.modified,
                permissions: None,
                owner: None,
                group: None,
                is_symlink: false,
                link_target: None,
                mime_type: mime,
                metadata: HashMap::new(),
            })
        }
    }

    async fn size(&mut self, path: &str) -> Result<u64, ProviderError> {
        Ok(self.stat(path).await?.size)
    }

    async fn exists(&mut self, path: &str) -> Result<bool, ProviderError> {
        match self.stat(path).await {
            Ok(_) => Ok(true),
            Err(ProviderError::NotFound(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    async fn keep_alive(&mut self) -> Result<(), ProviderError> {
        if self.connected {
            let url = self.api_url("account/info");
            let _ = self.get_with_retry(&url).await?;
        }
        Ok(())
    }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let url = self.api_url("account/info");
        let resp = self.get_with_retry(&url).await?;
        let info = Self::parse_api::<AccountInfo>(resp).await?;

        // FileLu API returns storage values already in GB
        let used = info.storage_used.unwrap_or(0);
        let left = info.storage_left.unwrap_or(0);
        let total = used + left;

        Ok(format!(
            "FileLu | {} | Used: {} GB / {} GB",
            info.email.unwrap_or_else(|| "user".to_string()),
            used,
            total
        ))
    }

    fn supports_share_links(&self) -> bool {
        true
    }

    async fn create_share_link(
        &mut self,
        path: &str,
        options: ShareLinkOptions,
    ) -> Result<ShareLinkResult, ProviderError> {
        let _ = &options;
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let norm = self.resolve_path(path);
        let file_code = self.resolve_file_code(&norm).await?;

        // Make file public (only_me=0) and return the canonical FileLu link
        let url = self.api_url_with(
            "file/only_me",
            &[("file_code", &file_code), ("only_me", "0")],
        );
        self.get_with_retry(&url).await?;
        Ok(ShareLinkResult {
            url: format!("https://filelu.com/{}", file_code),
            password: None,
            expires_at: None,
        })
    }

    async fn storage_info(&mut self) -> Result<StorageInfo, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let url = self.api_url("account/info");
        let resp = self.get_with_retry(&url).await?;
        let info = Self::parse_api::<AccountInfo>(resp).await?;

        // FileLu API returns storage values in GB; convert to bytes for StorageInfo
        let used = info.storage_used.unwrap_or(0).saturating_mul(1_073_741_824);
        let free = info.storage_left.unwrap_or(0).saturating_mul(1_073_741_824);
        Ok(StorageInfo {
            used,
            free,
            total: used + free,
        })
    }

    fn supports_server_copy(&self) -> bool {
        true
    }

    async fn server_copy(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        // Step 1: Clone the file (creates copy in same folder)
        let norm_from = self.resolve_path(from);
        let file_code = self.resolve_file_code(&norm_from).await?;
        let clone_url = self.api_url_with("file/clone", &[("file_code", &file_code)]);
        let resp = self.get_with_retry(&clone_url).await?;
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(format!("Clone response parse error: {}", e)))?;

        // Step 2: Move the clone to the destination folder if needed
        let norm_to = self.resolve_path(to);
        let (dest_dir, dest_name) = match norm_to.rfind('/') {
            Some(0) => ("/".to_string(), norm_to[1..].to_string()),
            Some(idx) => (norm_to[..idx].to_string(), norm_to[idx + 1..].to_string()),
            None => ("/".to_string(), norm_to.clone()),
        };

        // Extract cloned file code from response (array format)
        if let Some(arr) = body.get("result").and_then(|r| r.as_array()) {
            if let Some(cloned) = arr
                .first()
                .and_then(|e| e.get("filecode").or_else(|| e.get("file_code")))
                .and_then(|v| v.as_str())
            {
                // Move to destination folder
                let dest_fld_id = self.resolve_fld_id(&dest_dir).await?;
                let fld_id_str = dest_fld_id.to_string();
                let move_url = self.api_url_with(
                    "file/set_folder",
                    &[("file_code", cloned), ("fld_id", &fld_id_str)],
                );
                let move_resp = self.get_with_retry(&move_url).await?;
                Self::ensure_api_ok(move_resp).await?;

                // Rename if destination filename differs from source
                let src_name = norm_from.rsplit('/').next().unwrap_or("");
                if !dest_name.is_empty() && dest_name != src_name {
                    let rename_url = self.api_url_with(
                        "file/rename",
                        &[("file_code", cloned), ("name", &dest_name)],
                    );
                    let rename_resp = self.get_with_retry(&rename_url).await?;
                    Self::ensure_api_ok(rename_resp).await?;
                }
            }
        }

        self.invalidate_cache_under(&dest_dir);
        Ok(())
    }

    fn transfer_optimization_hints(&self) -> super::TransferOptimizationHints {
        super::TransferOptimizationHints {
            supports_resume_download: true,
            ..Default::default()
        }
    }
}

// ─── MIME helper ─────────────────────────────────────────────────────────

fn mime_from_ext(ext: &str) -> &'static str {
    match ext.to_lowercase().as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "mp4" => "video/mp4",
        "mkv" => "video/x-matroska",
        "avi" => "video/x-msvideo",
        "mov" => "video/quicktime",
        "mp3" => "audio/mpeg",
        "flac" => "audio/flac",
        "ogg" => "audio/ogg",
        "wav" => "audio/wav",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "tar" => "application/x-tar",
        "gz" => "application/gzip",
        "7z" => "application/x-7z-compressed",
        "rar" => "application/x-rar-compressed",
        "json" => "application/json",
        "xml" => "application/xml",
        "txt" => "text/plain",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" => "text/javascript",
        "ts" => "text/typescript",
        "rs" => "text/x-rust",
        "py" => "text/x-python",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_path() {
        assert_eq!(FileLuProvider::normalize_path("/"), "/");
        assert_eq!(FileLuProvider::normalize_path("/foo/"), "/foo");
        assert_eq!(FileLuProvider::normalize_path("foo/bar"), "/foo/bar");
        assert_eq!(FileLuProvider::normalize_path(""), "/");
    }

    #[test]
    fn test_deserialize_size_from_number() {
        let json = r#"{"size":1024}"#;
        let e: FileEntry = serde_json::from_str(json).unwrap();
        assert_eq!(e.size, 1024);
    }

    #[test]
    fn test_deserialize_size_from_string() {
        let json = r#"{"size":"2048"}"#;
        let e: FileEntry = serde_json::from_str(json).unwrap();
        assert_eq!(e.size, 2048);
    }

    #[test]
    fn test_deserialize_size_null() {
        let json = r#"{"size":null}"#;
        let e: FileEntry = serde_json::from_str(json).unwrap();
        assert_eq!(e.size, 0);
    }

    #[test]
    fn test_deserialize_v2_filelu_fields() {
        let json = r#"{
            "file_code": "218qmsppykci",
            "name": "kodi.exe",
            "size": 77567958,
            "uploaded": "2026-03-12 11:58:00",
            "hash": "73b6231c2379602a2266fbb0b94e9f302629426",
            "direct_link": "https://d1028.cdnguest.space/example/kodi.exe",
            "link": "https://filelu.com/218qmsppykci",
            "public": 0
        }"#;
        let entry: FileEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.name.as_deref(), Some("kodi.exe"));
        assert_eq!(
            entry.hash.as_deref(),
            Some("73b6231c2379602a2266fbb0b94e9f302629426")
        );
        assert_eq!(
            entry.direct_link.as_deref(),
            Some("https://d1028.cdnguest.space/example/kodi.exe")
        );
        assert_eq!(
            entry.link.as_deref(),
            Some("https://filelu.com/218qmsppykci")
        );
        assert_eq!(entry.is_public, Some(false));
    }

    #[test]
    fn test_mime_from_ext() {
        assert_eq!(mime_from_ext("pdf"), "application/pdf");
        assert_eq!(mime_from_ext("MP3"), "audio/mpeg");
        assert_eq!(mime_from_ext("unknown_ext"), "application/octet-stream");
    }
}
