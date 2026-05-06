//! Cloudinary Storage Provider
//!
//! Implements StorageProvider for Cloudinary's Admin and Upload APIs.
//! Authentication: HTTP Basic with the API key as username and the API secret
//! as password against `https://api.cloudinary.com/v1_1/<cloud_name>/`.
//!
//! Folder model: Cloudinary supports two modes, "fixed folders" (legacy, where
//! folders are derived from `public_id` prefixes) and "dynamic folders"
//! (modern, where `asset_folder` is a separate field). This provider tries the
//! dynamic-folder endpoint (`resources/by_asset_folder`) first; on a 4xx
//! response indicating that the account is on fixed folder mode, it falls
//! back to prefix-based listing via `GET /resources/<resource_type>?prefix=`.
//!
//! Download model: assets are served via a CDN URL (`secure_url`) returned by
//! the upload/list endpoints. We download from that URL anonymously. Private
//! / authenticated delivery (`type=authenticated`, signed URLs) is NOT
//! supported in this initial implementation; we restrict to `type=upload`
//! resources, which are publicly addressable.
//!
//! Free tier: 25 monthly credits, where 1 credit = 1 GB storage OR 1 GB
//! bandwidth OR 1000 transformations. Soft limit, see
//! `https://cloudinary.com/pricing`.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::{multipart, StatusCode};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Deserializer};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use tokio_util::io::ReaderStream;

use super::{
    response_bytes_with_limit, sanitize_api_error, ProviderConfig, ProviderError, ProviderType,
    RemoteEntry, StorageProvider, StorageInfo, TransferOptimizationHints, AEROFTP_USER_AGENT,
    MAX_DOWNLOAD_TO_BYTES,
};

/// Tolerant null-to-default deserializer (mirrors imagekit.rs `null_to_default`).
fn null_to_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

const API_HOST: &str = "https://api.cloudinary.com";

#[derive(Debug, Clone)]
pub struct CloudinaryConfig {
    pub cloud_name: String,
    pub api_key: String,
    pub api_secret: SecretString,
    pub initial_path: Option<String>,
}

impl CloudinaryConfig {
    pub fn from_provider_config(config: &ProviderConfig) -> Result<Self, ProviderError> {
        let cloud_name = config
            .extra
            .get("cloud_name")
            .cloned()
            .ok_or_else(|| {
                ProviderError::InvalidConfig("Cloudinary cloud name is required".to_string())
            })?;
        let api_key = config.username.clone().ok_or_else(|| {
            ProviderError::InvalidConfig("Cloudinary API key is required".to_string())
        })?;
        let api_secret = config.password.clone().ok_or_else(|| {
            ProviderError::InvalidConfig("Cloudinary API secret is required".to_string())
        })?;

        let cloud_name = cloud_name.trim().trim_matches('/').to_string();
        if cloud_name.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Cloudinary cloud name cannot be empty".to_string(),
            ));
        }
        // Cloud names are alphanumeric with `-` and `_`. Reject path-traversal
        // characters early so we never embed them into the URL.
        if cloud_name
            .chars()
            .any(|c| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        {
            return Err(ProviderError::InvalidConfig(
                "Cloudinary cloud name must be alphanumeric with - or _".to_string(),
            ));
        }

        Ok(Self {
            cloud_name,
            api_key: api_key.trim().to_string(),
            api_secret: SecretString::from(api_secret),
            initial_path: config.initial_path.clone(),
        })
    }
}

