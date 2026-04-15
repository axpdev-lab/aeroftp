//! OpenDrive Cloud Storage Provider
//!
//! Native REST API integration using session-based authentication.
//! API reference: docs/dev/guides/opendrive/OPENDRIVE-API.md

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use flate2::{write::ZlibEncoder, Compression};
use md5::{Digest, Md5};
use reqwest::multipart;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};
use url::form_urlencoded;

use super::{
    response_bytes_with_limit, sanitize_api_error, ProviderConfig, ProviderError, ProviderType,
    RemoteEntry, ShareLinkCapabilities, ShareLinkOptions, ShareLinkResult, StorageInfo,
    StorageProvider, MAX_DOWNLOAD_TO_BYTES,
};

#[derive(Debug, Clone)]
pub struct OpenDriveConfig {
    pub host: String,
    pub username: String,
    pub password: SecretString,
    pub initial_path: Option<String>,
}

impl OpenDriveConfig {
    pub fn from_provider_config(config: &ProviderConfig) -> Result<Self, ProviderError> {
        let username = config
            .username
            .clone()
            .ok_or_else(|| ProviderError::InvalidConfig("Username is required".into()))?;
        let password = config
            .password
            .clone()
            .ok_or_else(|| ProviderError::InvalidConfig("Password is required".into()))?;

        Ok(Self {
            host: if config.host.is_empty() {
                "dev.opendrive.com".to_string()
            } else {
                config.host.clone()
            },
            username,
            password: password.into(),
            initial_path: config.initial_path.clone(),
        })
    }
}

