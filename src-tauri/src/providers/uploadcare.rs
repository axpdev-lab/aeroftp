//! Uploadcare Storage Provider
//!
//! Implements StorageProvider for Uploadcare's Upload API + REST API v0.7.
//! Uploadcare exposes a flat media library, so AeroFTP maps project files at `/`
//! with stable internal paths based on each file UUID.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::{multipart, StatusCode};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use tokio_util::io::ReaderStream;

use super::{
    response_bytes_with_limit, sanitize_api_error, MultipartHandle, ProviderConfig, ProviderError,
    ProviderType, RemoteEntry, StorageProvider, TransferOptimizationHints, UploadedPart,
    AEROFTP_USER_AGENT, MAX_DOWNLOAD_TO_BYTES,
};

const API_BASE: &str = "https://api.uploadcare.com";
const API_ACCEPT: &str = "application/vnd.uploadcare-v0.7+json";
const UPLOAD_URL: &str = "https://upload.uploadcare.com/base/";
const MAX_DIRECT_UPLOAD: u64 = 100 * 1024 * 1024;

/// Uploadcare multipart-trait kick-in threshold (S3-T13).
///
/// Mirrors Uploadcare's documented `/multipart/start/` cutover at 10 MiB:
/// below this the runner keeps the direct `base/` POST (100 MiB cap);
/// at or above it switches to the multipart API which has no upper
/// per-file size limit.
const UPLOADCARE_MULTIPART_THRESHOLD: u64 = 10 * 1024 * 1024;

/// Uploadcare multipart preferred part size (S3-T13).
///
/// Uploadcare's multipart API requires every non-final part to be exactly
/// 5 MiB; the final part can be smaller.
const UPLOADCARE_MULTIPART_PART_SIZE: u64 = 5 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct UploadcareConfig {
    pub public_key: String,
    pub secret_key: SecretString,
}

impl UploadcareConfig {
    pub fn from_provider_config(config: &ProviderConfig) -> Result<Self, ProviderError> {
        let public_key = config
            .extra
            .get("public_key")
            .cloned()
            .or_else(|| config.username.clone())
            .ok_or_else(|| {
                ProviderError::InvalidConfig("Uploadcare public API key is required".to_string())
            })?;
        let secret_key = config.password.clone().ok_or_else(|| {
            ProviderError::InvalidConfig("Uploadcare secret API key is required".to_string())
        })?;

        Ok(Self {
            public_key: public_key.trim().to_string(),
            secret_key: SecretString::from(secret_key),
        })
    }
}

#[derive(Debug, Deserialize)]
struct UcFileList {
    #[serde(default)]
    next: Option<String>,
    #[serde(default)]
    results: Vec<UcFile>,
}

#[derive(Debug, Deserialize)]
struct UcFile {
    uuid: String,
    #[serde(default)]
    original_filename: Option<String>,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    mime_type: Option<String>,
    #[serde(default)]
    is_image: bool,
    #[serde(default)]
    datetime_uploaded: Option<String>,
    #[serde(default)]
    datetime_stored: Option<String>,
    #[serde(default)]
    original_file_url: Option<String>,
    #[serde(default)]
    variations: Option<serde_json::Value>,
    #[serde(default)]
    image_info: Option<UcImageInfo>,
}