#[derive(Debug, Deserialize)]
struct CloudinaryUploadResponse {
    #[serde(default)]
    public_id: String,
    #[serde(default)]
    secure_url: Option<String>,
    #[serde(default, deserialize_with = "null_to_default")]
    bytes: u64,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    resource_type: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    width: Option<u64>,
    #[serde(default)]
    height: Option<u64>,
    #[serde(default)]
    asset_folder: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
struct CloudinaryResource {
    #[serde(default)]
    asset_id: Option<String>,
    #[serde(default, deserialize_with = "null_to_default")]
    public_id: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    format: Option<String>,
    #[serde(default, deserialize_with = "null_to_default")]
    bytes: u64,
    #[serde(default)]
    secure_url: Option<String>,
    #[serde(default, deserialize_with = "null_to_default")]
    resource_type: String,
    #[serde(default, rename = "type")]
    delivery_type: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    width: Option<u64>,
    #[serde(default)]
    height: Option<u64>,
    #[serde(default)]
    asset_folder: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CloudinaryListResponse {
    #[serde(default)]
    resources: Vec<CloudinaryResource>,
    #[serde(default)]
    next_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CloudinarySubFolder {
    #[serde(default, deserialize_with = "null_to_default")]
    name: String,
    #[serde(default, deserialize_with = "null_to_default")]
    path: String,
}

#[derive(Debug, Deserialize)]
struct CloudinaryFolderListResponse {
    #[serde(default)]
    folders: Vec<CloudinarySubFolder>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct CloudinaryUsageResponse {
    #[serde(default)]
    credits: Option<CloudinaryUsageCredits>,
    #[serde(default)]
    storage: Option<CloudinaryUsageMetric>,
    #[serde(default)]
    bandwidth: Option<CloudinaryUsageMetric>,
    #[serde(default)]
    transformations: Option<CloudinaryUsageMetric>,
    #[serde(default)]
    media_limits: Option<CloudinaryMediaLimits>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct CloudinaryUsageCredits {
    #[serde(default)]
    usage: Option<f64>,
    #[serde(default)]
    limit: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct CloudinaryUsageMetric {
    #[serde(default)]
    usage: Option<u64>,
    #[serde(default)]
    limit: Option<u64>,
}

/// Cloudinary `media_limits` payload (sometimes the storage cap is here
/// instead of `storage.limit`, especially on free / paygo plans).
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct CloudinaryMediaLimits {
    #[serde(default)]
    total_storage_max_size_bytes: Option<u64>,
    #[serde(default)]
    image_max_size_bytes: Option<u64>,
    #[serde(default)]
    video_max_size_bytes: Option<u64>,
    #[serde(default)]
    raw_max_size_bytes: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct CloudinaryError {
    #[serde(default)]
    error: Option<CloudinaryErrorBody>,
}

#[derive(Debug, Deserialize)]
struct CloudinaryErrorBody {
    #[serde(default)]
    message: Option<String>,
}

pub struct CloudinaryProvider {
    config: CloudinaryConfig,
    client: reqwest::Client,
    connected: bool,
    current_path: String,
    /// Per-public_id cache of the resource_type returned by listing.
    /// Used to issue the correct DELETE URL (image/video/raw).
    resource_types: Mutex<HashMap<String, String>>,
    /// Whether the account is on dynamic-folder mode. None until probed.
    /// Set to `Some(true)` after a successful `by_asset_folder` call,
    /// `Some(false)` after we fall back to prefix-based listing.
    dynamic_folder_mode: Mutex<Option<bool>>,
}

impl CloudinaryProvider {
    pub fn new(config: CloudinaryConfig) -> Self {
        let current_path = normalize_path(config.initial_path.as_deref().unwrap_or(""));
        let client = reqwest::Client::builder()
            .user_agent(AEROFTP_USER_AGENT)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            config,
            client,
            connected: false,
            current_path,
            resource_types: Mutex::new(HashMap::new()),
            dynamic_folder_mode: Mutex::new(None),
        }
    }

    fn api_base(&self) -> String {
        format!("{}/v1_1/{}", API_HOST, self.config.cloud_name)
    }

    fn auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        req.basic_auth(
            &self.config.api_key,
            Some(self.config.api_secret.expose_secret()),
        )
    }

    fn resolve_path(&self, path: &str) -> String {
        if path.trim().is_empty() {
            return self.current_path.clone();
        }
        if path.starts_with('/') {
            normalize_path(path)
        } else {
            normalize_path(&format!("{}/{}", self.current_path, path))
        }
    }

    async fn parse_error(&self, resp: reqwest::Response) -> ProviderError {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let parsed = serde_json::from_str::<CloudinaryError>(&body).ok();
        let msg = parsed
            .and_then(|e| e.error.and_then(|b| b.message))
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

    fn cache_resource_type(&self, public_id: &str, resource_type: &str) {
        if public_id.is_empty() || resource_type.is_empty() {
            return;
        }
        if let Ok(mut map) = self.resource_types.lock() {
            map.insert(public_id.to_string(), resource_type.to_string());
        }
    }

    fn cached_resource_type(&self, public_id: &str) -> Option<String> {
        self.resource_types
            .lock()
            .ok()
            .and_then(|map| map.get(public_id).cloned())
    }

    async fn list_subfolders(&self, path: &str) -> Result<Vec<CloudinarySubFolder>, ProviderError> {
        let trimmed = path.trim_matches('/');
        let url = if trimmed.is_empty() {
            format!("{}/folders", self.api_base())
        } else {
            format!(
                "{}/folders/{}",
                self.api_base(),
                encode_folder_segments(trimmed)
            )
        };

        let resp = self
            .auth(self.client.get(&url))
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(self.parse_error(resp).await);
        }

        let parsed = resp
            .json::<CloudinaryFolderListResponse>()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;
        Ok(parsed.folders)
    }

    /// List files under an asset folder using the dynamic-folder endpoint.
    /// Returns Ok(None) if the endpoint signals fixed-folder mode (we should
    /// fall back to the prefix-based path).
    async fn list_files_dynamic(
        &self,
        folder: &str,
    ) -> Result<Option<Vec<CloudinaryResource>>, ProviderError> {
        let mut resources = Vec::new();
        let mut cursor: Option<String> = None;
        let folder_arg = folder.trim_matches('/');

        loop {
            let mut url = format!(
                "{}/resources/by_asset_folder?asset_folder={}&max_results=500",
                self.api_base(),
                urlencoding::encode(folder_arg)
            );
            if let Some(ref c) = cursor {
                url.push_str(&format!("&next_cursor={}", urlencoding::encode(c)));
            }

            let resp = self
                .auth(self.client.get(&url))
                .send()
                .await
                .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

            let status = resp.status();
            if !status.is_success() {
                // Heuristic: a 400 with body mentioning "fixed folders" or
                // unknown parameter `asset_folder` means the account is on the
                // legacy mode. Fall back instead of erroring out.
                if status.as_u16() == 400 {
                    let body = resp.text().await.unwrap_or_default();
                    let lower = body.to_ascii_lowercase();
                    if lower.contains("asset_folder")
                        || lower.contains("fixed folder")
                        || lower.contains("dynamic folder")
                    {
                        return Ok(None);
                    }
                    return Err(ProviderError::InvalidConfig(sanitize_api_error(&body)));
                }
                return Err(self.parse_error(resp).await);
            }

            let page = resp
                .json::<CloudinaryListResponse>()
                .await
                .map_err(|e| ProviderError::ParseError(e.to_string()))?;
            resources.extend(page.resources);
            match page.next_cursor.filter(|c| !c.is_empty()) {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }

        Ok(Some(resources))
    }

    /// Prefix-based listing for fixed-folder accounts. Iterates the three
    /// resource types because the prefix endpoint scopes by resource_type.
    async fn list_files_prefix(
        &self,
        folder: &str,
    ) -> Result<Vec<CloudinaryResource>, ProviderError> {
        let prefix = folder.trim_matches('/');
        let prefix_arg = if prefix.is_empty() {
            String::new()
        } else {
            format!("{}/", prefix)
        };

        let mut all = Vec::new();
        for kind in ["image", "video", "raw"] {
            let mut cursor: Option<String> = None;
            loop {
                let mut url = format!(
                    "{}/resources/{}?type=upload&max_results=500",
                    self.api_base(),
                    kind
                );
                if !prefix_arg.is_empty() {
                    url.push_str(&format!("&prefix={}", urlencoding::encode(&prefix_arg)));
                }
                if let Some(ref c) = cursor {
                    url.push_str(&format!("&next_cursor={}", urlencoding::encode(c)));
                }

                let resp = self
                    .auth(self.client.get(&url))
                    .send()
                    .await
                    .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

                if !resp.status().is_success() {
                    return Err(self.parse_error(resp).await);
                }

                let page = resp
                    .json::<CloudinaryListResponse>()
                    .await
                    .map_err(|e| ProviderError::ParseError(e.to_string()))?;
                for mut item in page.resources {
                    if item.resource_type.is_empty() {
                        item.resource_type = kind.to_string();
                    }
                    // In fixed-folder mode the public_id carries the prefix;
                    // narrow to direct children only (no further `/`).
                    let pid = item.public_id.clone();
                    let stripped = pid.strip_prefix(&prefix_arg).unwrap_or(&pid);
                    if stripped.contains('/') {
                        continue;
                    }
                    all.push(item);
                }
                match page.next_cursor.filter(|c| !c.is_empty()) {
                    Some(next) => cursor = Some(next),
                    None => break,
                }
            }
        }
        Ok(all)
    }

    async fn list_files(&self, folder: &str) -> Result<Vec<CloudinaryResource>, ProviderError> {
        let mode = self.dynamic_folder_mode.lock().ok().and_then(|m| *m);
        if let Some(false) = mode {
            return self.list_files_prefix(folder).await;
        }

        match self.list_files_dynamic(folder).await? {
            Some(items) => {
                if let Ok(mut m) = self.dynamic_folder_mode.lock() {
                    *m = Some(true);
                }
                Ok(items)
            }
            None => {
                tracing::warn!(
                    "Cloudinary cloud '{}' is on fixed-folder mode; falling back to prefix listing",
                    self.config.cloud_name
                );
                if let Ok(mut m) = self.dynamic_folder_mode.lock() {
                    *m = Some(false);
                }
                self.list_files_prefix(folder).await
            }
        }
    }

    fn primary_resource_type(&self, item: &CloudinaryResource) -> String {
        if !item.resource_type.is_empty() {
            item.resource_type.clone()
        } else {
            "image".to_string()
        }
    }

    async fn delete_resource(
        &self,
        public_id: &str,
        resource_type: &str,
    ) -> Result<bool, ProviderError> {
        let url = format!(
            "{}/resources/{}/upload?type=upload&public_ids[]={}",
            self.api_base(),
            resource_type,
            urlencoding::encode(public_id)
        );
        let resp = self
            .auth(self.client.delete(&url))
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(self.parse_error(resp).await);
        }

        // Cloudinary returns `{"deleted": {"<id>": "deleted" | "not_found"}}`
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;
        let status = body
            .get("deleted")
            .and_then(|d| d.get(public_id))
            .and_then(|s| s.as_str())
            .unwrap_or("");
        Ok(status == "deleted")
    }

    async fn delete_file_with_fallback(&self, public_id: &str) -> Result<(), ProviderError> {
        if let Some(kind) = self.cached_resource_type(public_id) {
            if self.delete_resource(public_id, &kind).await? {
                return Ok(());
            }
        }
        for kind in ["image", "video", "raw"] {
            match self.delete_resource(public_id, kind).await {
                Ok(true) => return Ok(()),
                Ok(false) => continue,
                Err(ProviderError::NotFound(_)) => continue,
                Err(e) => return Err(e),
            }
        }
        Err(ProviderError::NotFound(public_id.to_string()))
    }

    async fn fetch_usage(&self) -> Result<CloudinaryUsageResponse, ProviderError> {
        let resp = self
            .auth(self.client.get(format!("{}/usage", self.api_base())))
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(self.parse_error(resp).await);
        }
        resp.json::<CloudinaryUsageResponse>()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))
    }
}

#[async_trait]
impl StorageProvider for CloudinaryProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn provider_type(&self) -> ProviderType {
        ProviderType::Cloudinary
    }