#[derive(Debug, Deserialize)]
struct LoginResponse {
    #[serde(rename = "SessionID")]
    session_id: Option<String>,
    #[serde(rename = "UserName")]
    user_name: Option<String>,
    #[serde(rename = "UserPlan")]
    user_plan: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SessionInfoResponse {
    #[serde(rename = "SessionID")]
    _session_id: Option<String>,
    #[serde(rename = "UserName")]
    user_name: Option<String>,
    #[serde(rename = "UserPlan")]
    user_plan: Option<String>,
    #[serde(rename = "DriveName")]
    drive_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UserInfoResponse {
    #[serde(rename = "UserName")]
    user_name: Option<String>,
    #[serde(rename = "UserPlan")]
    user_plan: Option<String>,
    #[serde(rename = "MaxStorage")]
    max_storage: Option<serde_json::Value>,
    #[serde(rename = "StorageUsed")]
    storage_used: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct ExpiringFileLinkResponse {
    #[serde(rename = "DownloadLink")]
    download_link: Option<String>,
    #[serde(rename = "StreamingLink")]
    streaming_link: Option<String>,
    #[serde(rename = "Link")]
    link: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ExpiringFolderLinkResponse {
    #[serde(rename = "Link")]
    link: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TrashListResponse {
    #[serde(rename = "Folders", default)]
    folders: Vec<OpenDriveTrashFolder>,
    #[serde(rename = "Files", default)]
    files: Vec<OpenDriveTrashFile>,
}

#[derive(Debug, Deserialize)]
struct OpenDriveTrashFolder {
    #[serde(rename = "FolderID", alias = "FolderId")]
    folder_id: Option<String>,
    #[serde(rename = "Name")]
    name: Option<String>,
    #[serde(rename = "DateTrashed")]
    date_trashed: Option<serde_json::Value>,
    #[serde(rename = "DateModified")]
    date_modified: Option<serde_json::Value>,
    #[serde(rename = "Access")]
    access: Option<serde_json::Value>,
    #[serde(rename = "Link")]
    link: Option<String>,
    #[serde(rename = "Encrypted")]
    encrypted: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct OpenDriveTrashFile {
    #[serde(rename = "FileId", alias = "FileID")]
    file_id: Option<String>,
    #[serde(rename = "Name")]
    name: Option<String>,
    #[serde(rename = "GroupID")]
    _group_id: Option<serde_json::Value>,
    #[serde(rename = "Extension")]
    extension: Option<String>,
    #[serde(rename = "Size")]
    size: Option<serde_json::Value>,
    #[serde(rename = "DateTrashed")]
    date_trashed: Option<serde_json::Value>,
    #[serde(rename = "DateModified")]
    date_modified: Option<serde_json::Value>,
    #[serde(rename = "Access")]
    access: Option<serde_json::Value>,
    #[serde(rename = "Link")]
    link: Option<String>,
    #[serde(rename = "ThumbLink")]
    thumb_link: Option<String>,
    #[serde(rename = "Encrypted")]
    encrypted: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct FolderIdByPathResponse {
    #[serde(rename = "FolderId")]
    folder_id: Option<String>,
    #[serde(rename = "FolderID")]
    folder_id_alt: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FileIdByPathResponse {
    #[serde(rename = "FileId")]
    file_id: Option<String>,
    #[serde(rename = "DownloadLink")]
    _download_link: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FolderListResponse {
    #[serde(rename = "DirUpdateTime")]
    _dir_update_time: Option<serde_json::Value>,
    #[serde(rename = "Folders", default)]
    folders: Vec<OpenDriveFolder>,
    #[serde(rename = "Files", default)]
    files: Vec<OpenDriveFile>,
}

#[derive(Debug, Deserialize)]
struct OpenDriveFolder {
    #[serde(rename = "FolderID", alias = "FolderId")]
    folder_id: Option<String>,
    #[serde(rename = "Name")]
    name: Option<String>,
    #[serde(rename = "DateModified")]
    date_modified: Option<serde_json::Value>,
    #[serde(rename = "Access")]
    access: Option<serde_json::Value>,
    #[serde(rename = "Link")]
    link: Option<String>,
    #[serde(rename = "Encrypted")]
    encrypted: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenDriveFile {
    #[serde(rename = "FileId", alias = "FileID")]
    file_id: Option<String>,
    #[serde(rename = "Name")]
    name: Option<String>,
    #[serde(rename = "Size")]
    size: Option<serde_json::Value>,
    #[serde(rename = "DateModified")]
    date_modified: Option<serde_json::Value>,
    #[serde(rename = "Access")]
    access: Option<serde_json::Value>,
    #[serde(rename = "Extension")]
    extension: Option<String>,
    #[serde(rename = "DownloadLink")]
    download_link: Option<String>,
    #[serde(rename = "StreamingLink")]
    streaming_link: Option<String>,
    #[serde(rename = "ThumbLink")]
    thumb_link: Option<String>,
    #[serde(rename = "FileHash")]
    file_hash: Option<String>,
    #[serde(rename = "Link")]
    link: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FolderInfoResponse {
    #[serde(rename = "FolderID", alias = "FolderId")]
    folder_id: Option<String>,
    #[serde(rename = "Name")]
    name: Option<String>,
    #[serde(rename = "DateModified")]
    date_modified: Option<serde_json::Value>,
    #[serde(rename = "Access")]
    access: Option<serde_json::Value>,
    #[serde(rename = "Link")]
    link: Option<String>,
    #[serde(rename = "Encrypted")]
    encrypted: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FileInfoResponse {
    #[serde(rename = "FileId", alias = "FileID")]
    file_id: Option<String>,
    #[serde(rename = "Name")]
    name: Option<String>,
    #[serde(rename = "Extension")]
    extension: Option<String>,
    #[serde(rename = "Size")]
    size: Option<serde_json::Value>,
    #[serde(rename = "DateModified")]
    date_modified: Option<serde_json::Value>,
    #[serde(rename = "Access")]
    access: Option<serde_json::Value>,
    #[serde(rename = "DownloadLink")]
    download_link: Option<String>,
    #[serde(rename = "StreamingLink")]
    streaming_link: Option<String>,
    #[serde(rename = "ThumbLink")]
    thumb_link: Option<String>,
    #[serde(rename = "FileHash")]
    file_hash: Option<String>,
    #[serde(rename = "Link")]
    link: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CreateFolderResponse {
    #[serde(rename = "FolderID", alias = "FolderId")]
    _folder_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CreateFileResponse {
    #[serde(rename = "FileId", alias = "FileID")]
    file_id: Option<String>,
    #[serde(rename = "TempLocation")]
    temp_location: Option<String>,
    #[serde(rename = "RequireCompression")]
    require_compression: Option<serde_json::Value>,
    #[serde(rename = "RequireHashOnly")]
    _require_hash_only: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct OpenUploadResponse {
    #[serde(rename = "TempLocation")]
    temp_location: Option<String>,
    #[serde(rename = "RequireCompression")]
    require_compression: Option<serde_json::Value>,
    #[serde(rename = "RequireHashOnly")]
    _require_hash_only: Option<serde_json::Value>,
}

fn parse_u64_value(value: Option<&serde_json::Value>) -> u64 {
    match value {
        Some(serde_json::Value::Number(n)) => n.as_u64().unwrap_or(0),
        Some(serde_json::Value::String(s)) => s.parse::<u64>().unwrap_or(0),
        Some(serde_json::Value::Bool(b)) => u64::from(*b),
        _ => 0,
    }
}

fn parse_boolish(value: Option<&serde_json::Value>) -> bool {
    match value {
        Some(serde_json::Value::Bool(b)) => *b,
        Some(serde_json::Value::Number(n)) => n.as_i64().unwrap_or(0) != 0,
        Some(serde_json::Value::String(s)) => {
            matches!(
                s.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        }
        _ => false,
    }
}

fn parse_timestamp_to_iso(value: Option<&serde_json::Value>) -> Option<String> {
    let raw = match value {
        Some(serde_json::Value::Number(n)) => n.as_i64(),
        Some(serde_json::Value::String(s)) => s.parse::<i64>().ok(),
        _ => None,
    }?;
    if raw <= 0 {
        return None;
    }

    let seconds = if raw > 9_999_999_999 { raw / 1000 } else { raw };
    chrono::DateTime::<chrono::Utc>::from_timestamp(seconds, 0).map(|dt| dt.to_rfc3339())
}

fn normalize_path(path: &str) -> Result<String, ProviderError> {
    if path.contains('\0') {
        return Err(ProviderError::InvalidPath("Path contains null byte".into()));
    }

    let mut segments = Vec::new();
    let normalized_path = path.replace('\\', "/");
    for segment in normalized_path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                let _ = segments.pop();
            }
            value => segments.push(value),
        }
    }

    if segments.is_empty() {
        Ok("/".to_string())
    } else {
        Ok(format!("/{}", segments.join("/")))
    }
}

fn split_parent_child(path: &str) -> (String, String) {
    let normalized = if path.is_empty() { "/" } else { path };
    if normalized == "/" {
        return ("/".to_string(), String::new());
    }

    match normalized.rsplit_once('/') {
        Some(("", child)) => ("/".to_string(), child.to_string()),
        Some((parent, child)) => (parent.to_string(), child.to_string()),
        None => ("/".to_string(), normalized.to_string()),
    }
}

pub struct OpenDriveProvider {
    config: OpenDriveConfig,
    client: reqwest::Client,
    api_base: String,
    connected: bool,
    session_id: String,
    current_path: String,
    account_name: Option<String>,
    user_plan: Option<String>,
    /// Tracks last successful API call for proactive session refresh
    last_activity: std::time::Instant,
}

impl OpenDriveProvider {
    pub fn new(config: OpenDriveConfig) -> Self {
        let host = if config.host.starts_with("http://") || config.host.starts_with("https://") {
            config.host.trim_end_matches('/').to_string()
        } else {
            format!("https://{}", config.host.trim_end_matches('/'))
        };

        let client = reqwest::Client::builder()
            .user_agent(crate::providers::AEROFTP_USER_AGENT)
            .connect_timeout(std::time::Duration::from_secs(30))
            .read_timeout(std::time::Duration::from_secs(300))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            config,
            client,
            api_base: format!("{}/api/v1", host),
            connected: false,
            session_id: String::new(),
            current_path: "/".to_string(),
            account_name: None,
            user_plan: None,
            last_activity: std::time::Instant::now(),
        }
    }

    /// Re-authenticate if the session has likely expired (~50 min threshold).
    /// Called before API operations to avoid mid-transfer failures.
    async fn ensure_session(&mut self) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        // OpenDrive sessions expire after ~60 min. Refresh proactively at 50 min.
        if self.last_activity.elapsed() > std::time::Duration::from_secs(50 * 60) {
            tracing::info!(
                "[OpenDrive] Session likely expired ({}s idle), re-authenticating",
                self.last_activity.elapsed().as_secs()
            );
            self.reauth().await?;
        }
        Ok(())
    }

    /// Re-authenticate: login again to get a fresh session_id, preserving current_path.
    async fn reauth(&mut self) -> Result<(), ProviderError> {
        let response: LoginResponse = self
            .post_form(
                "session/login.json",
                &[
                    ("username", self.config.username.clone()),
                    ("passwd", self.config.password.expose_secret().to_string()),
                    ("version", "2.9.7".to_string()),
                    ("partner_id", String::new()),
                ],
            )
            .await?;

        self.session_id = response.session_id.ok_or_else(|| {
            ProviderError::AuthenticationFailed("Missing SessionID on reauth".into())
        })?;
        self.account_name = response.user_name.or(self.account_name.take());
        self.user_plan = response.user_plan.or(self.user_plan.take());
        self.last_activity = std::time::Instant::now();
        tracing::info!("[OpenDrive] Session refreshed successfully");
        Ok(())
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}/{}", self.api_base, path.trim_start_matches('/'))
    }

    fn resolve_path(&self, path: &str) -> Result<String, ProviderError> {
        if path.is_empty() || path == "." {
            return Ok(self.current_path.clone());
        }
        if path.starts_with('/') {
            normalize_path(path)
        } else {
            normalize_path(&format!(
                "{}/{}",
                self.current_path.trim_end_matches('/'),
                path
            ))
        }
    }

    async fn parse_error(&self, resp: reqwest::Response) -> ProviderError {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let message = sanitize_api_error(&body);
        match status.as_u16() {
            400 => ProviderError::InvalidPath(message),
            401 => ProviderError::AuthenticationFailed(message),
            403 => ProviderError::PermissionDenied(message),
            404 => ProviderError::NotFound(message),
            409 => ProviderError::AlreadyExists(message),
            500..=599 => ProviderError::ServerError(message),
            _ => ProviderError::Other(message),
        }
    }

    async fn get_json<T: for<'de> Deserialize<'de>>(&self, url: &str) -> Result<T, ProviderError> {
        let resp = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(self.parse_error(resp).await);
        }

        resp.json::<T>()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))
    }

    async fn post_form<T: for<'de> Deserialize<'de>>(
        &self,
        endpoint: &str,
        params: &[(&str, String)],
    ) -> Result<T, ProviderError> {
        let body = {
            let mut serializer = form_urlencoded::Serializer::new(String::new());
            for (key, value) in params {
                serializer.append_pair(key, value);
            }
            serializer.finish()
        };

        let resp = self
            .client
            .post(self.endpoint(endpoint))
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(body)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(self.parse_error(resp).await);
        }

        resp.json::<T>()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))
    }

    async fn post_form_unit(
        &self,
        endpoint: &str,
        params: &[(&str, String)],
    ) -> Result<(), ProviderError> {
        let body = {
            let mut serializer = form_urlencoded::Serializer::new(String::new());
            for (key, value) in params {
                serializer.append_pair(key, value);
            }
            serializer.finish()
        };

        let resp = self
            .client
            .post(self.endpoint(endpoint))
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(body)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(self.parse_error(resp).await);
        }

        Ok(())
    }

    async fn folder_id_by_path(&self, path: &str) -> Result<String, ProviderError> {
        let normalized = normalize_path(path)?;
        if normalized == "/" {
            return Ok("0".to_string());
        }

        let response: FolderIdByPathResponse = self
            .post_form(
                "folder/idbypath.json",
                &[
                    ("session_id", self.session_id.clone()),
                    ("path", normalized),
                ],
            )
            .await?;

        response
            .folder_id
            .or(response.folder_id_alt)
            .ok_or_else(|| ProviderError::ParseError("Missing FolderId in response".into()))
    }

    async fn file_id_by_path(&self, path: &str) -> Result<FileIdByPathResponse, ProviderError> {
        let normalized = normalize_path(path)?;
        self.post_form(
            "file/idbypath.json",
            &[
                ("session_id", self.session_id.clone()),
                ("path", normalized),
            ],
        )
        .await
    }

    async fn folder_info(&self, folder_id: &str) -> Result<FolderInfoResponse, ProviderError> {
        self.get_json(&self.endpoint(&format!(
            "folder/info.json/{}/{}",
            self.session_id, folder_id
        )))
        .await
    }

    async fn session_info(&self) -> Result<SessionInfoResponse, ProviderError> {
        self.get_json(&self.endpoint(&format!("session/info.json/{}", self.session_id)))
            .await
    }

    async fn user_info(&self) -> Result<UserInfoResponse, ProviderError> {
        self.get_json(&self.endpoint(&format!("users/info.json/{}", self.session_id)))
            .await
    }

    async fn list_folder_response(
        &self,
        folder_id: &str,
    ) -> Result<FolderListResponse, ProviderError> {
        self.get_json(&self.endpoint(&format!(
            "folder/list.json/{}/{}",
            self.session_id, folder_id
        )))
        .await
    }

    async fn find_file_in_parent_by_name(
        &self,
        path: &str,
    ) -> Result<OpenDriveFile, ProviderError> {
        let normalized = normalize_path(path)?;
        let (parent_path, file_name) = split_parent_child(&normalized);
        if file_name.is_empty() {
            return Err(ProviderError::InvalidPath("Missing file name".into()));
        }

        let folder_id = self.folder_id_by_path(&parent_path).await?;
        let response = self.list_folder_response(&folder_id).await?;

        response
            .files
            .into_iter()
            .find(|file| file.name.as_deref() == Some(file_name.as_str()))
            .ok_or(ProviderError::NotFound(normalized))
    }

    async fn resolve_file_id(&self, path: &str) -> Result<String, ProviderError> {
        let normalized = normalize_path(path)?;

        if let Ok(file_lookup) = self.file_id_by_path(&normalized).await {
            if let Some(file_id) = file_lookup.file_id {
                return Ok(file_id);
            }
        }

        let file = self.find_file_in_parent_by_name(&normalized).await?;
        file.file_id.ok_or_else(|| {
            ProviderError::ParseError("Missing FileId in folder list response".into())
        })
    }

    async fn file_info(&self, file_id: &str) -> Result<FileInfoResponse, ProviderError> {
        let mut url = reqwest::Url::parse(&self.endpoint(&format!("file/info.json/{}", file_id)))
            .map_err(|e| ProviderError::InvalidConfig(e.to_string()))?;
        url.query_pairs_mut()
            .append_pair("session_id", &self.session_id);
        self.get_json(url.as_str()).await
    }

    async fn move_file_via_temp_copy(
        &mut self,
        from_path: &str,
        to_path: &str,
    ) -> Result<(), ProviderError> {
        let unique_suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let temp_path = std::env::temp_dir()
            .join(format!(
                "aeroftp-opendrive-move-{}-{}.tmp",
                std::process::id(),
                unique_suffix
            ))
            .to_string_lossy()
            .into_owned();

        self.download(from_path, &temp_path, None).await?;
        let upload_result = self.upload(&temp_path, to_path, None).await;
        let _ = tokio::fs::remove_file(&temp_path).await;
        upload_result?;
        self.delete(from_path).await
    }

    fn folder_to_entry(&self, folder: OpenDriveFolder, parent: &str) -> RemoteEntry {
        let name = folder.name.unwrap_or_else(|| "Unnamed Folder".to_string());
        let path = if parent == "/" {
            format!("/{}", name)
        } else {
            format!("{}/{}", parent.trim_end_matches('/'), name)
        };
        let mut entry = RemoteEntry::directory(name, path);
        entry.modified = parse_timestamp_to_iso(folder.date_modified.as_ref());
        if let Some(folder_id) = folder.folder_id {
            entry
                .metadata
                .insert("opendrive_folder_id".into(), folder_id);
        }
        if let Some(link) = folder.link {
            entry.metadata.insert("opendrive_link".into(), link);
        }
        if let Some(encrypted) = folder.encrypted {
            entry
                .metadata
                .insert("opendrive_encrypted".into(), encrypted);
        }
        entry.metadata.insert(
            "opendrive_access".into(),
            parse_u64_value(folder.access.as_ref()).to_string(),
        );
        entry
    }

    fn file_to_entry(&self, file: OpenDriveFile, parent: &str) -> RemoteEntry {
        let name = file.name.unwrap_or_else(|| "Unnamed File".to_string());
        let path = if parent == "/" {
            format!("/{}", name)
        } else {
            format!("{}/{}", parent.trim_end_matches('/'), name)
        };
        let mut entry = RemoteEntry::file(name, path, parse_u64_value(file.size.as_ref()));
        entry.modified = parse_timestamp_to_iso(file.date_modified.as_ref());
        entry.mime_type = file.extension.clone();
        if let Some(file_id) = file.file_id {
            entry.metadata.insert("opendrive_file_id".into(), file_id);
        }
        if let Some(link) = file.link {
            entry.metadata.insert("opendrive_link".into(), link);
        }
        if let Some(download_link) = file.download_link {
            entry
                .metadata
                .insert("opendrive_download_link".into(), download_link);
        }
        if let Some(streaming_link) = file.streaming_link {
            entry
                .metadata
                .insert("opendrive_streaming_link".into(), streaming_link);
        }
        if let Some(thumb_link) = file.thumb_link {
            entry
                .metadata
                .insert("opendrive_thumb_link".into(), thumb_link);
        }
        if let Some(file_hash) = file.file_hash {
            entry
                .metadata
                .insert("opendrive_file_hash".into(), file_hash);
        }
        entry.metadata.insert(
            "opendrive_access".into(),
            parse_u64_value(file.access.as_ref()).to_string(),
        );
        entry
    }

    async fn compute_md5(&self, local_path: &str) -> Result<String, ProviderError> {
        use tokio::io::AsyncReadExt;

        let mut file = tokio::fs::File::open(local_path)
            .await
            .map_err(ProviderError::IoError)?;
        let mut hasher = Md5::new();
        let mut buffer = vec![0_u8; 64 * 1024];

        loop {
            let read = file
                .read(&mut buffer)
                .await
                .map_err(ProviderError::IoError)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }

        Ok(format!("{:x}", hasher.finalize()))
    }

    async fn upload_chunk(
        &self,
        file_id: &str,
        temp_location: &str,
        file_name: &str,
        _file_size: u64,
        local_path: &str,
        require_compression: bool,
    ) -> Result<bool, ProviderError> {
        let (body_bytes, compressed) = if require_compression {
            let bytes = tokio::fs::read(local_path)
                .await
                .map_err(ProviderError::IoError)?;
            let mut encoder = ZlibEncoder::new(Vec::new(), Compression::new(6));
            encoder
                .write_all(&bytes)
                .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
            let compressed_bytes = encoder
                .finish()
                .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
            (compressed_bytes, true)
        } else {
            (
                tokio::fs::read(local_path)
                    .await
                    .map_err(ProviderError::IoError)?,
                false,
            )
        };
        let chunk_size = body_bytes.len().to_string();
        let body_part = multipart::Part::bytes(body_bytes).file_name(file_name.to_string());

        let mut url = reqwest::Url::parse(&self.endpoint(&format!(
            "upload/upload_file_chunk2.json/{}/{}",
            self.session_id, file_id
        )))
        .map_err(|e| ProviderError::InvalidConfig(e.to_string()))?;

        url.query_pairs_mut()
            .append_pair("temp_location", temp_location)
            .append_pair("chunk_offset", "0")
            .append_pair("chunk_size", &chunk_size);

        let form = multipart::Form::new().part("file_data", body_part);

        let resp = self
            .client
            .post(url)
            .multipart(form)
            .send()
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(self.parse_error(resp).await);
        }

        Ok(compressed)
    }

    async fn stat_file_or_folder(&self, path: &str) -> Result<RemoteEntry, ProviderError> {
        let normalized = normalize_path(path)?;
        if normalized == "/" {
            return Ok(RemoteEntry::directory("/".to_string(), "/".to_string()));
        }

        if let Ok(folder_id) = self.folder_id_by_path(&normalized).await {
            let info = self.folder_info(&folder_id).await?;
            let (parent, _) = split_parent_child(&normalized);
            return Ok(self.folder_to_entry(
                OpenDriveFolder {
                    folder_id: info.folder_id,
                    name: info.name,
                    date_modified: info.date_modified,
                    access: info.access,
                    link: info.link,
                    encrypted: info.encrypted,
                },
                &parent,
            ));
        }

        let file_id = self.resolve_file_id(&normalized).await?;
        let info = self.file_info(&file_id).await?;
        let (parent, _) = split_parent_child(&normalized);
        Ok(self.file_to_entry(
            OpenDriveFile {
                file_id: info.file_id,
                name: info.name,
                extension: info.extension,
                size: info.size,
                date_modified: info.date_modified,
                access: info.access,
                download_link: info.download_link,
                streaming_link: info.streaming_link,
                thumb_link: info.thumb_link,
                file_hash: info.file_hash,
                link: info.link,
            },
            &parent,
        ))
    }

    async fn delete_unit_relative(&self, relative_path: &str) -> Result<(), ProviderError> {
        let resp = self
            .client
            .delete(self.endpoint(relative_path))
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(self.parse_error(resp).await);
        }

        Ok(())
    }

    fn trash_folder_to_entry(&self, folder: OpenDriveTrashFolder) -> RemoteEntry {
        let name = folder.name.unwrap_or_else(|| "Unnamed Folder".to_string());
        let mut entry = RemoteEntry::directory(name.clone(), format!("/Trash/{}", name));
        entry.modified = parse_timestamp_to_iso(folder.date_trashed.as_ref())
            .or_else(|| parse_timestamp_to_iso(folder.date_modified.as_ref()));
        if let Some(folder_id) = folder.folder_id {
            entry.metadata.insert("opendrive_item_id".into(), folder_id);
        }
        entry
            .metadata
            .insert("opendrive_trash_type".into(), "folder".into());
        if let Some(link) = folder.link {
            entry.metadata.insert("opendrive_link".into(), link);
        }
        entry.metadata.insert(
            "opendrive_access".into(),
            parse_u64_value(folder.access.as_ref()).to_string(),
        );
        entry.metadata.insert(
            "opendrive_encrypted".into(),
            parse_u64_value(folder.encrypted.as_ref()).to_string(),
        );
        entry
    }

    fn trash_file_to_entry(&self, file: OpenDriveTrashFile) -> RemoteEntry {
        let name = file.name.unwrap_or_else(|| "Unnamed File".to_string());
        let mut entry = RemoteEntry::file(
            name.clone(),
            format!("/Trash/{}", name),
            parse_u64_value(file.size.as_ref()),
        );
        entry.modified = parse_timestamp_to_iso(file.date_trashed.as_ref())
            .or_else(|| parse_timestamp_to_iso(file.date_modified.as_ref()));
        entry.mime_type = file.extension.clone();
        if let Some(file_id) = file.file_id {
            entry.metadata.insert("opendrive_item_id".into(), file_id);
        }
        entry
            .metadata
            .insert("opendrive_trash_type".into(), "file".into());
        if let Some(link) = file.link {
            entry.metadata.insert("opendrive_link".into(), link);
        }
        if let Some(thumb_link) = file.thumb_link {
            entry
                .metadata
                .insert("opendrive_thumb_link".into(), thumb_link);
        }
        entry.metadata.insert(
            "opendrive_access".into(),
            parse_u64_value(file.access.as_ref()).to_string(),
        );
        entry.metadata.insert(
            "opendrive_encrypted".into(),
            parse_u64_value(file.encrypted.as_ref()).to_string(),
        );
        entry
    }

    pub async fn list_trash(&mut self) -> Result<Vec<RemoteEntry>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let response: TrashListResponse = self
            .get_json(&self.endpoint(&format!("folder/trashlist.json/{}", self.session_id)))
            .await?;

        let mut entries = Vec::with_capacity(response.folders.len() + response.files.len());
        for folder in response.folders {
            entries.push(self.trash_folder_to_entry(folder));
        }
        for file in response.files {
            entries.push(self.trash_file_to_entry(file));
        }
        Ok(entries)
    }