#[derive(Debug, Deserialize)]
struct UcImageInfo {
    #[serde(default)]
    width: Option<u64>,
    #[serde(default)]
    height: Option<u64>,
    #[serde(default)]
    format: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UcError {
    #[serde(default)]
    detail: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

/// Response payload from `POST /multipart/start/` (S3-T13).
#[derive(Debug, Deserialize)]
struct UcMultipartStart {
    uuid: String,
    parts: Vec<String>,
}

/// Side-band metadata embedded in `MultipartHandle.upload_id` for the
/// Uploadcare multipart API. `parts` carries the pre-signed PUT URLs by
/// 1-based part number; `uuid` is required by the closing
/// `/multipart/complete/` POST.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct UploadcareMultipartMeta {
    uuid: String,
    parts: Vec<String>,
    total: u64,
    part: u64,
    total_parts: u32,
}

impl UploadcareMultipartMeta {
    fn encode(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    fn decode(raw: &str) -> Result<Self, ProviderError> {
        serde_json::from_str(raw).map_err(|e| {
            ProviderError::Other(format!("Uploadcare multipart handle decode failed: {}", e))
        })
    }
}

fn uploadcare_runner_part_size(total: u64) -> u64 {
    UPLOADCARE_MULTIPART_PART_SIZE.min(total.max(1))
}

fn uploadcare_total_parts(total: u64, part: u64) -> u32 {
    let raw = total.div_ceil(part.max(1)).max(1);
    raw.min(u32::MAX as u64) as u32
}

pub struct UploadcareProvider {
    config: UploadcareConfig,
    client: reqwest::Client,
    connected: bool,
}

impl UploadcareProvider {
    pub fn new(config: UploadcareConfig) -> Self {
        let client = reqwest::Client::builder()
            .user_agent(AEROFTP_USER_AGENT)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            config,
            client,
            connected: false,
        }
    }

    fn auth_header(&self) -> String {
        format!(
            "Uploadcare.Simple {}:{}",
            self.config.public_key,
            self.config.secret_key.expose_secret()
        )
    }

    fn rest(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        req.header("Accept", API_ACCEPT)
            .header("Authorization", self.auth_header())
    }

    async fn parse_error(&self, resp: reqwest::Response) -> ProviderError {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let parsed = serde_json::from_str::<UcError>(&body).ok();
        let msg = parsed
            .as_ref()
            .and_then(|e| e.detail.clone().or(e.error.clone()).or(e.message.clone()))
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| sanitize_api_error(&body));

        match status {
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                ProviderError::AuthenticationFailed(msg)
            }
            StatusCode::NOT_FOUND => ProviderError::NotFound(msg),
            s if s.is_client_error() => ProviderError::InvalidConfig(msg),
            s if s.is_server_error() => ProviderError::ServerError(msg),
            _ => ProviderError::Other(format!("HTTP {}: {}", status, msg)),
        }
    }

    async fn list_files(&self) -> Result<Vec<UcFile>, ProviderError> {
        let mut url = format!("{}/files/?limit=1000&stored=true&removed=false", API_BASE);
        let mut files = Vec::new();

        loop {
            validate_uploadcare_api_url(&url)?;
            let resp = self
                .rest(self.client.get(&url))
                .send()
                .await
                .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

            if !resp.status().is_success() {
                return Err(self.parse_error(resp).await);
            }

            let page = resp
                .json::<UcFileList>()
                .await
                .map_err(|e| ProviderError::ParseError(e.to_string()))?;
            files.extend(page.results);

            if let Some(next) = page.next.filter(|n| !n.is_empty()) {
                url = next;
            } else {
                break;
            }
        }

        Ok(files)
    }

    async fn get_file(&self, uuid: &str) -> Result<UcFile, ProviderError> {
        let uuid = parse_uuid(uuid)?;
        let resp = self
            .rest(self.client.get(format!("{}/files/{}/", API_BASE, uuid)))
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(self.parse_error(resp).await);
        }

        resp.json::<UcFile>()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))
    }

    async fn delete_storage(&self, uuid: &str) -> Result<(), ProviderError> {
        let uuid = parse_uuid(uuid)?;
        let resp = self
            .rest(
                self.client
                    .delete(format!("{}/files/{}/storage/", API_BASE, uuid)),
            )
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if resp.status().is_success() || resp.status().as_u16() == 204 {
            Ok(())
        } else {
            Err(self.parse_error(resp).await)
        }
    }
}