    fn display_name(&self) -> String {
        "Cloudinary".to_string()
    }

    fn account_email(&self) -> Option<String> {
        Some(self.config.cloud_name.clone())
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        // /usage requires admin auth; a 200 confirms the credentials.
        let _ = self.fetch_usage().await?;
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
        let folder = self.resolve_path(path);
        let folder_norm = folder.trim_matches('/').to_string();

        let subfolders = self.list_subfolders(&folder_norm).await.unwrap_or_default();
        let files = self.list_files(&folder_norm).await?;

        // Cache resource_types from this listing for subsequent deletes.
        for f in &files {
            self.cache_resource_type(&f.public_id, &f.resource_type);
        }

        let mut entries: Vec<RemoteEntry> = subfolders
            .into_iter()
            .map(|sf| folder_to_entry(&sf, &folder_norm))
            .collect();
        entries.extend(files.iter().map(|f| resource_to_entry(f, &folder_norm)));
        Ok(entries)
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> {
        if self.current_path.is_empty() {
            Ok("/".to_string())
        } else {
            Ok(format!("/{}", self.current_path))
        }
    }

    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let resolved = self.resolve_path(path);
        // Verify by listing subfolders of the parent (cheap probe).
        if !resolved.is_empty() {
            let parent = parent_segments(&resolved);
            let target = basename(&resolved).to_string();
            let folders = self.list_subfolders(&parent).await?;
            if !folders.iter().any(|f| f.name == target) {
                return Err(ProviderError::NotFound(format!("/{}", resolved)));
            }
        }
        self.current_path = resolved;
        Ok(())
    }

    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        self.current_path = parent_segments(&self.current_path);
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
            .get("secure_url")
            .cloned()
            .ok_or_else(|| ProviderError::NotFound("Cloudinary delivery URL missing".to_string()))?;
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
            .get("secure_url")
            .cloned()
            .ok_or_else(|| ProviderError::NotFound("Cloudinary delivery URL missing".to_string()))?;
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
        let file_name = if target.is_empty() || target.ends_with('/') {
            Path::new(local_path)
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .ok_or_else(|| {
                    ProviderError::InvalidPath("Upload path must include a filename".to_string())
                })?
        } else {
            basename(&target).to_string()
        };
        let folder = parent_segments(&target);

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

