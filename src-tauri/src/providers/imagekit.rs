//! ImageKit Storage Provider
//!
//! Implements StorageProvider for ImageKit's REST APIs.
//! Authentication: HTTP Basic with the private API key as username and an empty password.
//! API: https://api.imagekit.io/v1 and https://upload.imagekit.io/api/v1/files/upload

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::{multipart, StatusCode};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;
use std::path::Path;
use tokio_util::io::ReaderStream;

/// Deserialize a JSON value that may be `null` into a typed default.
///
/// `#[serde(default)]` only fires when a field is *missing*; an explicit
/// `null` still goes through the type's own `Deserialize` impl and trips
/// for non-`Option` targets like `u64` or `Vec<T>`. ImageKit's `/files`
/// listing with `type=all` mixes file and folder entries: folder rows
/// emit `null` for fields that only make sense on files (`size`, `tags`,
/// `mime`, `width`, `height`, ...), so the response would refuse to
/// parse against `Vec<IkFile>` until we tolerate `null` here.
fn null_to_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

use super::{
    response_bytes_with_limit, sanitize_api_error, ProviderConfig, ProviderError, ProviderType,
    RemoteEntry, StorageProvider, TransferOptimizationHints, AEROFTP_USER_AGENT,
    MAX_DOWNLOAD_TO_BYTES,
};

const API_BASE: &str = "https://api.imagekit.io/v1";
const UPLOAD_URL: &str = "https://upload.imagekit.io/api/v1/files/upload";

#[derive(Debug, Clone)]
pub struct ImageKitConfig {
    pub imagekit_id: String,
    pub private_key: SecretString,
    pub initial_path: Option<String>,
}