#[async_trait]
impl StorageProvider for UploadcareProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn provider_type(&self) -> ProviderType {
        ProviderType::Uploadcare
    }

    fn display_name(&self) -> String {
        "Uploadcare".to_string()
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        let resp = self
            .rest(self.client.get(format!("{}/files/?limit=1", API_BASE)))
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
        if normalize_path(path) != "/" {
            return Err(ProviderError::NotFound(path.to_string()));
        }
        let files = self.list_files().await?;
        Ok(files.iter().map(file_to_entry).collect())
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> {
        Ok("/".to_string())
    }

    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        if normalize_path(path) == "/" {
            Ok(())
        } else {
            Err(ProviderError::InvalidPath(
                "Uploadcare has a flat file library; folders are not supported".to_string(),
            ))
        }
    }

    async fn cd_up(&mut self) -> Result<(), ProviderError> {
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
                "Cannot download a directory".to_string(),
            ));
        }
        let url = entry
            .metadata
            .get("cdn_url")
            .cloned()
            .ok_or_else(|| ProviderError::NotFound("Uploadcare CDN URL missing".to_string()))?;
        validate_cdn_url(&url)?;

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
        let url = entry
            .metadata
            .get("cdn_url")
            .cloned()
            .ok_or_else(|| ProviderError::NotFound("Uploadcare CDN URL missing".to_string()))?;
        validate_cdn_url(&url)?;
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

        let meta = tokio::fs::metadata(local_path)
            .await
            .map_err(ProviderError::IoError)?;
        let total = meta.len();
        if total > MAX_DIRECT_UPLOAD {
            return Err(ProviderError::NotSupported(
                "Uploadcare direct uploads are limited to 100 MiB; multipart upload support is planned".to_string(),
            ));
        }

        let target = normalize_path(remote_path);
        let file_name = if target == "/" || target.ends_with('/') {
            Path::new(local_path)
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .ok_or_else(|| {
                    ProviderError::InvalidPath("Upload path must include a filename".to_string())
                })?
        } else {
            basename(&target).to_string()
        };

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

        let mime = mime_guess::from_path(&file_name).first_or_octet_stream();
        let body = reqwest::Body::wrap_stream(stream);
        let file_part = multipart::Part::stream_with_length(body, total)
            .file_name(file_name.clone())
            .mime_str(mime.as_ref())
            .map_err(|e| ProviderError::InvalidConfig(e.to_string()))?;
        let form = multipart::Form::new()
            .text("UPLOADCARE_PUB_KEY", self.config.public_key.clone())
            .text("UPLOADCARE_STORE", "1")
            .text("filename", file_name)
            .part("file", file_part);

        let resp = self
            .client
            .post(UPLOAD_URL)
            .multipart(form)
            .send()
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(self.parse_error(resp).await);
        }

        let _uploaded: HashMap<String, String> = resp
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;
        Ok(())
    }

    async fn mkdir(&mut self, _path: &str) -> Result<(), ProviderError> {
        Err(ProviderError::NotSupported(
            "Uploadcare folders are not supported; files are listed flat".to_string(),
        ))
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let uuid = parse_uuid(path)?;
        self.delete_storage(&uuid).await
    }

    async fn rmdir(&mut self, _path: &str) -> Result<(), ProviderError> {
        Err(ProviderError::NotSupported(
            "Uploadcare folders are not supported".to_string(),
        ))
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        self.rmdir(path).await
    }

    async fn rename(&mut self, _from: &str, _to: &str) -> Result<(), ProviderError> {
        Err(ProviderError::NotSupported(
            "Uploadcare files are immutable; rename is not supported".to_string(),
        ))
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        if normalize_path(path) == "/" {
            return Ok(RemoteEntry::directory("/".to_string(), "/".to_string()));
        }
        let uuid = parse_uuid(path)?;
        self.get_file(&uuid).await.map(|file| file_to_entry(&file))
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
        Ok("Uploadcare REST API v0.7".to_string())
    }

    fn supports_thumbnails(&self) -> bool {
        true
    }

    async fn get_thumbnail(&mut self, path: &str) -> Result<String, ProviderError> {
        let entry = self.stat(path).await?;
        let url = entry
            .metadata
            .get("cdn_url")
            .cloned()
            .ok_or_else(|| ProviderError::NotFound("Uploadcare CDN URL missing".to_string()))?;
        validate_cdn_url(&url)?;
        Ok(format!("{}-/resize/200x200/", url.trim_end_matches('/')))
    }

    fn supports_find(&self) -> bool {
        true
    }

    async fn find(&mut self, path: &str, pattern: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        if normalize_path(path) != "/" {
            return Err(ProviderError::NotFound(path.to_string()));
        }
        let files = self.list_files().await?;
        Ok(files
            .iter()
            .filter(|file| {
                super::matches_find_pattern(
                    file.original_filename.as_deref().unwrap_or(&file.uuid),
                    pattern,
                )
            })
            .map(file_to_entry)
            .collect())
    }

    fn transfer_optimization_hints(&self) -> TransferOptimizationHints {
        TransferOptimizationHints {
            supports_multipart: true,
            multipart_threshold: UPLOADCARE_MULTIPART_THRESHOLD,
            multipart_part_size: UPLOADCARE_MULTIPART_PART_SIZE,
            // Uploadcare's multipart parts are independent PUTs against
            // pre-signed S3 URLs returned by /multipart/start/; the
            // runner can fan them out freely. 4 matches the AIMD default.
            multipart_max_parallel: 4,
            supports_range_download: true,
            supports_resume_download: true,
            supports_resume_upload: true,
            ..TransferOptimizationHints::default()
        }
    }

    // Shaped-graph multipart trait wiring (S3-T13).
    //
    // Uploadcare's multipart upload protocol:
    //   1. `begin_multipart_upload` → POST `/multipart/start/` with form
    //      `{UPLOADCARE_PUB_KEY, UPLOADCARE_STORE, filename, size,
    //      content_type}`. The response carries the file uuid and a
    //      list of pre-signed PUT URLs (one per 5 MiB part).
    //   2. `upload_part` → PUT the matching signed URL with the chunk
    //      body. Every non-final part must be exactly 5 MiB; the final
    //      part may be smaller.
    //   3. `complete_multipart_upload` → POST `/multipart/complete/`
    //      with form `{UPLOADCARE_PUB_KEY, uuid}`. Returns the canonical
    //      UcFile metadata.
    //   4. `abort_multipart_upload` → best-effort no-op. Uploadcare
    //      garbage-collects orphaned multipart sessions after a few
    //      hours, so the abort never re-raises.
    async fn begin_multipart_upload(
        &mut self,
        remote_path: &str,
        total_size: u64,
        content_type: Option<&str>,
        _local_source_path: Option<&str>,
    ) -> Result<MultipartHandle, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        if total_size == 0 {
            return Err(ProviderError::Other(
                "Uploadcare multipart upload requires non-zero total_size".to_string(),
            ));
        }

        let target = normalize_path(remote_path);
        let file_name = if target == "/" || target.ends_with('/') {
            return Err(ProviderError::InvalidPath(
                "Uploadcare multipart upload requires a filename".to_string(),
            ));
        } else {
            basename(&target).to_string()
        };

        let mime = content_type.map(|s| s.to_string()).unwrap_or_else(|| {
            mime_guess::from_path(&file_name)
                .first_or_octet_stream()
                .as_ref()
                .to_string()
        });

        let form = multipart::Form::new()
            .text("UPLOADCARE_PUB_KEY", self.config.public_key.clone())
            .text("UPLOADCARE_STORE", "1")
            .text("filename", file_name)
            .text("size", total_size.to_string())
            .text("content_type", mime);

        let resp = self
            .client
            .post("https://upload.uploadcare.com/multipart/start/")
            .multipart(form)
            .send()
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(self.parse_error(resp).await);
        }
        let started: UcMultipartStart = resp
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        let part = uploadcare_runner_part_size(total_size);
        let total_parts = uploadcare_total_parts(total_size, part);
        if started.parts.len() as u32 != total_parts {
            return Err(ProviderError::ServerError(format!(
                "Uploadcare /multipart/start/ returned {} URLs but {} parts were expected",
                started.parts.len(),
                total_parts
            )));
        }

        let meta = UploadcareMultipartMeta {
            uuid: started.uuid,
            parts: started.parts,
            total: total_size,
            part,
            total_parts,
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
                "Uploadcare upload_part requires 1-based part_number".to_string(),
            ));
        }
        if data.is_empty() {
            return Err(ProviderError::Other(
                "Uploadcare upload_part received empty data".to_string(),
            ));
        }
        let meta = UploadcareMultipartMeta::decode(&handle.upload_id)?;
        if part_number > meta.total_parts {
            return Err(ProviderError::Other(format!(
                "Uploadcare part {} exceeds declared total_parts {}",
                part_number, meta.total_parts
            )));
        }
        let url = meta
            .parts
            .get((part_number - 1) as usize)
            .ok_or_else(|| {
                ProviderError::Other(format!(
                    "Uploadcare upload_part: no signed URL for part {}",
                    part_number
                ))
            })?
            .clone();

        let resp = self
            .client
            .put(&url)
            .body(data)
            .send()
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::TransferFailed(format!(
                "Uploadcare upload part {} failed ({}): {}",
                part_number,
                status,
                sanitize_api_error(&body)
            )));
        }
        let etag = resp
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
            .unwrap_or_default();
        Ok(UploadedPart { part_number, etag })
    }

    async fn complete_multipart_upload(
        &mut self,
        handle: MultipartHandle,
        parts: Vec<UploadedPart>,
    ) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let meta = UploadcareMultipartMeta::decode(&handle.upload_id)?;
        if parts.len() != meta.total_parts as usize {
            return Err(ProviderError::TransferFailed(format!(
                "Uploadcare complete: expected {} parts, runner committed {}",
                meta.total_parts,
                parts.len()
            )));
        }

        let form = multipart::Form::new()
            .text("UPLOADCARE_PUB_KEY", self.config.public_key.clone())
            .text("uuid", meta.uuid);
        let resp = self
            .client
            .post("https://upload.uploadcare.com/multipart/complete/")
            .multipart(form)
            .send()
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(self.parse_error(resp).await);
        }
        Ok(())
    }

    async fn abort_multipart_upload(
        &mut self,
        _handle: MultipartHandle,
    ) -> Result<(), ProviderError> {
        // Uploadcare does not document an abort endpoint for multipart
        // sessions; orphans are GCed after a few hours. Returning Ok
        // keeps the abort from masking the original transfer error.
        Ok(())
    }
}