        // Upload payload includes auth via the standard signed-or-basic flow.
        // We use Basic auth with key:secret (same path as admin endpoints), which
        // Cloudinary accepts on the upload API for server-side calls.
        let mut form = multipart::Form::new()
            .part("file", file_part)
            .text("use_filename", "true")
            .text("unique_filename", "false");
        if !folder.is_empty() {
            form = form
                .text("asset_folder", folder.clone())
                .text("folder", folder.clone());
        }

        let upload_url = format!("{}/auto/upload", self.api_base());
        let resp = self
            .auth(self.client.post(&upload_url))
            .multipart(form)
            .send()
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(self.parse_error(resp).await);
        }

        let parsed = resp
            .json::<CloudinaryUploadResponse>()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;
        if let Some(ref kind) = parsed.resource_type {
            self.cache_resource_type(&parsed.public_id, kind);
        }
        let _ = (
            parsed.secure_url,
            parsed.bytes,
            parsed.format,
            parsed.created_at,
            parsed.width,
            parsed.height,
            parsed.asset_folder,
        );
        Ok(())
    }

    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let resolved = self.resolve_path(path);
        let trimmed = resolved.trim_matches('/');
        if trimmed.is_empty() {
            return Ok(());
        }
        let url = format!(
            "{}/folders/{}",
            self.api_base(),
            encode_folder_segments(trimmed)
        );
        let resp = self
            .auth(self.client.post(&url))
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
            let public_id = entry
                .metadata
                .get("public_id")
                .cloned()
                .ok_or_else(|| ProviderError::NotFound("Missing Cloudinary public_id".to_string()))?;
            self.delete_file_with_fallback(&public_id).await
        }
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let resolved = self.resolve_path(path);
        let trimmed = resolved.trim_matches('/');
        if trimmed.is_empty() {
            return Err(ProviderError::InvalidPath(
                "Cannot remove the Cloudinary root".to_string(),
            ));
        }
        let url = format!(
            "{}/folders/{}",
            self.api_base(),
            encode_folder_segments(trimmed)
        );
        let resp = self
            .auth(self.client.delete(&url))
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        if resp.status().is_success() {
            Ok(())
        } else {
            Err(self.parse_error(resp).await)
        }
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let resolved = self.resolve_path(path);
        let trimmed = resolved.trim_matches('/').to_string();
        if trimmed.is_empty() {
            return Err(ProviderError::InvalidPath(
                "Cannot remove the Cloudinary root".to_string(),
            ));
        }
        let mut stack = vec![trimmed.clone()];
        let mut dirs = Vec::new();

        while let Some(dir) = stack.pop() {
            let subfolders = self.list_subfolders(&dir).await.unwrap_or_default();
            for sf in subfolders {
                let subpath = if sf.path.is_empty() {
                    if dir.is_empty() {
                        sf.name.clone()
                    } else {
                        format!("{}/{}", dir, sf.name)
                    }
                } else {
                    sf.path
                };
                stack.push(subpath);
            }
            let files = self.list_files(&dir).await?;
            for f in files {
                let kind = self.primary_resource_type(&f);
                let _ = self.delete_resource(&f.public_id, &kind).await?;
            }
            dirs.push(dir);
        }

        for dir in dirs.into_iter().rev() {
            let url = format!(
                "{}/folders/{}",
                self.api_base(),
                encode_folder_segments(&dir)
            );
            let resp = self
                .auth(self.client.delete(&url))
                .send()
                .await
                .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
            if !resp.status().is_success() && resp.status() != StatusCode::NOT_FOUND {
                return Err(self.parse_error(resp).await);
            }
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
            // PUT /folders/<from> with form to_folder=<to>
            let from_seg = source.trim_matches('/');
            let url = format!(
                "{}/folders/{}?to_folder={}",
                self.api_base(),
                encode_folder_segments(from_seg),
                urlencoding::encode(target.trim_matches('/'))
            );
            let resp = self
                .auth(self.client.put(&url))
                .send()
                .await
                .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
            if resp.status().is_success() {
                Ok(())
            } else {
                Err(self.parse_error(resp).await)
            }
        } else {
            let kind = entry
                .metadata
                .get("resource_type")
                .cloned()
                .or_else(|| {
                    entry
                        .metadata
                        .get("public_id")
                        .and_then(|pid| self.cached_resource_type(pid))
                })
                .unwrap_or_else(|| "image".to_string());
            let from_pid = entry
                .metadata
                .get("public_id")
                .cloned()
                .unwrap_or_else(|| source.trim_matches('/').to_string());
            let to_pid = target.trim_matches('/').to_string();
            let url = format!(
                "{}/{}/rename?from_public_id={}&to_public_id={}&overwrite=true",
                self.api_base(),
                kind,
                urlencoding::encode(&from_pid),
                urlencoding::encode(&to_pid)
            );
            let resp = self
                .auth(self.client.post(&url))
                .send()
                .await
                .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
            if resp.status().is_success() {
                Ok(())
            } else {
                Err(self.parse_error(resp).await)
            }
        }
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let resolved = self.resolve_path(path);
        let trimmed = resolved.trim_matches('/').to_string();
        if trimmed.is_empty() {
            return Ok(RemoteEntry::directory("/".to_string(), "/".to_string()));
        }

        // Try as folder: list its parent and look for it in subfolders.
        let parent = parent_segments(&resolved);
        let name = basename(&resolved).to_string();
        if let Ok(folders) = self.list_subfolders(&parent).await {
            if let Some(folder) = folders.into_iter().find(|f| f.name == name) {
                return Ok(folder_to_entry(&folder, &parent));
            }
        }

        // Treat as file: list parent files and look for it.
        let files = self.list_files(&parent).await?;
        for f in &files {
            self.cache_resource_type(&f.public_id, &f.resource_type);
        }
        let pid_no_ext = trimmed.clone();
        let entry = files.into_iter().find(|f| {
            f.public_id == pid_no_ext || resource_display_name(f) == name
        });
        match entry {
            Some(f) => Ok(resource_to_entry(&f, &parent)),
            None => Err(ProviderError::NotFound(format!("/{}", trimmed))),
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
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let _ = self.fetch_usage().await?;
        Ok(())
    }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        Ok(format!("Cloudinary cloud: {}", self.config.cloud_name))
    }

    async fn storage_info(&mut self) -> Result<StorageInfo, ProviderError> {
        let usage = self.fetch_usage().await?;
        let used = usage
            .storage
            .as_ref()
            .and_then(|m| m.usage)
            .unwrap_or(0);
        // Cloudinary does not expose a storage-byte quota for free / paygo
        // plans. The plan-level cap is `credits.limit` (e.g. 25 credits/mo),
        // but credits are fungible across storage, bandwidth and
        // transformations: mapping them 1:1 to bytes would mislead the user
        // (they don't have 25 GiB of dedicated storage). Paid plans do expose
        // `storage.limit` directly, and some return the cap inside
        // `media_limits.total_storage_max_size_bytes`. Use those when present;
        // otherwise leave `total = 0` and let the UI render the "credit-based"
        // placeholder. A dedicated `credits used/total` metric is tracked
        // separately (see UsageMetric design).
        let total = usage
            .storage
            .as_ref()
            .and_then(|m| m.limit)
            .filter(|&v| v > 0)
            .or_else(|| {
                usage
                    .media_limits
                    .as_ref()
                    .and_then(|m| m.total_storage_max_size_bytes)
                    .filter(|&v| v > 0)
            })
            .unwrap_or(0);
        let free = total.saturating_sub(used);
        Ok(StorageInfo { used, total, free })
    }

    fn supports_thumbnails(&self) -> bool {
        true
    }

    async fn get_thumbnail(&mut self, path: &str) -> Result<String, ProviderError> {
        let entry = self.stat(path).await?;
        entry
            .metadata
            .get("secure_url")
            .cloned()
            .ok_or_else(|| ProviderError::NotFound("No Cloudinary delivery URL".to_string()))
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
        let mut stack = vec![root.trim_matches('/').to_string()];
        let mut matches = Vec::new();
        while let Some(dir) = stack.pop() {
            let subfolders = self.list_subfolders(&dir).await.unwrap_or_default();
            for sf in &subfolders {
                let subpath = if sf.path.is_empty() {
                    if dir.is_empty() {
                        sf.name.clone()
                    } else {
                        format!("{}/{}", dir, sf.name)
                    }
                } else {
                    sf.path.clone()
                };
                stack.push(subpath);
                if super::matches_find_pattern(&sf.name, pattern) {
                    matches.push(folder_to_entry(sf, &dir));
                }
            }
            for f in self.list_files(&dir).await? {
                self.cache_resource_type(&f.public_id, &f.resource_type);
                let display = resource_display_name(&f);
                if super::matches_find_pattern(&display, pattern) {
                    matches.push(resource_to_entry(&f, &dir));
                }
            }
        }
        Ok(matches)
    }

    fn transfer_optimization_hints(&self) -> TransferOptimizationHints {
        TransferOptimizationHints {
            supports_range_download: true,
            supports_resume_download: true,
            ..TransferOptimizationHints::default()
        }
    }
}