    pub async fn restore_from_trash(
        &mut self,
        item_id: &str,
        is_dir: bool,
    ) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        if is_dir {
            self.post_form_unit(
                "folder/restore.json",
                &[
                    ("session_id", self.session_id.clone()),
                    ("folder_id", item_id.to_string()),
                ],
            )
            .await
        } else {
            self.post_form_unit(
                "file/restore.json",
                &[
                    ("session_id", self.session_id.clone()),
                    ("file_id", item_id.to_string()),
                ],
            )
            .await
        }
    }

    pub async fn permanent_delete_from_trash(
        &mut self,
        item_id: &str,
        is_dir: bool,
    ) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        if is_dir {
            self.post_form_unit(
                "folder/remove.json",
                &[
                    ("session_id", self.session_id.clone()),
                    ("folder_id", item_id.to_string()),
                ],
            )
            .await
        } else {
            self.delete_unit_relative(&format!("file.json/{}/{}", self.session_id, item_id))
                .await
        }
    }

    pub async fn empty_trash(&mut self) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        self.delete_unit_relative(&format!("folder/trash.json/{}", self.session_id))
            .await
    }
}

#[async_trait]
impl StorageProvider for OpenDriveProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn provider_type(&self) -> ProviderType {
        ProviderType::OpenDrive
    }

    fn display_name(&self) -> String {
        "OpenDrive".to_string()
    }

    fn account_email(&self) -> Option<String> {
        self.account_name.clone()
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        let response: LoginResponse = self
            .post_form(
                "session/login.json",
                &[
                    ("username", self.config.username.clone()),
                    ("passwd", self.config.password.expose_secret().to_string()),
                    ("version", "2.9.7".to_string()),
                    ("partner_id", String::new()),
                ],
            )
            .await?;

        self.session_id = response
            .session_id
            .ok_or_else(|| ProviderError::AuthenticationFailed("Missing SessionID".into()))?;
        self.account_name = response.user_name;
        self.user_plan = response.user_plan;
        self.connected = true;
        self.last_activity = std::time::Instant::now();

        if let Some(initial_path) = &self.config.initial_path {
            let normalized = normalize_path(initial_path)?;
            let _ = self.folder_id_by_path(&normalized).await?;
            self.current_path = normalized;
        }

        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        if !self.session_id.is_empty() {
            let _ = self
                .post_form_unit(
                    "session/logout.json",
                    &[("session_id", self.session_id.clone())],
                )
                .await;
        }

        self.connected = false;
        self.session_id.clear();
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        self.ensure_session().await?;

        let resolved = self.resolve_path(path)?;
        let folder_id = self.folder_id_by_path(&resolved).await?;
        let result: Result<FolderListResponse, _> = self
            .get_json(&self.endpoint(&format!(
                "folder/list.json/{}/{}",
                self.session_id, folder_id
            )))
            .await;

        // Retry once on auth failure (session expired mid-operation)
        let response = match result {
            Err(ProviderError::AuthenticationFailed(_)) => {
                tracing::warn!("[OpenDrive] Session expired during list, re-authenticating");
                self.reauth().await?;
                let folder_id = self.folder_id_by_path(&resolved).await?;
                self.get_json(&self.endpoint(&format!(
                    "folder/list.json/{}/{}",
                    self.session_id, folder_id
                )))
                .await?
            }
            other => other?,
        };

        self.last_activity = std::time::Instant::now();
        let mut entries = Vec::with_capacity(response.folders.len() + response.files.len());
        for folder in response.folders {
            entries.push(self.folder_to_entry(folder, &resolved));
        }
        for file in response.files {
            entries.push(self.file_to_entry(file, &resolved));
        }
        Ok(entries)
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> {
        Ok(self.current_path.clone())
    }

    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        self.ensure_session().await?;
        let resolved = self.resolve_path(path)?;
        let _ = self.folder_id_by_path(&resolved).await?;
        self.current_path = resolved;
        Ok(())
    }

    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        if self.current_path == "/" {
            return Ok(());
        }
        let (parent, _) = split_parent_child(&self.current_path);
        self.current_path = if parent.is_empty() {
            "/".to_string()
        } else {
            parent
        };
        Ok(())
    }

    async fn download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        self.ensure_session().await?;

        let resolved = self.resolve_path(remote_path)?;
        let file_id = self.resolve_file_id(&resolved).await?;

        let mut url =
            reqwest::Url::parse(&self.endpoint(&format!("download/file.json/{}", file_id)))
                .map_err(|e| ProviderError::InvalidConfig(e.to_string()))?;
        url.query_pairs_mut()
            .append_pair("session_id", &self.session_id);

        let resp = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(self.parse_error(resp).await);
        }

        let total = resp.content_length().unwrap_or(0);
        let mut atomic = super::atomic_write::AtomicFile::new(local_path)
            .await
            .map_err(ProviderError::IoError)?;

        use futures_util::StreamExt;

        let mut downloaded = 0_u64;
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
            atomic
                .write_all(&chunk)
                .await
                .map_err(ProviderError::IoError)?;
            downloaded += chunk.len() as u64;
            if let Some(ref cb) = on_progress {
                cb(downloaded, total);
            }
        }
        atomic.commit().await.map_err(ProviderError::IoError)?;
        self.last_activity = std::time::Instant::now();
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
        self.ensure_session().await?;

        let resolved = self.resolve_path(remote_path)?;
        let file_id = self.resolve_file_id(&resolved).await?;

        let mut url =
            reqwest::Url::parse(&self.endpoint(&format!("download/file.json/{}", file_id)))
                .map_err(|e| ProviderError::InvalidConfig(e.to_string()))?;
        url.query_pairs_mut()
            .append_pair("session_id", &self.session_id);

        let url_str = url.to_string();

        super::http_resumable_download(
            local_path,
            |range_header| {
                let mut req = self.client.get(&url_str);
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
        self.ensure_session().await?;
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let resolved = self.resolve_path(remote_path)?;
        let file_id = self.resolve_file_id(&resolved).await?;

        let mut url =
            reqwest::Url::parse(&self.endpoint(&format!("download/file.json/{}", file_id)))
                .map_err(|e| ProviderError::InvalidConfig(e.to_string()))?;
        url.query_pairs_mut()
            .append_pair("session_id", &self.session_id);

        let resp = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(self.parse_error(resp).await);
        }

        response_bytes_with_limit(resp, MAX_DOWNLOAD_TO_BYTES).await
    }

    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        self.ensure_session().await?;

        let resolved = self.resolve_path(remote_path)?;
        let (parent_path, file_name) = split_parent_child(&resolved);
        if file_name.is_empty() {
            return Err(ProviderError::InvalidPath("Missing file name".into()));
        }

        let folder_id = self.folder_id_by_path(&parent_path).await?;
        let metadata = tokio::fs::metadata(local_path)
            .await
            .map_err(ProviderError::IoError)?;
        let file_size = metadata.len();
        let file_hash = self.compute_md5(local_path).await?;

        if let Some(ref cb) = on_progress {
            cb(0, file_size);
        }

        let created: CreateFileResponse = self
            .post_form(
                "upload/create_file.json",
                &[
                    ("session_id", self.session_id.clone()),
                    ("folder_id", folder_id),
                    ("file_name", file_name.clone()),
                    ("file_size", file_size.to_string()),
                    ("file_hash", file_hash.clone()),
                    ("open_if_exists", "1".to_string()),
                ],
            )
            .await?;

        let file_id = created
            .file_id
            .ok_or_else(|| ProviderError::ParseError("Missing FileId from create_file".into()))?;

        let opened: OpenUploadResponse = self
            .post_form(
                "upload/open_file_upload.json",
                &[
                    ("session_id", self.session_id.clone()),
                    ("file_id", file_id.clone()),
                    ("file_size", file_size.to_string()),
                    ("file_hash", file_hash.clone()),
                ],
            )
            .await?;

        let require_compression = parse_boolish(
            opened
                .require_compression
                .as_ref()
                .or(created.require_compression.as_ref()),
        );
        let temp_location = opened
            .temp_location
            .or(created.temp_location)
            .ok_or_else(|| {
                ProviderError::ParseError("Missing TempLocation from upload flow".into())
            })?;
        let mut file_compressed = false;

        if file_size > 0 {
            file_compressed = self
                .upload_chunk(
                    &file_id,
                    &temp_location,
                    &file_name,
                    file_size,
                    local_path,
                    require_compression,
                )
                .await?;
        }

        let file_time = metadata
            .modified()
            .ok()
            .and_then(|mtime| mtime.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs().to_string())
            .unwrap_or_else(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|duration| duration.as_secs().to_string())
                    .unwrap_or_else(|_| "0".to_string())
            });

        self.post_form_unit(
            "upload/close_file_upload.json",
            &[
                ("session_id", self.session_id.clone()),
                ("file_id", file_id),
                ("file_size", file_size.to_string()),
                ("temp_location", temp_location),
                ("file_time", file_time),
                ("file_hash", file_hash),
                (
                    "file_compressed",
                    if file_compressed { "1" } else { "0" }.to_string(),
                ),
            ],
        )
        .await?;

        if let Some(ref cb) = on_progress {
            cb(file_size, file_size);
        }

        self.last_activity = std::time::Instant::now();
        Ok(())
    }

    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        self.ensure_session().await?;

        let resolved = self.resolve_path(path)?;
        if resolved == "/" {
            return Ok(());
        }
        let (parent_path, folder_name) = split_parent_child(&resolved);
        if folder_name.is_empty() {
            return Err(ProviderError::InvalidPath("Missing folder name".into()));
        }
        let parent_id = self.folder_id_by_path(&parent_path).await?;

        let _: CreateFolderResponse = self
            .post_form(
                "folder.json",
                &[
                    ("session_id", self.session_id.clone()),
                    ("folder_name", folder_name),
                    ("folder_sub_parent", parent_id),
                    ("folder_is_public", "0".to_string()),
                ],
            )
            .await?;
        Ok(())
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        self.ensure_session().await?;
        let resolved = self.resolve_path(path)?;
        let file_id = self.resolve_file_id(&resolved).await?;
        self.post_form_unit(
            "file/trash.json",
            &[
                ("session_id", self.session_id.clone()),
                ("file_id", file_id),
            ],
        )
        .await
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        self.rmdir_recursive(path).await
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        self.ensure_session().await?;
        let resolved = self.resolve_path(path)?;
        if resolved == "/" {
            return Err(ProviderError::InvalidPath(
                "Cannot remove root folder".into(),
            ));
        }
        let folder_id = self.folder_id_by_path(&resolved).await?;
        self.post_form_unit(
            "folder/trash.json",
            &[
                ("session_id", self.session_id.clone()),
                ("folder_id", folder_id),
            ],
        )
        .await
    }

    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        self.ensure_session().await?;

        let from_resolved = self.resolve_path(from)?;
        let to_resolved = self.resolve_path(to)?;
        if from_resolved == "/" {
            return Err(ProviderError::InvalidPath(
                "Cannot rename root folder".into(),
            ));
        }

        let (from_parent_path, _) = split_parent_child(&from_resolved);
        let (to_parent_path, to_name) = split_parent_child(&to_resolved);
        if to_name.is_empty() {
            return Err(ProviderError::InvalidPath("Missing target name".into()));
        }

        if let Ok(folder_id) = self.folder_id_by_path(&from_resolved).await {
            if from_parent_path == to_parent_path {
                self.post_form_unit(
                    "folder/rename.json",
                    &[
                        ("session_id", self.session_id.clone()),
                        ("folder_id", folder_id),
                        ("folder_name", to_name),
                    ],
                )
                .await?;
                return Ok(());
            }

            let to_parent_id = self.folder_id_by_path(&to_parent_path).await?;
            self.post_form_unit(
                "folder/move_copy.json",
                &[
                    ("session_id", self.session_id.clone()),
                    ("folder_id", folder_id),
                    ("dst_folder_id", to_parent_id),
                    ("move", "true".to_string()),
                    ("new_folder_name", to_name),
                ],
            )
            .await?;
            return Ok(());
        }

        let file_id = self.resolve_file_id(&from_resolved).await?;

        if from_parent_path == to_parent_path {
            self.post_form_unit(
                "file/rename.json",
                &[
                    ("session_id", self.session_id.clone()),
                    ("file_id", file_id),
                    ("new_file_name", to_name),
                ],
            )
            .await?;
            return Ok(());
        }

        let to_parent_id = self.folder_id_by_path(&to_parent_path).await?;
        match self
            .post_form_unit(
                "file/move_copy.json",
                &[
                    ("session_id", self.session_id.clone()),
                    ("src_file_id", file_id),
                    ("dst_folder_id", to_parent_id),
                    ("move", "true".to_string()),
                    ("overwrite_if_exists", "true".to_string()),
                    ("new_file_name", to_name),
                ],
            )
            .await
        {
            Ok(()) => Ok(()),
            Err(ProviderError::InvalidPath(message))
                if message.contains("Invalid value specified for `move`") =>
            {
                self.move_file_via_temp_copy(&from_resolved, &to_resolved)
                    .await
            }
            Err(error) => Err(error),
        }
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        self.stat_file_or_folder(path).await
    }

    async fn size(&mut self, path: &str) -> Result<u64, ProviderError> {
        Ok(self.stat(path).await?.size)
    }

    async fn exists(&mut self, path: &str) -> Result<bool, ProviderError> {
        match self.stat(path).await {
            Ok(_) => Ok(true),
            Err(ProviderError::NotFound(_)) => Ok(false),
            Err(err) => Err(err),
        }
    }

    async fn keep_alive(&mut self) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        // Proactively refresh session if idle too long (50 min threshold)
        if self.last_activity.elapsed() > std::time::Duration::from_secs(50 * 60) {
            self.reauth().await?;
        } else {
            let _: SessionInfoResponse = self.session_info().await?;
            self.last_activity = std::time::Instant::now();
        }
        Ok(())
    }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let info: SessionInfoResponse = self.session_info().await?;
        let mut parts = vec!["OpenDrive".to_string()];
        if let Some(name) = info
            .drive_name
            .or(info.user_name)
            .or(self.account_name.clone())
        {
            parts.push(name);
        }
        if let Some(plan) = info.user_plan.or(self.user_plan.clone()) {
            parts.push(format!("({})", plan));
        }
        Ok(parts.join(" "))
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
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let resolved = self.resolve_path(path)?;
        let expiry = chrono::Utc::now()
            + chrono::Duration::seconds(
                options.expires_in_secs.unwrap_or(365 * 24 * 60 * 60) as i64
            );
        let expiry_date = expiry.format("%Y-%m-%d").to_string();
        let expires_at_str = expiry.to_rfc3339();

        if let Ok(folder_id) = self.folder_id_by_path(&resolved).await {
            let response: ExpiringFolderLinkResponse = self
                .get_json(&self.endpoint(&format!(
                    "folder/expiringlink.json/{}/{}/0/{}/1",
                    self.session_id, expiry_date, folder_id
                )))
                .await?;
            let url = response
                .link
                .ok_or_else(|| ProviderError::ParseError("Missing folder expiring link".into()))?;
            return Ok(ShareLinkResult {
                url,
                password: None,
                expires_at: Some(expires_at_str),
            });
        }

        let file_id = self.resolve_file_id(&resolved).await?;
        let response: ExpiringFileLinkResponse = self
            .get_json(&self.endpoint(&format!(
                "file/expiringlink.json/{}/{}/0/{}/1",
                self.session_id, expiry_date, file_id
            )))
            .await?;

        let url = response
            .download_link
            .or(response.link)
            .or(response.streaming_link)
            .ok_or_else(|| ProviderError::ParseError("Missing file expiring link".into()))?;

        Ok(ShareLinkResult {
            url,
            password: None,
            expires_at: Some(expires_at_str),
        })
    }

    fn supports_thumbnails(&self) -> bool {
        true
    }

    async fn storage_info(&mut self) -> Result<StorageInfo, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let info = self.user_info().await?;
        if self.account_name.is_none() {
            self.account_name = info.user_name.clone();
        }
        if self.user_plan.is_none() {
            self.user_plan = info.user_plan.clone();
        }

        let total_raw = parse_u64_value(info.max_storage.as_ref());
        let used_raw = parse_u64_value(info.storage_used.as_ref());

        if total_raw == 0 {
            return Err(ProviderError::ParseError(
                "OpenDrive users/info did not return MaxStorage".into(),
            ));
        }

        // OpenDrive reports MaxStorage in MiB and StorageUsed in bytes.
        // Verified live against the dashboard: MaxStorage=5120 => 5 GB, StorageUsed=64314 => 63 KB.
        let total = total_raw.saturating_mul(1024 * 1024);
        let used = used_raw;

        Ok(StorageInfo {
            used,
            total,
            free: total.saturating_sub(used),
        })
    }

    async fn get_thumbnail(&mut self, path: &str) -> Result<String, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let resolved = self.resolve_path(path)?;
        let file_id = self.resolve_file_id(&resolved).await?;
        let info = self.file_info(&file_id).await?;
        let thumb_url = info.thumb_link.unwrap_or_else(|| {
            format!(
                "{}/api/file/thumb.json/{}?session_id={}",
                self.api_base.trim_end_matches("/api/v1"),
                file_id,
                self.session_id
            )
        });

        let resp = self
            .client
            .get(&thumb_url)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(self.parse_error(resp).await);
        }

        let bytes = resp
            .bytes()
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        Ok(format!("data:image/jpeg;base64,{}", BASE64.encode(&bytes)))
    }

    fn transfer_optimization_hints(&self) -> super::TransferOptimizationHints {
        super::TransferOptimizationHints {
            supports_resume_download: true,
            ..Default::default()
        }
    }
}