impl ImageKitConfig {
    pub fn from_provider_config(config: &ProviderConfig) -> Result<Self, ProviderError> {
        let imagekit_id = config
            .extra
            .get("imagekit_id")
            .cloned()
            .or_else(|| config.username.clone())
            .ok_or_else(|| {
                ProviderError::InvalidConfig("ImageKit URL endpoint ID is required".to_string())
            })?;

        let private_key = config.password.clone().ok_or_else(|| {
            ProviderError::InvalidConfig("ImageKit private API key is required".to_string())
        })?;

        Ok(Self {
            imagekit_id: imagekit_id.trim().trim_matches('/').to_string(),
            private_key: SecretString::from(private_key),
            initial_path: config.initial_path.clone(),
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IkFile {
    #[serde(default, deserialize_with = "null_to_default")]
    file_id: String,
    #[serde(default, deserialize_with = "null_to_default")]
    name: String,
    #[serde(default, deserialize_with = "null_to_default")]
    file_path: String,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    thumbnail_url: Option<String>,
    #[serde(default, rename = "type", deserialize_with = "null_to_default")]
    entry_type: String,
    #[serde(default)]
    file_type: Option<String>,
    #[serde(default)]
    mime: Option<String>,
    #[serde(default, deserialize_with = "null_to_default")]
    size: u64,
    #[serde(default)]
    height: Option<u64>,
    #[serde(default)]
    width: Option<u64>,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
    #[serde(default)]
    is_private_file: Option<bool>,
    #[serde(default, deserialize_with = "null_to_default")]
    tags: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IkUploadResponse {
    #[serde(default)]
    file_id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    file_path: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    thumbnail_url: Option<String>,
    #[serde(default)]
    file_type: Option<String>,
    #[serde(default)]
    size: u64,
}

#[derive(Debug, Deserialize)]
struct IkError {
    #[serde(default)]
    message: String,
    #[serde(default)]
    error: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IkBulkJob {
    #[serde(default)]
    job_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RenameFileRequest<'a> {
    file_path: &'a str,
    new_file_name: &'a str,
    purge_cache: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MoveFileRequest<'a> {
    source_file_path: &'a str,
    destination_path: &'a str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CopyFileRequest<'a> {
    source_file_path: &'a str,
    destination_path: &'a str,
    include_file_versions: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FolderRequest<'a> {
    folder_name: &'a str,
    parent_folder_path: &'a str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DeleteFolderRequest<'a> {
    folder_path: &'a str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BulkFolderRequest<'a> {
    source_folder_path: &'a str,
    destination_path: &'a str,
    include_file_versions: bool,
}

pub struct ImageKitProvider {
    config: ImageKitConfig,
    client: reqwest::Client,
    connected: bool,
    current_path: String,
}

impl ImageKitProvider {
    pub fn new(config: ImageKitConfig) -> Self {
        let current_path = normalize_path(config.initial_path.as_deref().unwrap_or("/"));
        let client = reqwest::Client::builder()
            .user_agent(AEROFTP_USER_AGENT)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            config,
            client,
            connected: false,
            current_path,
        }
    }

    fn auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        req.basic_auth(self.config.private_key.expose_secret(), Some(""))
    }

    fn resolve_path(&self, path: &str) -> String {
        if path.trim().is_empty() {
            return self.current_path.clone();
        }
        if path.starts_with('/') {
            normalize_path(path)
        } else {
            normalize_path(&format!("{}/{}", self.current_path.trim_end_matches('/'), path))
        }
    }

    async fn parse_error(&self, resp: reqwest::Response) -> ProviderError {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let parsed = serde_json::from_str::<IkError>(&body).ok();
        let msg = parsed
            .as_ref()
            .map(|e| {
                if !e.message.trim().is_empty() {
                    e.message.clone()
                } else {
                    e.error.clone()
                }
            })
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| sanitize_api_error(&body));

        match status {
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                ProviderError::AuthenticationFailed(msg)
            }
            StatusCode::NOT_FOUND => ProviderError::NotFound(msg),
            StatusCode::CONFLICT => ProviderError::AlreadyExists(msg),
            s if s.is_client_error() => ProviderError::InvalidConfig(msg),
            s if s.is_server_error() => ProviderError::ServerError(msg),
            _ => ProviderError::Other(format!("HTTP {}: {}", status, msg)),
        }
    }

    async fn list_raw(&self, path: &str) -> Result<Vec<IkFile>, ProviderError> {
        validate_path(path)?;
        // ImageKit `/files` accepts only `file | file-version | folder | all`
        // for the `type` query parameter. Sending `file-and-folder` (an
        // earlier guess from the API docs) gets rejected at runtime with
        // `Invalid configuration: Your request contains invalid value for
        // type parameter ...`. `all` returns folders + files in one call,
        // which is what the rest of this provider already expects.
        let url = format!(
            "{}/files?path={}&type=all&limit=1000&skip=0",
            API_BASE,
            urlencoding::encode(path)
        );
        let resp = self
            .auth(self.client.get(url))
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(self.parse_error(resp).await);
        }

        resp.json::<Vec<IkFile>>()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))
    }

    async fn find_entry(&self, path: &str) -> Result<IkFile, ProviderError> {
        let resolved = normalize_path(path);
        if resolved == "/" {
            return Ok(IkFile {
                file_id: String::new(),
                name: "/".to_string(),
                file_path: "/".to_string(),
                url: None,
                thumbnail_url: None,
                entry_type: "folder".to_string(),
                file_type: None,
                mime: None,
                size: 0,
                height: None,
                width: None,
                created_at: None,
                updated_at: None,
                is_private_file: None,
                tags: Vec::new(),
            });
        }

        let parent = parent_path(&resolved);
        let name = basename(&resolved);
        let items = self.list_raw(&parent).await?;
        items
            .into_iter()
            .find(|item| normalize_path(&item.file_path) == resolved || item.name == name)
            .ok_or(ProviderError::NotFound(resolved))
    }

    async fn delete_file_by_path(&self, path: &str) -> Result<(), ProviderError> {
        let item = self.find_entry(path).await?;
        if item.file_id.is_empty() {
            return Err(ProviderError::InvalidPath(format!(
                "No file id for {}",
                path
            )));
        }

        let resp = self
            .auth(self.client.delete(format!("{}/files/{}", API_BASE, item.file_id)))
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if resp.status().is_success() {
            Ok(())
        } else {
            Err(self.parse_error(resp).await)
        }
    }

    async fn delete_folder_by_path(&self, path: &str) -> Result<(), ProviderError> {
        let folder_path = folder_path(path);
        let resp = self
            .auth(self.client.delete(format!("{}/folder/", API_BASE)))
            .json(&DeleteFolderRequest {
                folder_path: &folder_path,
            })
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if resp.status().is_success() {
            Ok(())
        } else {
            Err(self.parse_error(resp).await)
        }
    }

    async fn copy_file(&self, from: &str, to: &str) -> Result<(), ProviderError> {
        let source = normalize_path(from);
        let dest_parent = folder_path(&parent_path(&normalize_path(to)));
        let resp = self
            .auth(self.client.post(format!("{}/files/copy", API_BASE)))
            .json(&CopyFileRequest {
                source_file_path: &source,
                destination_path: &dest_parent,
                include_file_versions: false,
            })
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(self.parse_error(resp).await);
        }

        let src_name = basename(&source);
        let target = normalize_path(to);
        let dest_name = basename(&target);
        if src_name != dest_name {
            let copied = format!("{}/{}", dest_parent.trim_end_matches('/'), src_name);
            self.rename_file(&copied, dest_name).await?;
        }
        Ok(())
    }

    async fn rename_file(&self, from: &str, new_name: &str) -> Result<(), ProviderError> {
        let source = normalize_path(from);
        let resp = self
            .auth(self.client.put(format!("{}/files/rename", API_BASE)))
            .json(&RenameFileRequest {
                file_path: &source,
                new_file_name: new_name,
                purge_cache: true,
            })
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if resp.status().is_success() {
            Ok(())
        } else {
            Err(self.parse_error(resp).await)
        }
    }

    async fn move_file(&self, from: &str, to: &str) -> Result<(), ProviderError> {
        let source = normalize_path(from);
        let target = normalize_path(to);
        let src_parent = parent_path(&source);
        let dest_parent = folder_path(&parent_path(&target));

        if folder_path(&src_parent) != dest_parent {
            let resp = self
                .auth(self.client.put(format!("{}/files/move", API_BASE)))
                .json(&MoveFileRequest {
                    source_file_path: &source,
                    destination_path: &dest_parent,
                })
                .send()
                .await
                .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

            if !resp.status().is_success() {
                return Err(self.parse_error(resp).await);
            }
        }

        let src_name = basename(&source);
        let dest_name = basename(&target);
        if src_name != dest_name {
            let moved_path = format!("{}/{}", dest_parent.trim_end_matches('/'), src_name);
            self.rename_file(&moved_path, dest_name).await?;
        }
        Ok(())
    }

    async fn start_folder_job(
        &self,
        endpoint: &str,
        from: &str,
        to: &str,
        include_versions: bool,
    ) -> Result<(), ProviderError> {
        let source = folder_path(from);
        let destination = folder_path(to);
        let resp = self
            .auth(self.client.post(format!("{}/bulkJobs/{}", API_BASE, endpoint)))
            .json(&BulkFolderRequest {
                source_folder_path: &source,
                destination_path: &destination,
                include_file_versions: include_versions,
            })
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(self.parse_error(resp).await);
        }

        if let Ok(job) = resp.json::<IkBulkJob>().await {
            if !job.job_id.is_empty() {
                tracing::debug!("ImageKit folder job started: {}", job.job_id);
            }
        }
        Ok(())
    }
}