// =========================================================================
// Helpers
// =========================================================================

fn folder_to_entry(folder: &CloudinarySubFolder, parent: &str) -> RemoteEntry {
    let path = if !folder.path.is_empty() {
        format!("/{}", folder.path.trim_matches('/'))
    } else if parent.is_empty() {
        format!("/{}", folder.name)
    } else {
        format!("/{}/{}", parent.trim_matches('/'), folder.name)
    };
    let mut entry = RemoteEntry::directory(folder.name.clone(), path);
    entry
        .metadata
        .insert("kind".to_string(), "folder".to_string());
    entry
}

fn resource_display_name(item: &CloudinaryResource) -> String {
    if let Some(ref dn) = item.display_name {
        if !dn.trim().is_empty() {
            let mut name = dn.clone();
            if let Some(ref fmt) = item.format {
                if !name.to_lowercase().ends_with(&format!(".{}", fmt.to_lowercase())) {
                    name = format!("{}.{}", name, fmt);
                }
            }
            return name;
        }
    }
    let base = basename(&item.public_id).to_string();
    if let Some(ref fmt) = item.format {
        if !base.is_empty() && !base.to_lowercase().ends_with(&format!(".{}", fmt.to_lowercase()))
        {
            return format!("{}.{}", base, fmt);
        }
    }
    base
}