fn file_to_entry(file: &UcFile) -> RemoteEntry {
    let name = file
        .original_filename
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| file.uuid.clone());
    let cdn_url = file
        .original_file_url
        .clone()
        .unwrap_or_else(|| format!("https://ucarecdn.com/{}/", file.uuid));

    let mut metadata = HashMap::new();
    metadata.insert("uuid".to_string(), file.uuid.clone());
    metadata.insert("cdn_url".to_string(), cdn_url);
    metadata.insert("is_image".to_string(), file.is_image.to_string());
    if let Some(stored) = &file.datetime_stored {
        metadata.insert("datetime_stored".to_string(), stored.clone());
    }
    if let Some(info) = &file.image_info {
        if let Some(width) = info.width {
            metadata.insert("width".to_string(), width.to_string());
        }
        if let Some(height) = info.height {
            metadata.insert("height".to_string(), height.to_string());
        }
        if let Some(format) = &info.format {
            metadata.insert("format".to_string(), format.clone());
        }
    }
    if let Some(variations) = &file.variations {
        metadata.insert("variations".to_string(), variations.to_string());
    }

    RemoteEntry {
        name,
        path: format!("/{}", file.uuid),
        is_dir: false,
        size: file.size,
        modified: file.datetime_uploaded.clone(),
        permissions: None,
        owner: None,
        group: None,
        is_symlink: false,
        link_target: None,
        mime_type: file.mime_type.clone(),
        metadata,
    }
}