#[async_trait]
impl StorageProvider for ImageKitProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn provider_type(&self) -> ProviderType {
        ProviderType::ImageKit
    }

    fn display_name(&self) -> String {
        "ImageKit".to_string()
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        let resp = self
            .auth(self.client.get(format!("{}/files?limit=1&skip=0", API_BASE)))
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(self.parse_error(resp).await);
        }

        self.connected = true;
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.connected = false;
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let resolved = self.resolve_path(path);
        let items = self.list_raw(&resolved).await?;
        Ok(items.iter().map(file_to_entry).collect())
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> {
        Ok(self.current_path.clone())
    }

    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let resolved = self.resolve_path(path);
        let entry = self.stat(&resolved).await?;
        if !entry.is_dir {
            return Err(ProviderError::InvalidPath(format!(
                "'{}' is not a directory",
                resolved
            )));
        }
        self.current_path = resolved;
        Ok(())
    }

    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        self.current_path = parent_path(&self.current_path);
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
        let entry = self.stat(remote_path).await?;
        if entry.is_dir {
            return Err(ProviderError::InvalidPath(
                "Cannot download a directory as a file".to_string(),
            ));
        }
        let url = entry
            .metadata
            .get("url")
            .cloned()
            .ok_or_else(|| ProviderError::NotFound("ImageKit URL missing".to_string()))?;
        validate_download_url(&url)?;

        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(ProviderError::TransferFailed(format!(
                "Download failed: HTTP {}",
                resp.status()
            )));
        }

        let total = resp.content_length().unwrap_or(entry.size);
        let mut atomic = super::atomic_write::AtomicFile::new(local_path)
            .await
            .map_err(ProviderError::IoError)?;
        let mut downloaded = 0u64;
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
        Ok(())
    }

    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let entry = self.stat(remote_path).await?;
        if entry.is_dir {
            return Err(ProviderError::InvalidPath(
                "Cannot download a directory as bytes".to_string(),
            ));
        }
        let url = entry
            .metadata
            .get("url")
            .cloned()
            .ok_or_else(|| ProviderError::NotFound("ImageKit URL missing".to_string()))?;
        validate_download_url(&url)?;
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(ProviderError::TransferFailed(format!(
                "Download failed: HTTP {}",
                resp.status()
            )));
        }
        response_bytes_with_limit(resp, MAX_DOWNLOAD_TO_BYTES).await
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

        let target = self.resolve_path(remote_path);
        let file_name = if target.ends_with('/') {
            Path::new(local_path)
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .ok_or_else(|| {
                    ProviderError::InvalidPath("Upload path must include a filename".to_string())
                })?
        } else {
            basename(&target).to_string()
        };
        let folder = folder_path(&parent_path(&target));
        let meta = tokio::fs::metadata(local_path)
            .await
            .map_err(ProviderError::IoError)?;
        let total = meta.len();
        let file = tokio::fs::File::open(local_path)
            .await
            .map_err(ProviderError::IoError)?;

        let mut uploaded = 0u64;
        let progress_cb = on_progress;
        let stream = ReaderStream::with_capacity(file, 64 * 1024).map(move |chunk| {
            if let Ok(bytes) = &chunk {
                uploaded += bytes.len() as u64;
                if let Some(ref cb) = progress_cb {
                    cb(uploaded, total);
                }
            }
            chunk
        });

        let body = reqwest::Body::wrap_stream(stream);
        let mime = mime_guess::from_path(&file_name).first_or_octet_stream();
        let file_part = multipart::Part::stream_with_length(body, total)
            .file_name(file_name.clone())
            .mime_str(mime.as_ref())
            .map_err(|e| ProviderError::InvalidConfig(e.to_string()))?;

        let form = multipart::Form::new()
            .part("file", file_part)
            .text("fileName", file_name)
            .text("folder", folder)
            .text("useUniqueFileName", "false")
            .text("overwriteFile", "true");

        let resp = self
            .auth(self.client.post(UPLOAD_URL))
            .multipart(form)
            .send()
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(self.parse_error(resp).await);
        }

        let uploaded = resp
            .json::<IkUploadResponse>()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;
        let _ = (
            uploaded.file_id,
            uploaded.name,
            uploaded.file_path,
            uploaded.url,
            uploaded.thumbnail_url,
            uploaded.file_type,
            uploaded.size,
        );
        Ok(())
    }

    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let resolved = self.resolve_path(path);
        let name = basename(&resolved);
        if name.is_empty() {
            return Ok(());
        }
        let parent = folder_path(&parent_path(&resolved));

        let resp = self
            .auth(self.client.post(format!("{}/folder/", API_BASE)))
            .json(&FolderRequest {
                folder_name: name,
                parent_folder_path: &parent,
            })
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if resp.status().is_success() {
            Ok(())
        } else {
            Err(self.parse_error(resp).await)
        }
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let resolved = self.resolve_path(path);
        let entry = self.stat(&resolved).await?;
        if entry.is_dir {
            self.rmdir(&resolved).await
        } else {
            self.delete_file_by_path(&resolved).await
        }
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let resolved = self.resolve_path(path);
        self.delete_folder_by_path(&resolved).await
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let resolved = self.resolve_path(path);
        let mut stack = vec![resolved.clone()];
        let mut dirs = Vec::new();

        while let Some(dir) = stack.pop() {
            let entries = self.list_raw(&dir).await?;
            for entry in entries {
                if entry.entry_type == "folder" {
                    stack.push(normalize_path(&entry.file_path));
                } else {
                    self.delete_file_by_path(&entry.file_path).await?;
                }
            }
            dirs.push(dir);
        }

        for dir in dirs.into_iter().rev() {
            self.delete_folder_by_path(&dir).await?;
        }
        Ok(())
    }

    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let source = self.resolve_path(from);
        let target = self.resolve_path(to);
        let entry = self.stat(&source).await?;
        if entry.is_dir {
            let src_name = basename(&source);
            let dest_name = basename(&target);
            if src_name != dest_name {
                return Err(ProviderError::NotSupported(
                    "ImageKit folder rename is not exposed as a synchronous API; move to a destination folder is supported".to_string(),
                ));
            }
            self.start_folder_job("moveFolder", &source, &parent_path(&target), false)
                .await
        } else {
            self.move_file(&source, &target).await
        }
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let resolved = self.resolve_path(path);
        self.find_entry(&resolved).await.map(|item| file_to_entry(&item))
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
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        Ok(())
    }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        Ok(format!("ImageKit endpoint: {}", self.config.imagekit_id))
    }

    fn supports_server_copy(&self) -> bool {
        true
    }

    async fn server_copy(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let source = self.resolve_path(from);
        let target = self.resolve_path(to);
        let entry = self.stat(&source).await?;
        if entry.is_dir {
            self.start_folder_job("copyFolder", &source, &parent_path(&target), false)
                .await
        } else {
            self.copy_file(&source, &target).await
        }
    }

    fn supports_thumbnails(&self) -> bool {
        true
    }

    async fn get_thumbnail(&mut self, path: &str) -> Result<String, ProviderError> {
        let entry = self.stat(path).await?;
        entry
            .metadata
            .get("thumbnail_url")
            .or_else(|| entry.metadata.get("url"))
            .cloned()
            .ok_or_else(|| ProviderError::NotFound("No ImageKit thumbnail URL".to_string()))
    }

    fn supports_find(&self) -> bool {
        true
    }

    async fn find(
        &mut self,
        path: &str,
        pattern: &str,
    ) -> Result<Vec<RemoteEntry>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let root = self.resolve_path(path);
        let mut stack = vec![root];
        let mut matches = Vec::new();
        while let Some(dir) = stack.pop() {
            for item in self.list_raw(&dir).await? {
                if item.entry_type == "folder" {
                    stack.push(normalize_path(&item.file_path));
                }
                if super::matches_find_pattern(&item.name, pattern) {
                    matches.push(file_to_entry(&item));
                }
            }
        }
        Ok(matches)
    }

    fn transfer_optimization_hints(&self) -> TransferOptimizationHints {
        TransferOptimizationHints {
            supports_range_download: true,
            supports_server_checksum: false,
            supports_resume_download: true,
            ..TransferOptimizationHints::default()
        }
    }
}