fn resource_to_entry(item: &CloudinaryResource, parent: &str) -> RemoteEntry {
    let name = resource_display_name(item);
    let path = if parent.is_empty() {
        format!("/{}", item.public_id)
    } else {
        format!("/{}", item.public_id.trim_start_matches('/'))
    };

    let mut metadata = HashMap::new();
    metadata.insert("public_id".to_string(), item.public_id.clone());
    if let Some(ref aid) = item.asset_id {
        metadata.insert("asset_id".to_string(), aid.clone());
    }
    if !item.resource_type.is_empty() {
        metadata.insert("resource_type".to_string(), item.resource_type.clone());
    }
    if let Some(ref dt) = item.delivery_type {
        metadata.insert("delivery_type".to_string(), dt.clone());
    }
    if let Some(ref url) = item.secure_url {
        metadata.insert("secure_url".to_string(), url.clone());
    }
    if let Some(ref fmt) = item.format {
        metadata.insert("format".to_string(), fmt.clone());
    }
    if let Some(w) = item.width {
        metadata.insert("width".to_string(), w.to_string());
    }
    if let Some(h) = item.height {
        metadata.insert("height".to_string(), h.to_string());
    }
    if let Some(ref af) = item.asset_folder {
        metadata.insert("asset_folder".to_string(), af.clone());
    }

    let mime_type = item
        .format
        .as_ref()
        .map(|fmt| match item.resource_type.as_str() {
            "video" => format!("video/{}", fmt),
            "image" => format!("image/{}", fmt),
            _ => format!("application/{}", fmt),
        });

    RemoteEntry {
        name,
        path,
        is_dir: false,
        size: item.bytes,
        modified: item.created_at.clone(),
        permissions: None,
        owner: None,
        group: None,
        is_symlink: false,
        link_target: None,
        mime_type,
        metadata,
    }
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
    parts.join("/")
}