fn parse_uuid(path: &str) -> Result<String, ProviderError> {
    let normalized = normalize_path(path);
    let uuid = normalized
        .trim_start_matches('/')
        .split('/')
        .next()
        .unwrap_or("");
    if uuid.is_empty() {
        return Err(ProviderError::InvalidPath(
            "Missing Uploadcare UUID".to_string(),
        ));
    }
    if uuid.contains('\0') || uuid.contains("..") {
        return Err(ProviderError::InvalidPath(
            "Invalid Uploadcare UUID".to_string(),
        ));
    }
    Ok(uuid.to_string())
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

fn validate_cdn_url(url: &str) -> Result<(), ProviderError> {
    let parsed = url::Url::parse(url)
        .map_err(|e| ProviderError::ServerError(format!("Invalid Uploadcare CDN URL: {}", e)))?;
    if parsed.scheme() != "https" {
        return Err(ProviderError::ServerError(
            "Uploadcare CDN URL must use https".to_string(),
        ));
    }
    let host = parsed.host_str().unwrap_or_default();
    if host != "ucarecdn.com" && !host.ends_with(".ucarecd.net") {
        return Err(ProviderError::ServerError(format!(
            "Unexpected Uploadcare CDN host: {}",
            host
        )));
    }
    Ok(())
}

fn validate_uploadcare_api_url(url: &str) -> Result<(), ProviderError> {
    let parsed = url::Url::parse(url)
        .map_err(|e| ProviderError::ServerError(format!("Invalid Uploadcare API URL: {}", e)))?;
    if parsed.scheme() != "https" || parsed.host_str() != Some("api.uploadcare.com") {
        return Err(ProviderError::ServerError(
            "Unexpected Uploadcare pagination URL".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- S3-T13 multipart trait wiring ----

    #[test]
    fn uploadcare_multipart_meta_roundtrip_preserves_fields() {
        let meta = UploadcareMultipartMeta {
            uuid: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            parts: vec![
                "https://uploadcare-s3/u/1?signature=a".to_string(),
                "https://uploadcare-s3/u/2?signature=b".to_string(),
            ],
            total: 9 * 1024 * 1024,
            part: 5 * 1024 * 1024,
            total_parts: 2,
        };
        let encoded = meta.encode();
        let decoded = UploadcareMultipartMeta::decode(&encoded).expect("decode roundtrip");
        assert_eq!(meta, decoded);
    }

    #[test]
    fn uploadcare_multipart_meta_decode_rejects_garbage() {
        let err = UploadcareMultipartMeta::decode("not-json").unwrap_err();
        assert!(matches!(err, ProviderError::Other(_)));
    }

    #[test]
    fn uploadcare_runner_part_size_clamps_and_never_returns_zero() {
        assert_eq!(uploadcare_runner_part_size(1024), 1024);
        assert_eq!(
            uploadcare_runner_part_size(UPLOADCARE_MULTIPART_PART_SIZE),
            UPLOADCARE_MULTIPART_PART_SIZE
        );
        assert_eq!(
            uploadcare_runner_part_size(50 * 1024 * 1024 * 1024),
            UPLOADCARE_MULTIPART_PART_SIZE
        );
        assert_eq!(uploadcare_runner_part_size(0), 1);
    }

    #[test]
    fn uploadcare_total_parts_matches_runner_formula() {
        let p = UPLOADCARE_MULTIPART_PART_SIZE;
        assert_eq!(uploadcare_total_parts(0, p), 1);
        assert_eq!(uploadcare_total_parts(p, p), 1);
        assert_eq!(uploadcare_total_parts(4 * p, p), 4);
        assert_eq!(uploadcare_total_parts(p + 1, p), 2);
        // part=0 guard
        assert_eq!(uploadcare_total_parts(7, 0), 7);
    }

    #[test]
    fn uploadcare_transfer_hints_advertise_multipart() {
        let cfg = UploadcareConfig {
            public_key: "demo".to_string(),
            secret_key: SecretString::from("demo-secret".to_string()),
        };
        let p = UploadcareProvider::new(cfg);
        let hints = p.transfer_optimization_hints();
        assert!(hints.supports_multipart);
        assert_eq!(hints.multipart_threshold, UPLOADCARE_MULTIPART_THRESHOLD);
        assert_eq!(hints.multipart_part_size, UPLOADCARE_MULTIPART_PART_SIZE);
        assert_eq!(hints.multipart_max_parallel, 4);
        assert!(hints.supports_resume_download);
        assert!(hints.supports_resume_upload);
    }
}