fn file_to_entry(item: &IkFile) -> RemoteEntry {
    let is_dir = item.entry_type == "folder";
    let mut metadata = HashMap::new();
    if !item.file_id.is_empty() {
        metadata.insert("file_id".to_string(), item.file_id.clone());
    }
    if let Some(url) = &item.url {
        metadata.insert("url".to_string(), url.clone());
    }
    if let Some(url) = &item.thumbnail_url {
        metadata.insert("thumbnail_url".to_string(), url.clone());
    }
    if let Some(kind) = &item.file_type {
        metadata.insert("file_type".to_string(), kind.clone());
    }
    if let Some(height) = item.height {
        metadata.insert("height".to_string(), height.to_string());
    }
    if let Some(width) = item.width {
        metadata.insert("width".to_string(), width.to_string());
    }
    if let Some(is_private) = item.is_private_file {
        metadata.insert("is_private_file".to_string(), is_private.to_string());
    }
    if !item.tags.is_empty() {
        metadata.insert("tags".to_string(), item.tags.join(","));
    }

    RemoteEntry {
        name: item.name.clone(),
        path: normalize_path(&item.file_path),
        is_dir,
        size: if is_dir { 0 } else { item.size },
        modified: item.updated_at.clone().or_else(|| item.created_at.clone()),
        permissions: None,
        owner: None,
        group: None,
        is_symlink: false,
        link_target: None,
        mime_type: item.mime.clone(),
        metadata,
    }
}