fn basename(path: &str) -> &str {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default()
}

fn parent_segments(path: &str) -> String {
    let trimmed = path.trim_matches('/');
    if trimmed.is_empty() {
        return String::new();
    }
    match trimmed.rfind('/') {
        Some(idx) => trimmed[..idx].to_string(),
        None => String::new(),
    }
}

fn encode_folder_segments(folder: &str) -> String {
    folder
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|s| urlencoding::encode(s).to_string())
        .collect::<Vec<_>>()
        .join("/")
}

fn validate_download_url(url: &str) -> Result<(), ProviderError> {
    let parsed = url::Url::parse(url)
        .map_err(|e| ProviderError::ServerError(format!("Invalid Cloudinary URL: {}", e)))?;
    if parsed.scheme() != "https" {
        return Err(ProviderError::ServerError(
            "Cloudinary download URL must use https".to_string(),
        ));
    }
    let host = parsed.host_str().unwrap_or_default().to_ascii_lowercase();
    if host != "res.cloudinary.com"
        && !host.ends_with(".cloudinary.com")
        && !host.ends_with(".cloudinary.net")
    {
        return Err(ProviderError::ServerError(format!(
            "Unexpected Cloudinary delivery host: {}",
            host
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_path_trims_and_strips_dots() {
        assert_eq!(normalize_path(""), "");
        assert_eq!(normalize_path("/"), "");
        assert_eq!(normalize_path("/foo/bar/"), "foo/bar");
        assert_eq!(normalize_path("foo/./bar"), "foo/bar");
        assert_eq!(normalize_path("foo/bar/../baz"), "foo/baz");
    }

    #[test]
    fn test_parent_segments() {
        assert_eq!(parent_segments(""), "");
        assert_eq!(parent_segments("foo"), "");
        assert_eq!(parent_segments("foo/bar"), "foo");
        assert_eq!(parent_segments("/foo/bar/baz/"), "foo/bar");
    }

    #[test]
    fn test_encode_folder_segments() {
        assert_eq!(encode_folder_segments("foo"), "foo");
        assert_eq!(encode_folder_segments("foo/bar baz"), "foo/bar%20baz");
        assert_eq!(encode_folder_segments("/foo/bar/"), "foo/bar");
    }

    #[test]
    fn test_validate_download_url_accepts_cloudinary_https() {
        assert!(validate_download_url("https://res.cloudinary.com/demo/image/upload/v1/sample.jpg").is_ok());
    }

    #[test]
    fn test_validate_download_url_rejects_http() {
        assert!(validate_download_url("http://res.cloudinary.com/demo/image/upload/v1/sample.jpg").is_err());
    }

    #[test]
    fn test_validate_download_url_rejects_other_hosts() {
        assert!(validate_download_url("https://example.com/sample.jpg").is_err());
    }

    #[test]
    fn test_config_rejects_invalid_cloud_name() {
        let mut extra = HashMap::new();
        extra.insert("cloud_name".to_string(), "../etc".to_string());
        let cfg = ProviderConfig {
            name: "test".to_string(),
            provider_type: ProviderType::Cloudinary,
            host: "api.cloudinary.com".to_string(),
            port: Some(443),
            username: Some("key".to_string()),
            password: Some("secret".to_string()),
            initial_path: None,
            extra,
        };
        assert!(CloudinaryConfig::from_provider_config(&cfg).is_err());
    }

    #[test]
    fn test_config_accepts_well_formed_cloud_name() {
        let mut extra = HashMap::new();
        extra.insert("cloud_name".to_string(), "dxz9abc12".to_string());
        let cfg = ProviderConfig {
            name: "test".to_string(),
            provider_type: ProviderType::Cloudinary,
            host: "api.cloudinary.com".to_string(),
            port: Some(443),
            username: Some("key".to_string()),
            password: Some("secret".to_string()),
            initial_path: None,
            extra,
        };
        let parsed = CloudinaryConfig::from_provider_config(&cfg).unwrap();
        assert_eq!(parsed.cloud_name, "dxz9abc12");
        assert_eq!(parsed.api_key, "key");
    }
}
