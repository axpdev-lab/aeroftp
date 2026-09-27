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
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Mutex;
use tokio_util::io::ReaderStream;

use super::{
    response_bytes_with_limit, sanitize_api_error, MultipartHandle, ProviderConfig, ProviderError,
    ProviderType, RemoteEntry, StorageInfo, StorageProvider, TransferOptimizationHints,
    UploadedPart, AEROFTP_USER_AGENT, MAX_DOWNLOAD_TO_BYTES,
};

/// Cloudinary chunked-upload kick-in threshold (S3-T14).
///
/// Below this size the runner keeps the legacy single-shot multipart POST.
/// At or above, it switches to the chunked path which carries the
/// `X-Unique-Upload-Id` header so Cloudinary stitches the chunks
/// server-side. Cloudinary documents 100 MB as the cutover.
const CLOUDINARY_MULTIPART_THRESHOLD: u64 = 100 * 1024 * 1024;

/// Cloudinary chunked-upload preferred part size (S3-T14).
///
/// Cloudinary's chunked upload accepts arbitrary part sizes; 20 MiB
/// amortises the multipart-form overhead while keeping per-chunk
/// memory bounded for the live test rig.
const CLOUDINARY_MULTIPART_PART_SIZE: u64 = 20 * 1024 * 1024;

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
        let cloud_name = config.extra.get("cloud_name").cloned().ok_or_else(|| {
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

/// Side-band metadata embedded in `MultipartHandle.upload_id` for the
/// Cloudinary chunked-upload path (S3-T14). `unique_upload_id` is the
/// per-upload random tag every chunk must carry in the
/// `X-Unique-Upload-Id` header so Cloudinary stitches the chunks
/// together server-side; `folder` is pre-resolved so upload_part does
/// not have to re-parse the remote_path.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct CloudinaryMultipartMeta {
    unique_upload_id: String,
    upload_url: String,
    folder: String,
    file_name: String,
    total: u64,
    part: u64,
    total_parts: u32,
}

impl CloudinaryMultipartMeta {
    fn encode(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    fn decode(raw: &str) -> Result<Self, ProviderError> {
        serde_json::from_str(raw).map_err(|e| {
            ProviderError::Other(format!("Cloudinary multipart handle decode failed: {}", e))
        })
    }
}

fn cloudinary_runner_part_size(total: u64) -> u64 {
    CLOUDINARY_MULTIPART_PART_SIZE.min(total.max(1))
}

fn cloudinary_total_parts(total: u64, part: u64) -> u32 {
    let raw = total.div_ceil(part.max(1)).max(1);
    raw.min(u32::MAX as u64) as u32
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
    #[cfg(test)]
    api_base_override: Option<String>,
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
            #[cfg(test)]
            api_base_override: None,
        }
    }

    fn api_base(&self) -> String {
        #[cfg(test)]
        if let Some(base) = &self.api_base_override {
            return base.clone();
        }
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
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        Self::classify_cloudinary_error(status, &body)
    }

    /// Pure status+body -> ProviderError classifier (extracted from `parse_error`
    /// so it can be unit-tested without a live `reqwest::Response`). Cloudinary
    /// returns a JSON `{ "error": { "message" } }`; the message (or the sanitized
    /// raw body) is the human text, and the HTTP status picks the variant.
    fn classify_cloudinary_error(status: u16, body: &str) -> ProviderError {
        let parsed = serde_json::from_str::<CloudinaryError>(body).ok();
        let msg = parsed
            .and_then(|e| e.error.and_then(|b| b.message))
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| sanitize_api_error(body));

        // Real responses always carry a valid HTTP status; the defensive fallback
        // never fires in practice but keeps the fn total.
        let status = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        match status {
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                ProviderError::AuthenticationFailed(msg)
            }
            StatusCode::NOT_FOUND => ProviderError::NotFound(msg),
            StatusCode::CONFLICT => ProviderError::AlreadyExists(msg),
            // A rename onto a public id taken since the destination check is
            // a 400 whose message says so: a taken name, not a bad config.
            s if s.is_client_error() && msg.to_ascii_lowercase().contains("already exist") => {
                ProviderError::AlreadyExists(msg)
            }
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

    /// Move and/or rename a file on a dynamic-folder account. There the
    /// folder is the asset's `asset_folder` and the name it shows is its
    /// `display_name`, both independent of the public id: renaming the
    /// public id, which is what a fixed-folder account needs, answered Ok and
    /// left the asset in its folder under its old name. `PUT
    /// /resources/{asset_id}` sets both (Admin API, "update details of an
    /// existing resource by asset ID"); the public id, and with it every
    /// delivery URL, stays as it was.
    async fn update_asset_place(
        &self,
        entry: &RemoteEntry,
        source: &str,
        target: &str,
    ) -> Result<(), ProviderError> {
        let asset_id = entry.metadata.get("asset_id").ok_or_else(|| {
            ProviderError::Other(format!(
                "Cannot move {source}: Cloudinary listed it without an asset id"
            ))
        })?;
        let mut form: Vec<(&str, String)> = Vec::new();
        let to_folder = parent_segments(target);
        if parent_segments(source) != to_folder {
            form.push(("asset_folder", to_folder));
        }
        let to_name = basename(target);
        if basename(source) != to_name {
            // The listing appends `.{format}` to a display name without it.
            let display_name = match entry.metadata.get("format") {
                Some(format) if !format.is_empty() => to_name
                    .strip_suffix(&format!(".{format}"))
                    .unwrap_or(to_name),
                _ => to_name,
            };
            form.push(("display_name", display_name.to_string()));
        }
        if form.is_empty() {
            return Ok(());
        }
        let url = format!(
            "{}/resources/{}",
            self.api_base(),
            urlencoding::encode(asset_id)
        );
        let body = {
            let mut serializer = url::form_urlencoded::Serializer::new(String::new());
            for (key, value) in &form {
                serializer.append_pair(key, value);
            }
            serializer.finish()
        };
        let resp = self
            .auth(self.client.put(&url))
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(body)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(self.parse_error(resp).await)
        }
    }

    /// Rename or replace. With `overwrite` false an occupied destination is
    /// refused before anything changes (the `rename` contract). With it true
    /// (the `replace` contract) a fixed-folder account renames the public id
    /// with `overwrite=true`, one step on the server; a dynamic-folder
    /// account, where two assets may share a display name in one folder,
    /// moves the asset in and only then deletes the one it displaced, so the
    /// destination is never empty.
    async fn move_asset(
        &mut self,
        from: &str,
        to: &str,
        overwrite: bool,
    ) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let source = self.resolve_path(from);
        let target = self.resolve_path(to);
        if source == target {
            return Ok(());
        }
        let entry = self.stat(&source).await?;
        // The trait promises no overwrite: refuse an occupied destination
        // before either branch runs.
        let displaced = match self.stat(&target).await {
            Ok(_) if !overwrite => return Err(ProviderError::AlreadyExists(to.to_string())),
            Ok(occupant) => Some(occupant),
            Err(ProviderError::NotFound(_)) => None,
            Err(e) => return Err(e),
        };
        // A replace puts a file in place of a file and a folder in place of
        // a folder: a file onto a folder took the public id of an image
        // named like the folder and overwrote it.
        if let Some(occupant) = &displaced {
            super::refuse_replace_across_types(to, entry.is_dir, occupant.is_dir)?;
        }
        let dynamic_folders = self.dynamic_folder_mode.lock().ok().and_then(|m| *m) == Some(true);
        if !entry.is_dir && dynamic_folders {
            self.update_asset_place(&entry, &source, &target).await?;
            return match displaced {
                Some(occupant)
                    if !occupant.is_dir
                        && occupant.metadata.get("asset_id") != entry.metadata.get("asset_id") =>
                {
                    self.delete_displaced_asset(&occupant, to).await
                }
                _ => Ok(()),
            };
        }
        if entry.is_dir {
            // The folder endpoint answers 409 for an existing destination,
            // and a folder has no single item to set aside: refuse before
            // the request, with the reason.
            if displaced.is_some() {
                return Err(ProviderError::NotSupported(format!(
                    "cannot replace the folder {to} with {from}: Cloudinary moves a folder only \
                     to a free name, and nothing was changed"
                )));
            }
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
            // Public ids are unique per resource type, and a rename moves an
            // asset within its own. An image replaced onto the video `v`
            // (`v.mp4`) took the public id `v` among the images with
            // overwrite=true: it overwrote an image `v` (`v.png`) nobody
            // named, or with none there replaced nothing, and the video
            // stayed. A replace onto an asset of another type is refused.
            if let Some(occupant) = displaced.as_ref().filter(|occupant| !occupant.is_dir) {
                let occupant_kind = occupant
                    .metadata
                    .get("resource_type")
                    .map_or("image", String::as_str);
                if occupant_kind != kind {
                    return Err(ProviderError::AlreadyExists(format!(
                        "{to} is a Cloudinary {occupant_kind} asset and {from} a {kind} one: \
                         public ids are kept per resource type, so a rename cannot replace it"
                    )));
                }
            }
            let from_pid = entry
                .metadata
                .get("public_id")
                .cloned()
                .unwrap_or_else(|| source.trim_matches('/').to_string());
            // The public id of an image or a video carries no extension (the
            // listing adds `.{format}`), so the target name is not one. Onto
            // an occupant its own public id is the one to take: with the
            // extension a second asset appeared, listed under the same name,
            // and the old one stayed. Onto a free name the format comes off.
            let to_pid = match &displaced {
                Some(occupant)
                    if !occupant.is_dir && occupant.metadata.contains_key("public_id") =>
                {
                    occupant.metadata["public_id"].clone()
                }
                _ => {
                    let named = target.trim_matches('/');
                    let stem = match (kind.as_str(), entry.metadata.get("format")) {
                        ("image" | "video", Some(format)) if !format.is_empty() => {
                            named.strip_suffix(&format!(".{format}")).unwrap_or(named)
                        }
                        _ => named,
                    };
                    // `a.jpg` (public id `a.jpg`) renamed to `a.jpg.jpg`: the
                    // stem is the source's own id, so the name keeps its
                    // format.
                    if stem == from_pid {
                        named.to_string()
                    } else {
                        stem.to_string()
                    }
                }
            };
            // A free name whose public id (the name without its format) is
            // held by another asset, one of another format listed under
            // another name (`report.pdf` for `report.jpg`): the rename would
            // take its public id, and a replace would overwrite it. It is
            // refused, naming that asset.
            if displaced.is_none() {
                let parent = parent_segments(&target);
                let dynamic = self.dynamic_folders();
                // Public ids are unique per resource type: an image `b` and a
                // video `b` coexist. The source itself is no holder.
                if let Some(holder) = self.list_files(&parent).await?.into_iter().find(|f| {
                    f.public_id == to_pid
                        && f.public_id != from_pid
                        && self.primary_resource_type(f) == kind
                }) {
                    return Err(ProviderError::AlreadyExists(format!(
                        "{to}: its public id `{to_pid}` belongs to {}, a different asset",
                        resource_name(&holder, dynamic)
                    )));
                }
            }
            // Without `overwrite` (default false) Cloudinary refuses a target
            // public id that is already taken (rename reference). A replace
            // asks for the overwrite, and only onto the asset it found there.
            let mut url = format!(
                "{}/{}/rename?from_public_id={}&to_public_id={}",
                self.api_base(),
                kind,
                urlencoding::encode(&from_pid),
                urlencoding::encode(&to_pid)
            );
            let replaces_the_occupant = displaced.as_ref().is_some_and(|occupant| {
                !occupant.is_dir
                    && occupant
                        .metadata
                        .get("resource_type")
                        .map_or("image", String::as_str)
                        == kind
                    && occupant.metadata.get("public_id").map(String::as_str)
                        == Some(to_pid.as_str())
            });
            if overwrite && replaces_the_occupant {
                url.push_str("&overwrite=true");
            }
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

    /// Delete the asset a dynamic-folder replace displaced. It now shares its
    /// folder and display name with the asset that took its place, so a
    /// failure here is an error that says so, never a success.
    async fn delete_displaced_asset(
        &self,
        occupant: &RemoteEntry,
        to: &str,
    ) -> Result<(), ProviderError> {
        let public_id = occupant
            .metadata
            .get("public_id")
            .cloned()
            .unwrap_or_default();
        let kind = occupant
            .metadata
            .get("resource_type")
            .cloned()
            .unwrap_or_else(|| "image".to_string());
        let outcome = if public_id.is_empty() {
            Err(ProviderError::Other(
                "it was listed without a public id".to_string(),
            ))
        } else {
            let delivery = occupant
                .metadata
                .get("delivery_type")
                .map_or("upload", String::as_str);
            match self.delete_resource(&public_id, &kind, delivery).await {
                Ok(true) => Ok(()),
                Ok(false) => Err(ProviderError::Other(
                    "Cloudinary did not delete it".to_string(),
                )),
                Err(e) => Err(e),
            }
        };
        outcome.map_err(|e| {
            ProviderError::Other(format!(
                "moved the file to {to}, but the asset it replaced ({public_id}) is still there \
                 under the same name: {e}"
            ))
        })
    }

    /// Whether the account names assets by `asset_folder` and
    /// `display_name` (dynamic folders), as the last listing found.
    fn dynamic_folders(&self) -> bool {
        self.dynamic_folder_mode.lock().ok().and_then(|m| *m) == Some(true)
    }

    fn primary_resource_type(&self, item: &CloudinaryResource) -> String {
        if !item.resource_type.is_empty() {
            item.resource_type.clone()
        } else {
            "image".to_string()
        }
    }

    /// Delete one asset, named by public id, resource type and delivery type
    /// (`upload`, `private`, `authenticated`): public ids are unique only
    /// within the pair of types, so an upload `v` and a private `v` are two
    /// assets.
    async fn delete_resource(
        &self,
        public_id: &str,
        resource_type: &str,
        delivery_type: &str,
    ) -> Result<bool, ProviderError> {
        let url = format!(
            "{}/resources/{}/{}?public_ids[]={}",
            self.api_base(),
            resource_type,
            urlencoding::encode(delivery_type),
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
            if self.delete_resource(public_id, &kind, "upload").await? {
                return Ok(());
            }
        }
        for kind in ["image", "video", "raw"] {
            match self.delete_resource(public_id, kind, "upload").await {
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

        let subfolders = self.list_subfolders(&folder_norm).await?;
        let files = self.list_files(&folder_norm).await?;

        // Cache resource_types from this listing for subsequent deletes.
        for f in &files {
            self.cache_resource_type(&f.public_id, &f.resource_type);
        }

        let mut entries: Vec<RemoteEntry> = subfolders
            .into_iter()
            .map(|sf| folder_to_entry(&sf, &folder_norm))
            .collect();
        let dynamic = self.dynamic_folders();
        let unique = names_listed_once(&files, dynamic);
        entries.extend(files.iter().map(|f| {
            let name_is_unique = unique.contains(&resource_name(f, dynamic));
            resource_to_entry(f, &folder_norm, dynamic, name_is_unique)
        }));
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
        let url = entry.metadata.get("secure_url").cloned().ok_or_else(|| {
            ProviderError::NotFound("Cloudinary delivery URL missing".to_string())
        })?;
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
        let mut stream = Box::pin(crate::transfer_dag::throttle::throttle_stream(
            resp.bytes_stream(),
            crate::transfer_dag::governor::TransferDirection::Download,
        ));
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
        let url = entry.metadata.get("secure_url").cloned().ok_or_else(|| {
            ProviderError::NotFound("Cloudinary delivery URL missing".to_string())
        })?;
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

    /// Ranged read for remote archive/encryption surfacing (header/tail windows).
    /// Cloudinary's delivery CDN serves byte ranges over a plain HTTP `Range`
    /// header; if a range is ever ignored (200 OK), slice the full body locally.
    async fn read_range(
        &mut self,
        remote_path: &str,
        offset: u64,
        len: u64,
    ) -> Result<Vec<u8>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        if len == 0 {
            return Ok(Vec::new());
        }
        let entry = self.stat(remote_path).await?;
        if entry.is_dir {
            return Err(ProviderError::InvalidPath(
                "Cannot read a range from a directory".to_string(),
            ));
        }
        let url = entry.metadata.get("secure_url").cloned().ok_or_else(|| {
            ProviderError::NotFound("Cloudinary delivery URL missing".to_string())
        })?;
        validate_download_url(&url)?;
        // offset + len - 1 must not wrap: a wrapped end makes an invalid Range
        // header that servers ignore, turning a bounded probe into a full-body
        // download. (len == 0 already returned above.)
        let end = offset
            .checked_add(len - 1)
            .ok_or_else(|| ProviderError::Other("read_range end overflows u64".to_string()))?;
        let resp = self
            .client
            .get(&url)
            .header("Range", format!("bytes={}-{}", offset, end))
            .send()
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(ProviderError::TransferFailed(format!(
                "Range download failed: HTTP {}",
                status
            )));
        }
        let answered = resp
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.to_string());
        let bytes = response_bytes_with_limit(resp, MAX_DOWNLOAD_TO_BYTES).await?;
        // A 206 that does not name the window it carries is written at this
        // offset just the same, so it has to say which range it is.
        match super::multi_thread::ranged_answer(
            status,
            answered.as_deref(),
            bytes.len() as u64,
            offset,
            end,
        ) {
            Ok(super::multi_thread::RangedAnswer::Window) => Ok(bytes),
            Ok(super::multi_thread::RangedAnswer::WholeObject) => {
                Ok(super::multi_thread::slice_whole_object(&bytes, offset, len))
            }
            Err(why) => Err(ProviderError::ParallelRefused(
                super::multi_thread::parallel_refused("Cloudinary range read", remote_path, &why),
            )),
        }
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
        let stream = crate::transfer_dag::throttle::throttle_stream(
            ReaderStream::with_capacity(file, 64 * 1024),
            crate::transfer_dag::governor::TransferDirection::Upload,
        )
        .map(move |chunk| {
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
            let public_id = entry.metadata.get("public_id").cloned().ok_or_else(|| {
                ProviderError::NotFound("Missing Cloudinary public_id".to_string())
            })?;
            // The type of the asset the path names. An image `v` and a video
            // `v` share the public id, and the cache keyed by public id holds
            // the type listed last: `rm /v.png` deleted the video.
            let delivery = entry
                .metadata
                .get("delivery_type")
                .map_or("upload", String::as_str);
            match entry.metadata.get("resource_type") {
                Some(kind) => {
                    if self.delete_resource(&public_id, kind, delivery).await? {
                        Ok(())
                    } else {
                        Err(ProviderError::NotFound(resolved))
                    }
                }
                None => self.delete_file_with_fallback(&public_id).await,
            }
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
            let subfolders = self.list_subfolders(&dir).await?;
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
                let delivery = f.delivery_type.as_deref().unwrap_or("upload");
                let _ = self.delete_resource(&f.public_id, &kind, delivery).await?;
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
        self.move_asset(from, to, false).await
    }

    /// A rename with `overwrite=true` (fixed folders), or a move that then
    /// deletes the asset it displaced (dynamic folders): see `move_asset`.
    async fn replace(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        self.move_asset(from, to, true).await
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

        // `stat` is the inverse of `list`: a path names first the item `list`
        // gives that path, folders and assets alike. Public ids are unique
        // per resource type, so several items can hold one listed path (an
        // image `v` and a video `v`, a folder `photos` and an image
        // `photos`): that path is refused as ambiguous. It looked at the
        // folders first and then took the first asset matching by name or by
        // public id, so an action on one item reached another (read from the
        // code for a fixed-folder account: `rm` of a video deleted the image
        // beside it).
        let parent = parent_segments(&resolved);
        let name = basename(&resolved).to_string();
        let wanted = format!("/{trimmed}");
        let folders = self.list_subfolders(&parent).await?;
        let files = self.list_files(&parent).await?;
        for f in &files {
            self.cache_resource_type(&f.public_id, &f.resource_type);
        }
        let dynamic = self.dynamic_folders();
        let unique = names_listed_once(&files, dynamic);
        // A listing that returns the same asset twice lists it once.
        let mut assets: Vec<CloudinaryResource> = Vec::new();
        for f in files {
            if !assets.iter().any(|seen| same_asset(seen, &f)) {
                assets.push(f);
            }
        }
        let entry_of = |f: &CloudinaryResource| {
            let name_is_unique = unique.contains(&resource_name(f, dynamic));
            resource_to_entry(f, &parent, dynamic, name_is_unique)
        };
        let mut listed: Vec<RemoteEntry> = folders
            .iter()
            .filter(|f| f.name == name)
            .map(|f| folder_to_entry(f, &parent))
            .collect();
        listed.extend(assets.iter().map(&entry_of).filter(|e| e.path == wanted));
        match listed.len() {
            0 => {}
            1 => return Ok(listed.remove(0)),
            several => return Err(ambiguous_path(&wanted, several)),
        }
        // Not a listed path. A fixed-folder account still resolves a path
        // spelled as a public id, when exactly one asset has it. The name
        // `list` shows never needs this: a name one asset shows is its
        // listed path. Matching the display name on a fixed-folder account,
        // or the public id on a dynamic-folder one, is matching what a
        // rename leaves unchanged: the old name kept resolving to the renamed
        // asset (found live on 2026-09-26).
        if !dynamic {
            let by_id: Vec<&CloudinaryResource> =
                assets.iter().filter(|f| f.public_id == trimmed).collect();
            match by_id.as_slice() {
                [] => {}
                [only] => return Ok(entry_of(only)),
                several => return Err(ambiguous_path(&wanted, several.len())),
            }
        }
        Err(ProviderError::NotFound(wanted))
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
        let used = usage.storage.as_ref().and_then(|m| m.usage).unwrap_or(0);
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
        Ok(StorageInfo {
            used,
            total,
            free,
            versioning_bytes: None,
        })
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

    async fn find(&mut self, path: &str, pattern: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let root = self.resolve_path(path);
        let mut stack = vec![root.trim_matches('/').to_string()];
        let mut matches = Vec::new();
        while let Some(dir) = stack.pop() {
            let subfolders = self.list_subfolders(&dir).await?;
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
            let files = self.list_files(&dir).await?;
            let dynamic = self.dynamic_folders();
            let unique = names_listed_once(&files, dynamic);
            for f in files {
                self.cache_resource_type(&f.public_id, &f.resource_type);
                let name = resource_name(&f, dynamic);
                if super::matches_find_pattern(&name, pattern) {
                    let name_is_unique = unique.contains(&name);
                    matches.push(resource_to_entry(&f, &dir, dynamic, name_is_unique));
                }
            }
        }
        Ok(matches)
    }

    fn transfer_optimization_hints(&self) -> TransferOptimizationHints {
        TransferOptimizationHints {
            supports_multipart: true,
            multipart_threshold: CLOUDINARY_MULTIPART_THRESHOLD,
            multipart_part_size: CLOUDINARY_MULTIPART_PART_SIZE,
            // Cloudinary's chunked upload demands a monotonic
            // `Content-Range`; chunks are not idempotent across the
            // same `X-Unique-Upload-Id`. Strict sequential dispatch.
            multipart_max_parallel: 1,
            supports_range_download: true,
            supports_resume_download: true,
            supports_resume_upload: true,
            ..TransferOptimizationHints::default()
        }
    }

    // Shaped-graph multipart trait wiring (S3-T14).
    //
    // Cloudinary's chunked upload maps onto the trait as:
    //   1. `begin_multipart_upload` → no server round-trip: generate a
    //      per-upload `X-Unique-Upload-Id` (UUID v4) and stash it
    //      together with the resolved folder, target file name,
    //      total/part-size pair, and the precomputed upload URL.
    //   2. `upload_part` → POST `<upload_url>` with multipart form
    //      `{file:chunk_bytes, ...auth, asset_folder, folder,
    //      use_filename, unique_filename}` plus headers
    //      `X-Unique-Upload-Id: <id>` and `Content-Range: bytes A-B/T`.
    //      Cloudinary returns the final `CloudinaryUploadResponse` on
    //      the closing chunk; intermediate chunks return `done: false`.
    //   3. `complete_multipart_upload` → validate part count. Cloudinary
    //      finalises implicitly when the last chunk's Content-Range
    //      covers `total - 1`; no separate commit call exists.
    //   4. `abort_multipart_upload` → no-op. Cloudinary GCs incomplete
    //      uploads after their TTL.
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
                "Cloudinary multipart upload requires non-zero total_size".to_string(),
            ));
        }

        let target = self.resolve_path(remote_path);
        let file_name = if target.is_empty() || target.ends_with('/') {
            return Err(ProviderError::InvalidPath(
                "Cloudinary multipart upload requires a filename".to_string(),
            ));
        } else {
            basename(&target).to_string()
        };
        let folder = parent_segments(&target);
        let part = cloudinary_runner_part_size(total_size);
        let total_parts = cloudinary_total_parts(total_size, part);

        let meta = CloudinaryMultipartMeta {
            unique_upload_id: uuid::Uuid::new_v4().simple().to_string(),
            upload_url: format!("{}/auto/upload", self.api_base()),
            folder,
            file_name,
            total: total_size,
            part,
            total_parts,
        };
        Ok(MultipartHandle {
            upload_id: meta.encode(),
            remote_path: remote_path.to_string(),
        })
    }

    // DAG-P2-05: Cloudinary chunked upload is a single multipart POST with a
    // known Content-Range and no whole-part hashing, so stream the part body one
    // bounded window at a time instead of buffering the whole part in memory.
    fn multipart_streams_part_body(&self) -> bool {
        true
    }

    async fn upload_part(
        &mut self,
        handle: &MultipartHandle,
        part_number: u32,
        data: Vec<u8>,
    ) -> Result<UploadedPart, ProviderError> {
        self.upload_part_body(
            handle,
            part_number,
            crate::transfer_multipart::PartBody::owned(data),
        )
        .await
    }

    async fn upload_part_body(
        &mut self,
        handle: &MultipartHandle,
        part_number: u32,
        body: crate::transfer_multipart::PartBody,
    ) -> Result<UploadedPart, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        if part_number == 0 {
            return Err(ProviderError::Other(
                "Cloudinary upload_part requires 1-based part_number".to_string(),
            ));
        }
        let part_len = body.len();
        if part_len == 0 {
            return Err(ProviderError::Other(
                "Cloudinary upload_part received empty data".to_string(),
            ));
        }
        let meta = CloudinaryMultipartMeta::decode(&handle.upload_id)?;
        if part_number > meta.total_parts {
            return Err(ProviderError::Other(format!(
                "Cloudinary part {} exceeds declared total_parts {}",
                part_number, meta.total_parts
            )));
        }
        let offset = (part_number as u64 - 1) * meta.part;
        let end = offset
            .checked_add(part_len)
            .ok_or_else(|| ProviderError::Other("Cloudinary part offset overflow".to_string()))?;
        if end > meta.total {
            return Err(ProviderError::Other(format!(
                "Cloudinary part {} exceeds declared total: offset {} + len {} > total {}",
                part_number, offset, part_len, meta.total
            )));
        }
        let content_range = format!("bytes {}-{}/{}", offset, end - 1, meta.total);

        let mime = mime_guess::from_path(&meta.file_name).first_or_octet_stream();
        let file_part = multipart::Part::stream_with_length(body.into_reqwest_body(), part_len)
            .file_name(meta.file_name.clone())
            .mime_str(mime.as_ref())
            .map_err(|e| ProviderError::InvalidConfig(e.to_string()))?;
        let mut form = multipart::Form::new()
            .part("file", file_part)
            .text("use_filename", "true")
            .text("unique_filename", "false");
        if !meta.folder.is_empty() {
            form = form
                .text("asset_folder", meta.folder.clone())
                .text("folder", meta.folder.clone());
        }

        let resp = self
            .auth(self.client.post(&meta.upload_url))
            .header("X-Unique-Upload-Id", &meta.unique_upload_id)
            .header("Content-Range", content_range)
            .multipart(form)
            .send()
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(self.parse_error(resp).await);
        }
        // Intermediate chunks return `{"done": false}`, the final chunk
        // returns the full CloudinaryUploadResponse. We don't care which
        // shape it is at the trait level - the runner uses the count
        // check in `complete_multipart_upload` to detect truncation.
        Ok(UploadedPart {
            part_number,
            etag: meta.unique_upload_id,
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
        let meta = CloudinaryMultipartMeta::decode(&handle.upload_id)?;
        if parts.len() != meta.total_parts as usize {
            return Err(ProviderError::TransferFailed(format!(
                "Cloudinary complete: expected {} parts, runner committed {}",
                meta.total_parts,
                parts.len()
            )));
        }
        // Cloudinary finalises implicitly on the closing chunk; nothing
        // else is required.
        Ok(())
    }

    async fn abort_multipart_upload(
        &mut self,
        _handle: MultipartHandle,
    ) -> Result<(), ProviderError> {
        // Cloudinary has no documented abort endpoint for incomplete
        // chunked uploads; orphans are GCed automatically. Returning
        // Ok keeps the abort from masking the original transfer error.
        Ok(())
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

/// The display name of an asset with its format, or the last segment of
/// its public id when it has none: the name on a dynamic-folder account.
fn resource_display_name(item: &CloudinaryResource) -> String {
    if let Some(ref dn) = item.display_name {
        if !dn.trim().is_empty() {
            let mut name = dn.clone();
            if let Some(ref fmt) = item.format {
                if !name
                    .to_lowercase()
                    .ends_with(&format!(".{}", fmt.to_lowercase()))
                {
                    name = format!("{}.{}", name, fmt);
                }
            }
            return name;
        }
    }
    let base = basename(&item.public_id).to_string();
    if let Some(ref fmt) = item.format {
        if !base.is_empty()
            && !base
                .to_lowercase()
                .ends_with(&format!(".{}", fmt.to_lowercase()))
        {
            return format!("{}.{}", base, fmt);
        }
    }
    base
}

/// The name an asset goes by. On a dynamic-folder account that is its
/// display name, which a rename changes and the public id does not; on a
/// fixed-folder account it is the last segment of its public id, which a
/// rename changes, while `display_name` keeps the original filename.
fn resource_name(item: &CloudinaryResource, dynamic: bool) -> String {
    if dynamic {
        return resource_display_name(item);
    }
    let base = basename(&item.public_id).to_string();
    match item.format.as_deref() {
        Some(format)
            if !base.is_empty()
                && !base
                    .to_lowercase()
                    .ends_with(&format!(".{}", format.to_lowercase())) =>
        {
            format!("{base}.{format}")
        }
        _ => base,
    }
}

/// `item` as an entry of the folder `parent`: its path is the folder and the
/// name `list` shows, the name `stat` resolves first. On a fixed-folder
/// account it was the public id, which two assets of different types share
/// (the image `b`, listed `b.png`, and a raw `b`): an action on the image
/// through its listed path found the raw asset. A name the folder shows more
/// than once (public ids `a` and `a.jpg`, both listed `a.jpg`) would give two
/// assets one path, so on a fixed-folder account such an entry keeps its
/// public id as its path (`name_is_unique` false).
fn resource_to_entry(
    item: &CloudinaryResource,
    parent: &str,
    dynamic: bool,
    name_is_unique: bool,
) -> RemoteEntry {
    let name = resource_name(item, dynamic);
    let folder = parent.trim_matches('/');
    let path = if !dynamic && !name_is_unique {
        format!("/{}", item.public_id.trim_start_matches('/'))
    } else if folder.is_empty() {
        format!("/{name}")
    } else {
        format!("/{folder}/{name}")
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

/// A path several items hold (see [`CloudinaryProvider::stat`]).
fn ambiguous_path(path: &str, items: usize) -> ProviderError {
    ProviderError::InvalidPath(format!(
        "{path} names {items} Cloudinary items (an asset and a folder, assets of two \
         resource types, or assets with the same display name): the one meant cannot be told \
         apart and nothing was done. Rename or delete one of them in the Cloudinary Media \
         Library"
    ))
}

/// The names `files` shows for one asset each (see [`resource_to_entry`]).
fn names_listed_once(files: &[CloudinaryResource], dynamic: bool) -> HashSet<String> {
    let mut assets: HashMap<String, Vec<&CloudinaryResource>> = HashMap::new();
    for f in files {
        let named = assets.entry(resource_name(f, dynamic)).or_default();
        if !named.iter().any(|seen| same_asset(seen, f)) {
            named.push(f);
        }
    }
    assets
        .into_iter()
        .filter(|(_, named)| named.len() == 1)
        .map(|(name, _)| name)
        .collect()
}

/// Whether two listed entries are one asset: by `asset_id` when both carry
/// one, otherwise by public id, resource type and delivery type (public ids
/// are unique per resource type and delivery type, so an `upload` and a
/// `private` asset can share one; a missing type is Cloudinary's `upload`).
fn same_asset(a: &CloudinaryResource, b: &CloudinaryResource) -> bool {
    if let (Some(x), Some(y)) = (&a.asset_id, &b.asset_id) {
        return x == y;
    }
    let delivery = |r: &CloudinaryResource| {
        r.delivery_type
            .clone()
            .unwrap_or_else(|| "upload".to_string())
    };
    a.public_id == b.public_id && a.resource_type == b.resource_type && delivery(a) == delivery(b)
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

    /// The dedupe took an `upload` and a `private` asset with one public id
    /// for one asset and dropped the second before the match.
    #[test]
    fn same_asset_tells_delivery_types_apart() {
        let asset = |id: Option<&str>, delivery: Option<&str>| {
            serde_json::from_value::<CloudinaryResource>(serde_json::json!({
                "asset_id": id,
                "public_id": "v",
                "resource_type": "image",
                "type": delivery,
            }))
            .expect("resource")
        };
        assert!(!same_asset(
            &asset(None, Some("upload")),
            &asset(None, Some("private"))
        ));
        assert!(same_asset(&asset(None, None), &asset(None, Some("upload"))));
        assert!(!same_asset(
            &asset(Some("a1"), Some("upload")),
            &asset(Some("a2"), Some("upload"))
        ));
        assert!(same_asset(
            &asset(Some("a1"), None),
            &asset(Some("a1"), Some("upload"))
        ));
    }

    /// A Cloudinary double whose root holds `resource` and nothing else, on
    /// a dynamic-folder account (listing by `asset_folder`) or a fixed-folder
    /// one (`by_asset_folder` refused, listing by prefix).
    async fn provider_listing(resource: serde_json::Value, dynamic: bool) -> CloudinaryProvider {
        provider_listing_all(serde_json::json!([resource]), dynamic).await
    }

    /// [`provider_listing`] with every asset of `resources` (a JSON array)
    /// at the root.
    async fn provider_listing_all(
        resources: serde_json::Value,
        dynamic: bool,
    ) -> CloudinaryProvider {
        let listing = serde_json::json!({ "resources": resources }).to_string();
        let app = axum::Router::new()
            .route(
                "/resources/by_asset_folder",
                axum::routing::get({
                    let listing = listing.clone();
                    move || {
                        let listing = listing.clone();
                        async move {
                            if dynamic {
                                (axum::http::StatusCode::OK, listing)
                            } else {
                                (
                                    axum::http::StatusCode::BAD_REQUEST,
                                    r#"{"error":{"message":"Unknown parameter asset_folder"}}"#
                                        .to_string(),
                                )
                            }
                        }
                    }
                }),
            )
            .route(
                // The prefix listing of a fixed-folder account, per type.
                "/resources/image",
                axum::routing::get(move || {
                    let listing = listing.clone();
                    async move { listing }
                }),
            )
            .route(
                "/resources/{kind}",
                axum::routing::get(|| async { r#"{"resources":[]}"# }),
            )
            .route(
                "/folders",
                axum::routing::get(|| async { r#"{"folders":[]}"# }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.ok() });
        let mut provider = CloudinaryProvider::new(CloudinaryConfig {
            cloud_name: "test".to_string(),
            api_key: "test".to_string(),
            api_secret: SecretString::from("test".to_string()),
            initial_path: None,
        });
        provider.connected = true;
        provider.api_base_override = Some(format!("http://{addr}"));
        provider
    }

    /// Resolve `path` with `stat`; `Some(name)` when it resolves.
    async fn resolved_name(provider: &mut CloudinaryProvider, path: &str) -> Option<String> {
        match provider.stat(path).await {
            Ok(entry) => Some(entry.name),
            Err(ProviderError::NotFound(_)) => None,
            Err(e) => panic!("{path}: {e}"),
        }
    }

    /// An asset named `a` and renamed to `c`: on a fixed-folder account the
    /// rename changes the public id and leaves `display_name` at the original
    /// filename. `stat` matched that display name too, so the old name still
    /// resolved to the renamed asset (found live on 2026-09-26: after
    /// `mv /d/a.txt /d/c.txt`, `cat /d/a.txt` answered its content) and a new
    /// file could not take the name. A path resolves only to the name `ls`
    /// shows, which on these accounts comes from the public id.
    #[tokio::test]
    async fn fixed_folders_name_an_asset_by_its_public_id_only() {
        let renamed = serde_json::json!({
            "asset_id": "AID", "public_id": "c", "display_name": "a", "format": "jpg",
            "bytes": 3, "resource_type": "image", "type": "upload",
        });
        let mut provider = provider_listing(renamed, false).await;
        let listed: Vec<RemoteEntry> = provider.list("/").await.expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "c.jpg");
        assert_eq!(resolved_name(&mut provider, "/a.jpg").await, None);
        assert_eq!(
            resolved_name(&mut provider, "/c.jpg").await.as_deref(),
            Some("c.jpg")
        );
        let path = listed[0].path.clone();
        assert_eq!(
            resolved_name(&mut provider, &path).await.as_deref(),
            Some("c.jpg")
        );
    }

    /// On a dynamic-folder account a rename changes `display_name` and keeps
    /// the public id, which `stat` also matched: the old name resolved to the
    /// renamed asset there too. The listed path is the one `stat` resolves.
    #[tokio::test]
    async fn dynamic_folders_name_an_asset_by_its_display_name_only() {
        let renamed = serde_json::json!({
            "asset_id": "AID", "public_id": "a", "display_name": "c", "format": "jpg",
            "bytes": 3, "resource_type": "image", "type": "upload", "asset_folder": "",
        });
        let mut provider = provider_listing(renamed, true).await;
        let listed: Vec<RemoteEntry> = provider.list("/").await.expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "c.jpg");
        assert_eq!(resolved_name(&mut provider, "/a.jpg").await, None);
        assert_eq!(resolved_name(&mut provider, "/a").await, None);
        assert_eq!(
            resolved_name(&mut provider, "/c.jpg").await.as_deref(),
            Some("c.jpg")
        );
        let path = listed[0].path.clone();
        assert_eq!(
            resolved_name(&mut provider, &path).await.as_deref(),
            Some("c.jpg")
        );
    }

    /// What a Cloudinary double received: rename queries and
    /// `PUT /resources/{asset_id}` bodies.
    type CloudinaryCalls = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

    /// A Cloudinary double holding the image `a` (asset `AID`) at the root
    /// and, when `occupied`, the image `b` too. On a fixed-folder account
    /// (`dynamic` false) `by_asset_folder` answers 400 as a legacy account
    /// does and the provider lists by prefix; on a dynamic-folder account it
    /// lists by `asset_folder`. Returns the provider, the rename queries and
    /// the asset updates (`PUT` bodies, and `DELETE <query>` for a delete).
    async fn provider_for_file_rename(
        occupied: bool,
        dynamic: bool,
    ) -> (CloudinaryProvider, CloudinaryCalls, CloudinaryCalls) {
        provider_for_file_rename_with(occupied, dynamic, "jpg").await
    }

    /// [`provider_for_file_rename`] whose image `b` has the format `b_format`.
    async fn provider_for_file_rename_with(
        occupied: bool,
        dynamic: bool,
        b_format: &'static str,
    ) -> (CloudinaryProvider, CloudinaryCalls, CloudinaryCalls) {
        use std::sync::Arc;
        let renames: CloudinaryCalls = Arc::default();
        let updates: CloudinaryCalls = Arc::default();
        let (seen_renames, seen_updates) = (Arc::clone(&renames), Arc::clone(&updates));
        let image = |id: &str, format: &str| {
            serde_json::json!({
                "asset_id": format!("AID_{id}"), "public_id": id, "display_name": id,
                "format": format, "bytes": 3, "resource_type": "image", "type": "upload",
                "asset_folder": "",
            })
        };
        let mut at_root = vec![image("a", "jpg")];
        if occupied {
            at_root.push(image("b", b_format));
        }
        let root_listing = serde_json::json!({ "resources": at_root }).to_string();
        let app = axum::Router::new()
            .route(
                "/image/rename",
                axum::routing::post(move |uri: axum::http::Uri| {
                    seen_renames
                        .lock()
                        .unwrap()
                        .push(uri.query().unwrap_or("").to_string());
                    async { r#"{"public_id":"b"}"# }
                }),
            )
            .route(
                // GET is the prefix listing of a fixed-folder account
                // (`/resources/image`), PUT the update of one asset.
                "/resources/{asset_id}",
                axum::routing::put(move |body: String| {
                    seen_updates.lock().unwrap().push(body);
                    async { r#"{"asset_id":"AID_a"}"# }
                })
                .get({
                    let listing = root_listing.clone();
                    move || {
                        let listing = listing.clone();
                        async move { listing }
                    }
                }),
            )
            .route(
                "/resources/{kind}/upload",
                axum::routing::delete({
                    let seen_deletes = Arc::clone(&updates);
                    move |uri: axum::http::Uri| {
                        seen_deletes
                            .lock()
                            .unwrap()
                            .push(format!("DELETE {}", uri.query().unwrap_or("")));
                        async { r#"{"deleted":{"b":"deleted"}}"# }
                    }
                }),
            )
            .route(
                "/folders",
                axum::routing::get(|| async { r#"{"folders":[]}"# }),
            )
            .route(
                "/folders/{*path}",
                axum::routing::get(|| async { r#"{"folders":[]}"# }),
            )
            .route(
                "/resources/by_asset_folder",
                axum::routing::get(move |uri: axum::http::Uri| {
                    let listing = root_listing.clone();
                    async move {
                        if !dynamic {
                            return (
                                axum::http::StatusCode::BAD_REQUEST,
                                r#"{"error":{"message":"Unknown parameter asset_folder"}}"#
                                    .to_string(),
                            );
                        }
                        let at_root = uri.query().unwrap_or("").contains("asset_folder=&")
                            || uri.query().unwrap_or("").ends_with("asset_folder=");
                        let body = if at_root {
                            listing
                        } else {
                            r#"{"resources":[]}"#.to_string()
                        };
                        (axum::http::StatusCode::OK, body)
                    }
                }),
            )
            .fallback({
                let listing = serde_json::json!({ "resources": [image("a", "jpg")] }).to_string();
                let occupied_listing = serde_json::json!({
                    "resources": [image("a", "jpg"), image("b", b_format)]
                })
                .to_string();
                move || {
                    let body = if occupied {
                        occupied_listing.clone()
                    } else {
                        listing.clone()
                    };
                    async move { body }
                }
            });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.ok() });
        let mut provider = CloudinaryProvider::new(CloudinaryConfig {
            cloud_name: "test".to_string(),
            api_key: "test".to_string(),
            api_secret: SecretString::from("test".to_string()),
            initial_path: None,
        });
        provider.connected = true;
        provider.api_base_override = Some(format!("http://{addr}"));
        (provider, renames, updates)
    }

    /// The value of `key` in a query string.
    fn query_value(query: &str, key: &str) -> Option<String> {
        url::form_urlencoded::parse(query.as_bytes())
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.into_owned())
    }

    /// An image's public id has no extension: renamed to `b.jpg` it takes
    /// the public id `b`. With `b.jpg` it was listed as `b.jpg` beside any
    /// asset `b`, also listed as `b.jpg`.
    #[tokio::test]
    async fn file_rename_never_asks_for_overwrite() {
        let (mut provider, renames, _) = provider_for_file_rename(false, false).await;
        provider.rename("/a.jpg", "/b.jpg").await.expect("rename");
        let renames = renames.lock().unwrap().clone();
        assert_eq!(renames.len(), 1, "{renames:?}");
        assert_eq!(
            query_value(&renames[0], "to_public_id").as_deref(),
            Some("b")
        );
        assert!(!renames[0].contains("overwrite"), "{renames:?}");
    }

    #[tokio::test]
    async fn file_rename_refuses_an_existing_destination() {
        let (mut provider, renames, _) = provider_for_file_rename(true, false).await;
        let outcome = provider.rename("/a.jpg", "/b.jpg").await;
        assert!(
            matches!(outcome, Err(ProviderError::AlreadyExists(_))),
            "{outcome:?}"
        );
        assert!(renames.lock().unwrap().is_empty());
    }

    /// `replace` is the verb for "put this over that" (CLI `edit`, MCP
    /// `remote_edit`, the AeroCrypt marker publish). On a fixed-folder
    /// account it takes the public id of the asset it replaces, with
    /// `overwrite=true`: with the target name `b.jpg` as the public id it
    /// created a second asset beside `b` and the old one stayed.
    #[tokio::test]
    async fn replace_renames_over_an_existing_destination_with_overwrite() {
        let (mut provider, renames, _) = provider_for_file_rename(true, false).await;
        provider.replace("/a.jpg", "/b.jpg").await.expect("replace");
        let renames = renames.lock().unwrap().clone();
        assert_eq!(renames.len(), 1, "{renames:?}");
        assert_eq!(
            query_value(&renames[0], "from_public_id").as_deref(),
            Some("a")
        );
        assert_eq!(
            query_value(&renames[0], "to_public_id").as_deref(),
            Some("b")
        );
        assert_eq!(
            query_value(&renames[0], "overwrite").as_deref(),
            Some("true")
        );
    }

    /// On a fixed-folder account `b.jpg` is free while the asset `b.pdf`
    /// holds the public id `b`. The rename took `b`, and the replace behind
    /// a served WebDAV MOVE with `Overwrite: T` asked for the overwrite:
    /// `b.pdf` was replaced by the image. It is refused, naming `b.pdf`.
    #[tokio::test]
    async fn a_free_name_whose_public_id_is_taken_is_refused() {
        let (mut provider, renames, _) = provider_for_file_rename_with(true, false, "pdf").await;
        for outcome in [
            provider.rename("/a.jpg", "/b.jpg").await,
            provider.replace("/a.jpg", "/b.jpg").await,
        ] {
            match outcome {
                Err(ProviderError::AlreadyExists(message)) => {
                    assert!(message.contains("b.pdf"), "{message}")
                }
                other => panic!("{other:?}"),
            }
        }
        assert!(
            renames.lock().unwrap().is_empty(),
            "{:?}",
            renames.lock().unwrap()
        );
    }

    /// A fixed-folder Cloudinary double holding `resources` (public id,
    /// format, resource type) and the folders `folders` at the root. Returns
    /// a provider on it and every rename query, and every delete as
    /// `DELETE <path>`.
    async fn provider_on_fixed_folders(
        resources: &'static [(&'static str, &'static str, &'static str)],
        folders: &'static [&'static str],
    ) -> (CloudinaryProvider, CloudinaryCalls) {
        use axum::response::IntoResponse;
        use std::sync::Arc;
        let renames: CloudinaryCalls = Arc::default();
        let seen = Arc::clone(&renames);
        let app =
            axum::Router::new().fallback(axum::routing::any(move |req: axum::extract::Request| {
                let seen = Arc::clone(&seen);
                async move {
                    let path = req.uri().path().to_string();
                    let query = req.uri().query().unwrap_or("").to_string();
                    if req.method() == axum::http::Method::POST && path.ends_with("/rename") {
                        seen.lock().unwrap().push(query);
                        return axum::Json(serde_json::json!({ "public_id": "x" })).into_response();
                    }
                    if req.method() == axum::http::Method::DELETE {
                        seen.lock().unwrap().push(format!("DELETE {path}"));
                        let public_id = query.rsplit('=').next().unwrap_or("").to_string();
                        return axum::Json(
                            serde_json::json!({ "deleted": { public_id: "deleted" } }),
                        )
                        .into_response();
                    }
                    if path == "/resources/by_asset_folder" {
                        return (
                            axum::http::StatusCode::BAD_REQUEST,
                            r#"{"error":{"message":"Unknown parameter asset_folder"}}"#,
                        )
                            .into_response();
                    }
                    if path == "/folders" {
                        let listed: Vec<serde_json::Value> = folders
                            .iter()
                            .map(|f| serde_json::json!({ "name": f, "path": f }))
                            .collect();
                        return axum::Json(serde_json::json!({ "folders": listed }))
                            .into_response();
                    }
                    if path.starts_with("/folders/") {
                        if req.method() == axum::http::Method::PUT {
                            seen.lock().unwrap().push(format!("PUT {path}"));
                        }
                        return axum::Json(serde_json::json!({ "folders": [] })).into_response();
                    }
                    let kind = path.trim_start_matches("/resources/");
                    let listed: Vec<serde_json::Value> = resources
                        .iter()
                        .filter(|(_, _, resource_type)| *resource_type == kind)
                        .map(|(public_id, format, resource_type)| {
                            let mut resource = serde_json::json!({
                                // Cloudinary's asset id is unique across
                                // types, unlike the public id.
                                "asset_id": format!("AID_{resource_type}_{public_id}"),
                                "public_id": public_id,
                                "bytes": 3, "resource_type": resource_type, "type": "upload",
                            });
                            // A raw asset may come without a format.
                            if !format.is_empty() {
                                resource["format"] = serde_json::json!(format);
                            }
                            resource
                        })
                        .collect();
                    axum::Json(serde_json::json!({ "resources": listed })).into_response()
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.ok() });
        let mut provider = CloudinaryProvider::new(CloudinaryConfig {
            cloud_name: "test".to_string(),
            api_key: "test".to_string(),
            api_secret: SecretString::from("test".to_string()),
            initial_path: None,
        });
        provider.connected = true;
        provider.api_base_override = Some(format!("http://{addr}"));
        (provider, renames)
    }

    /// A replace of a file onto a folder took the public id of an image
    /// named like the folder (`photos.png`, public id `photos`) and asked
    /// for the overwrite: the image was replaced. A replace across file and
    /// folder is refused before any call.
    #[tokio::test]
    async fn a_file_replaced_onto_a_folder_overwrites_no_image_named_like_it() {
        let (mut provider, renames) = provider_on_fixed_folders(
            &[("a", "jpg", "image"), ("photos", "png", "image")],
            &["photos"],
        )
        .await;
        let outcome = provider.replace("/a.jpg", "/photos").await;
        assert!(
            matches!(outcome, Err(ProviderError::AlreadyExists(_))),
            "{outcome:?}"
        );
        assert!(
            renames.lock().unwrap().is_empty(),
            "{:?}",
            renames.lock().unwrap()
        );
    }

    /// Public ids are kept per resource type. An image replaced onto the
    /// video `v.mp4` took the public id `v` among the images with
    /// overwrite=true: the image `v.png`, which nobody named, was overwritten
    /// and the video stayed. It is refused before any call.
    #[tokio::test]
    async fn a_replace_onto_an_asset_of_another_type_is_refused() {
        let (mut provider, renames) = provider_on_fixed_folders(
            &[
                ("a", "jpg", "image"),
                ("v", "png", "image"),
                ("v", "mp4", "video"),
            ],
            &[],
        )
        .await;
        let outcome = provider.replace("/a.jpg", "/v.mp4").await;
        assert!(
            matches!(outcome, Err(ProviderError::AlreadyExists(ref m)) if m.contains("video")),
            "{outcome:?}"
        );
        assert!(
            renames.lock().unwrap().is_empty(),
            "{:?}",
            renames.lock().unwrap()
        );
    }

    /// A folder replace onto an existing folder reached `PUT /folders`,
    /// which Cloudinary answers with 409: it is refused before the request.
    /// A folder move to a free name still goes through.
    #[tokio::test]
    async fn a_folder_replace_onto_an_existing_folder_is_refused_before_the_request() {
        let (mut provider, calls) = provider_on_fixed_folders(&[], &["a", "b"]).await;
        let outcome = provider.replace("/a", "/b").await;
        assert!(
            matches!(outcome, Err(ProviderError::NotSupported(_))),
            "{outcome:?}"
        );
        assert!(
            calls.lock().unwrap().is_empty(),
            "{:?}",
            calls.lock().unwrap()
        );

        provider
            .replace("/a", "/c")
            .await
            .expect("move to a free name");
        assert_eq!(*calls.lock().unwrap(), ["PUT /folders/a"]);
    }

    /// With an image `v` (`v.png`) and a video `v` (`v.mp4`), `rm /v.png`
    /// took the resource type from a cache keyed by public id, which held
    /// the type listed last, and deleted the video. The type is the one of
    /// the asset the path names.
    #[tokio::test]
    async fn rm_deletes_the_asset_its_path_names() {
        let (mut provider, calls) =
            provider_on_fixed_folders(&[("v", "png", "image"), ("v", "mp4", "video")], &[]).await;
        provider.delete("/v.png").await.expect("rm");
        assert_eq!(*calls.lock().unwrap(), ["DELETE /resources/image/upload"]);
    }

    /// A private `v` and an upload `v` are two assets (public ids are unique
    /// per resource type and delivery type), and the delete always named the
    /// upload one: `rm` of the private asset's path deleted the other.
    #[tokio::test]
    async fn rm_deletes_the_asset_of_the_delivery_type_its_path_names() {
        use std::sync::{Arc, Mutex};
        let deletes: Arc<Mutex<Vec<String>>> = Arc::default();
        let seen = Arc::clone(&deletes);
        let asset = |display: &str, delivery: &str| {
            serde_json::json!({
                "asset_id": format!("AID_{delivery}"), "public_id": "v",
                "display_name": display, "format": "jpg", "bytes": 3,
                "resource_type": "image", "type": delivery, "asset_folder": "",
            })
        };
        let listing = serde_json::json!({
            "resources": [asset("u", "upload"), asset("p", "private")]
        })
        .to_string();
        let app = axum::Router::new()
            .route(
                "/resources/by_asset_folder",
                axum::routing::get(move || {
                    let listing = listing.clone();
                    async move { listing }
                }),
            )
            .route(
                "/resources/{kind}/{delivery}",
                axum::routing::delete(move |uri: axum::http::Uri| {
                    seen.lock().unwrap().push(uri.path().to_string());
                    async { r#"{"deleted":{"v":"deleted"}}"# }
                }),
            )
            .route(
                "/folders",
                axum::routing::get(|| async { r#"{"folders":[]}"# }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.ok() });
        let mut provider = CloudinaryProvider::new(CloudinaryConfig {
            cloud_name: "test".to_string(),
            api_key: "test".to_string(),
            api_secret: SecretString::from("test".to_string()),
            initial_path: None,
        });
        provider.connected = true;
        provider.api_base_override = Some(format!("http://{addr}"));

        provider.delete("/p.jpg").await.expect("rm");
        assert_eq!(*deletes.lock().unwrap(), ["/resources/image/private"]);
    }

    /// On a fixed-folder account the listed path was the public id, which
    /// the image `b` (`b.png`) and a raw `b` share: an action on the image
    /// through its listed path found the raw asset. Each listed path names
    /// its own asset.
    #[tokio::test]
    async fn each_listed_path_names_its_own_asset() {
        let (mut provider, _) =
            provider_on_fixed_folders(&[("b", "png", "image"), ("b", "", "raw")], &[]).await;
        let listed = provider.list("/").await.expect("list");
        assert_eq!(listed.len(), 2, "{listed:?}");
        for entry in listed {
            let found = provider.stat(&entry.path).await.expect("stat");
            assert_eq!(
                found.metadata.get("resource_type"),
                entry.metadata.get("resource_type"),
                "{} resolved to {found:?}",
                entry.path
            );
        }
    }

    /// Two assets that show one name (public ids `a` and `a.jpg`, both listed
    /// `a.jpg`) shared the path `/a.jpg`, and `stat` handed over the first
    /// listed, so `rm` of one could delete the other. Such entries keep their
    /// public id as their path, each resolving to its own asset; a path that
    /// still names two assets (an image and a raw asset both `a.jpg`) is
    /// refused as ambiguous.
    #[tokio::test]
    async fn a_name_two_assets_show_gives_each_its_own_path() {
        let (mut provider, _) =
            provider_on_fixed_folders(&[("a", "jpg", "image"), ("a.jpg", "jpg", "image")], &[])
                .await;
        let listed = provider.list("/").await.expect("list");
        assert_eq!(listed.len(), 2, "{listed:?}");
        for entry in listed {
            let found = provider.stat(&entry.path).await.expect("stat");
            assert_eq!(
                found.metadata.get("public_id"),
                entry.metadata.get("public_id"),
                "{} resolved to {found:?}",
                entry.path
            );
        }
        let (mut provider, _) =
            provider_on_fixed_folders(&[("a.jpg", "jpg", "image"), ("a.jpg", "", "raw")], &[])
                .await;
        let outcome = provider.stat("/a.jpg").await;
        assert!(
            matches!(outcome, Err(ProviderError::InvalidPath(_))),
            "{outcome:?}"
        );
    }

    /// `stat` resolved a listed path by looking at folders first, then by
    /// name, then by public id over all types (images first): an action on
    /// one item reached another. It resolves the path `list` gives first.
    /// Here the video `v` is listed `/v` (the video `v.mp4` shows the same
    /// name), and `/v` resolved to the image `v`.
    #[tokio::test]
    async fn a_listed_public_id_path_resolves_to_its_own_asset() {
        let (mut provider, _) = provider_on_fixed_folders(
            &[
                ("v", "png", "image"),
                ("v", "mp4", "video"),
                ("v.mp4", "mp4", "video"),
            ],
            &[],
        )
        .await;
        let found = provider.stat("/v").await.expect("stat /v");
        assert_eq!(
            (
                found.metadata.get("public_id").map(String::as_str),
                found.metadata.get("resource_type").map(String::as_str)
            ),
            (Some("v"), Some("video")),
            "{found:?}"
        );
    }

    /// The image `a` (listed `/a`, as the image `a.jpg` shows the same name)
    /// and the raw `a` share the path `/a`, which resolved to the raw asset:
    /// `rm` of the image deleted it. A path two assets hold is refused.
    #[tokio::test]
    async fn a_path_two_assets_hold_is_refused() {
        let (mut provider, _) = provider_on_fixed_folders(
            &[
                ("a", "", "raw"),
                ("a", "jpg", "image"),
                ("a.jpg", "jpg", "image"),
            ],
            &[],
        )
        .await;
        let shared = provider.stat("/a").await;
        assert!(
            matches!(shared, Err(ProviderError::InvalidPath(_))),
            "{shared:?}"
        );
    }

    /// The image `photos` (listed `/photos`, as the raw `photos.png` shows
    /// the same name) and the folder `photos` share the path, which resolved
    /// to the folder: `rm` of the image acted on the folder. A path an asset
    /// and a folder hold is refused.
    #[tokio::test]
    async fn a_path_an_asset_and_a_folder_hold_is_refused() {
        let (mut provider, _) = provider_on_fixed_folders(
            &[("photos", "png", "image"), ("photos.png", "", "raw")],
            &["photos"],
        )
        .await;
        let with_folder = provider.stat("/photos").await;
        assert!(
            matches!(with_folder, Err(ProviderError::InvalidPath(_))),
            "{with_folder:?}"
        );
    }

    /// On a dynamic-folder account two assets in one folder may show one
    /// display name (an upload made twice): both listed at `/a.jpg`, and
    /// `stat` took the first, so `rm` of one could delete the other. The
    /// path is refused as ambiguous.
    #[tokio::test]
    async fn a_display_name_two_assets_share_is_refused_on_dynamic_folders() {
        let asset = |public_id: &str| {
            serde_json::json!({
                "asset_id": format!("AID_{public_id}"), "public_id": public_id,
                "display_name": "a", "format": "jpg", "bytes": 3,
                "resource_type": "image", "type": "upload", "asset_folder": "",
            })
        };
        let mut provider =
            provider_listing_all(serde_json::json!([asset("x1"), asset("x2")]), true).await;
        let outcome = provider.stat("/a.jpg").await;
        assert!(
            matches!(outcome, Err(ProviderError::InvalidPath(_))),
            "{outcome:?}"
        );
    }

    /// A path names the asset `list` shows under that name first: with a
    /// raw `b` and an image `b.png` (public id `b`), `/b` resolved to the
    /// image, listed first, while `ls` shows the raw asset as `b`.
    #[tokio::test]
    async fn a_path_resolves_to_the_listed_name_before_a_public_id() {
        let (mut provider, _) =
            provider_on_fixed_folders(&[("b", "png", "image"), ("b", "", "raw")], &[]).await;
        let found = provider.stat("/b").await.expect("stat");
        assert_eq!(
            found.metadata.get("resource_type").map(String::as_str),
            Some("raw"),
            "{found:?}"
        );
    }

    /// Public ids are unique per resource type: with a video `b` (`b.mp4`),
    /// the image `a.jpg` may still become `b.jpg`. And `a.jpg` whose public
    /// id is `a.jpg` may become `a.jpg.jpg`: its own id is no holder.
    #[tokio::test]
    async fn only_an_asset_of_the_same_type_other_than_the_source_holds_the_id() {
        let (mut provider, renames) =
            provider_on_fixed_folders(&[("a", "jpg", "image"), ("b", "mp4", "video")], &[]).await;
        provider
            .rename("/a.jpg", "/b.jpg")
            .await
            .expect("an image b is free");
        let (mut provider, own) =
            provider_on_fixed_folders(&[("a.jpg", "jpg", "image")], &[]).await;
        provider
            .rename("/a.jpg", "/a.jpg.jpg")
            .await
            .expect("the source is no holder");
        assert_eq!(
            query_value(&renames.lock().unwrap()[0], "to_public_id").as_deref(),
            Some("b")
        );
        assert_eq!(
            query_value(&own.lock().unwrap()[0], "to_public_id").as_deref(),
            Some("a.jpg.jpg")
        );
    }

    /// On a dynamic-folder account two assets may share a display name, so
    /// a replace moves the new asset in first and then deletes the old one:
    /// the destination is never empty and never left doubled.
    #[tokio::test]
    async fn dynamic_folder_replace_moves_in_then_deletes_the_displaced_asset() {
        let (mut provider, renames, updates) = provider_for_file_rename(true, true).await;
        provider.replace("/a.jpg", "/b.jpg").await.expect("replace");
        assert!(renames.lock().unwrap().is_empty());
        let updates = updates.lock().unwrap().clone();
        assert_eq!(updates.len(), 2, "{updates:?}");
        assert_eq!(updates[0], "display_name=b");
        assert!(
            updates[1].starts_with("DELETE ") && updates[1].contains("public_ids[]=b"),
            "{updates:?}"
        );
    }

    /// Cloudinary refuses a rename onto a taken public id with a 400; one
    /// taken after the destination check must still read as AlreadyExists,
    /// which sync and `mkdir -p` handle, not as a configuration error.
    #[test]
    fn a_taken_public_id_is_already_exists_not_invalid_config() {
        let refused = CloudinaryProvider::classify_cloudinary_error(
            400,
            r#"{"error":{"message":"to_public_id (b) already exists"}}"#,
        );
        assert!(
            matches!(refused, ProviderError::AlreadyExists(_)),
            "{refused:?}"
        );
        let invalid = CloudinaryProvider::classify_cloudinary_error(
            400,
            r#"{"error":{"message":"Invalid public_id"}}"#,
        );
        assert!(
            matches!(invalid, ProviderError::InvalidConfig(_)),
            "{invalid:?}"
        );
    }

    /// On a dynamic-folder account the folder is `asset_folder` and the
    /// name is `display_name`: renaming the public id answered Ok and moved
    /// nothing (found live, 2026-09-25).
    #[tokio::test]
    async fn dynamic_folder_move_updates_the_asset_folder_and_display_name() {
        let (mut provider, renames, updates) = provider_for_file_rename(false, true).await;
        provider.rename("/a.jpg", "/sub/b.jpg").await.expect("move");
        assert!(
            renames.lock().unwrap().is_empty(),
            "the public id must not change"
        );
        let updates = updates.lock().unwrap().clone();
        assert_eq!(updates, ["asset_folder=sub&display_name=b"], "{updates:?}");
    }

    #[tokio::test]
    async fn folder_listing_failure_is_not_an_empty_or_missing_path() {
        use axum::{http::StatusCode, routing::get, Router};

        let app = Router::new()
            .route(
                "/folders/{*path}",
                get(|| async {
                    (
                        StatusCode::FORBIDDEN,
                        r#"{"error":{"message":"folder access denied"}}"#,
                    )
                }),
            )
            .fallback(|| async { r#"{"resources":[]}"# });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut provider = CloudinaryProvider::new(CloudinaryConfig {
            cloud_name: "test".to_string(),
            api_key: "test".to_string(),
            api_secret: SecretString::from("test".to_string()),
            initial_path: None,
        });
        provider.connected = true;
        provider.api_base_override = Some(format!("http://{addr}"));

        let outcomes = [
            provider.list("/parent").await.map(|_| ()),
            provider.stat("/parent/child").await.map(|_| ()),
            provider.find("/parent", "*").await.map(|_| ()),
            provider.rmdir_recursive("/parent").await,
            provider.exists("/parent/child").await.map(|_| ()),
        ];
        server.abort();
        for outcome in outcomes {
            assert!(
                matches!(outcome, Err(ProviderError::AuthenticationFailed(ref message))
                if message == "folder access denied"),
                "{outcome:?}"
            );
        }
    }

    // Row 4 (#347): the JSON `error.message` becomes the human text and the HTTP
    // status selects the variant; auth folds 401 and 403 together.
    #[test]
    fn classify_cloudinary_error_maps_status_to_variant() {
        let body = r#"{"error":{"message":"resource not found"}}"#;
        assert!(matches!(
            CloudinaryProvider::classify_cloudinary_error(404, body),
            ProviderError::NotFound(ref m) if m == "resource not found"
        ));

        let auth = r#"{"error":{"message":"bad signature"}}"#;
        for code in [401u16, 403] {
            assert!(
                matches!(
                    CloudinaryProvider::classify_cloudinary_error(code, auth),
                    ProviderError::AuthenticationFailed(ref m) if m == "bad signature"
                ),
                "HTTP {code} must map to AuthenticationFailed"
            );
        }

        let conflict = r#"{"error":{"message":"already there"}}"#;
        assert!(matches!(
            CloudinaryProvider::classify_cloudinary_error(409, conflict),
            ProviderError::AlreadyExists(_)
        ));

        // Other 4xx -> InvalidConfig; 5xx -> ServerError.
        assert!(matches!(
            CloudinaryProvider::classify_cloudinary_error(400, r#"{"error":{"message":"bad req"}}"#),
            ProviderError::InvalidConfig(ref m) if m == "bad req"
        ));
        assert!(matches!(
            CloudinaryProvider::classify_cloudinary_error(500, r#"{"error":{"message":"boom"}}"#),
            ProviderError::ServerError(ref m) if m == "boom"
        ));

        // 3xx (non client/server) -> Other with "HTTP <code> <reason>: <msg>".
        match CloudinaryProvider::classify_cloudinary_error(302, r#"{"error":{"message":"moved"}}"#)
        {
            ProviderError::Other(msg) => assert!(msg.contains("HTTP 302"), "got: {msg}"),
            e => panic!("expected Other, got {e:?}"),
        }
    }

    // When the body is not the Cloudinary error envelope, the message falls back
    // to the sanitized raw body rather than being lost.
    #[test]
    fn classify_cloudinary_error_falls_back_to_sanitized_body() {
        match CloudinaryProvider::classify_cloudinary_error(404, "<html>nope</html>") {
            ProviderError::NotFound(msg) => assert!(!msg.is_empty(), "message must not be empty"),
            e => panic!("expected NotFound, got {e:?}"),
        }
        // Empty `error.message` is treated as absent -> sanitized body.
        match CloudinaryProvider::classify_cloudinary_error(404, r#"{"error":{"message":"   "}}"#) {
            ProviderError::NotFound(_) => {}
            e => panic!("expected NotFound, got {e:?}"),
        }
    }

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
        assert!(validate_download_url(
            "https://res.cloudinary.com/demo/image/upload/v1/sample.jpg"
        )
        .is_ok());
    }

    #[test]
    fn test_validate_download_url_rejects_http() {
        assert!(
            validate_download_url("http://res.cloudinary.com/demo/image/upload/v1/sample.jpg")
                .is_err()
        );
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

    // ---- S3-T14 multipart trait wiring ----

    #[test]
    fn cloudinary_multipart_meta_roundtrip_preserves_fields() {
        let meta = CloudinaryMultipartMeta {
            unique_upload_id: "abc123def456".to_string(),
            upload_url: "https://api.cloudinary.com/v1_1/test/auto/upload".to_string(),
            folder: "Documents/2026".to_string(),
            file_name: "weird name (1).bin".to_string(),
            total: 1_073_741_824,
            part: 20 * 1024 * 1024,
            total_parts: 52,
        };
        let encoded = meta.encode();
        let decoded = CloudinaryMultipartMeta::decode(&encoded).expect("decode roundtrip");
        assert_eq!(meta, decoded);
    }

    #[test]
    fn cloudinary_multipart_meta_decode_rejects_garbage() {
        let err = CloudinaryMultipartMeta::decode("not-json").unwrap_err();
        assert!(matches!(err, ProviderError::Other(_)));
    }

    #[test]
    fn cloudinary_runner_part_size_clamps_and_never_returns_zero() {
        assert_eq!(cloudinary_runner_part_size(1024), 1024);
        assert_eq!(
            cloudinary_runner_part_size(CLOUDINARY_MULTIPART_PART_SIZE),
            CLOUDINARY_MULTIPART_PART_SIZE
        );
        assert_eq!(
            cloudinary_runner_part_size(50 * 1024 * 1024 * 1024),
            CLOUDINARY_MULTIPART_PART_SIZE
        );
        assert_eq!(cloudinary_runner_part_size(0), 1);
    }

    #[test]
    fn cloudinary_total_parts_matches_runner_formula() {
        let p = CLOUDINARY_MULTIPART_PART_SIZE;
        assert_eq!(cloudinary_total_parts(0, p), 1);
        assert_eq!(cloudinary_total_parts(p, p), 1);
        assert_eq!(cloudinary_total_parts(4 * p, p), 4);
        assert_eq!(cloudinary_total_parts(p + 1, p), 2);
        // part=0 guard
        assert_eq!(cloudinary_total_parts(7, 0), 7);
    }

    #[test]
    fn cloudinary_content_range_math_is_inclusive_zero_based() {
        let part = CLOUDINARY_MULTIPART_PART_SIZE;
        let total: u64 = 2 * part + 4096;
        let range = |n: u32| -> String {
            let offset = (n as u64 - 1) * part;
            let len = ((total - offset).min(part)) as usize;
            let end = offset + len as u64;
            format!("bytes {}-{}/{}", offset, end - 1, total)
        };
        assert_eq!(range(1), format!("bytes 0-{}/{}", part - 1, total));
        assert_eq!(
            range(2),
            format!("bytes {}-{}/{}", part, 2 * part - 1, total)
        );
        assert_eq!(
            range(3),
            format!("bytes {}-{}/{}", 2 * part, total - 1, total)
        );
    }
}