fn validate_path(path: &str) -> Result<(), ProviderError> {
    if path.contains('\0') {
        return Err(ProviderError::InvalidPath(
            "Path contains null byte".to_string(),
        ));
    }
    for component in path.split('/') {
        if component == ".." {
            return Err(ProviderError::InvalidPath(
                "Path traversal (..) not allowed".to_string(),
            ));
        }
    }
    Ok(())
}

fn normalize_path(path: &str) -> String {
    let mut parts = Vec::new();
    for part in path.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            parts.pop();
        } else {
            parts.push(part);
        }
    }
    if parts.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", parts.join("/"))
    }
}

fn basename(path: &str) -> &str {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default()
}

fn parent_path(path: &str) -> String {
    let normalized = normalize_path(path);
    if normalized == "/" {
        return "/".to_string();
    }
    match normalized.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(idx) => normalized[..idx].to_string(),
    }
}

fn folder_path(path: &str) -> String {
    let normalized = normalize_path(path);
    if normalized == "/" {
        "/".to_string()
    } else {
        format!("{}/", normalized.trim_end_matches('/'))
    }
}

fn validate_download_url(url: &str) -> Result<(), ProviderError> {
    let parsed = url::Url::parse(url)
        .map_err(|e| ProviderError::ServerError(format!("Invalid ImageKit URL: {}", e)))?;
    if parsed.scheme() != "https" {
        return Err(ProviderError::ServerError(
            "ImageKit download URL must use https".to_string(),
        ));
    }
    Ok(())
}
