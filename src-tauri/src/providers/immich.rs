//! Immich Storage Provider
//!
//! Implements StorageProvider for Immich photo management servers.
//! Uses API key authentication (x-api-key header).
//! Albums are shown as directories, media assets as files.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue};
use serde::Deserialize;
use std::collections::HashMap;
use tracing::info;

use super::{
    sanitize_api_error, ProviderConfig, ProviderError, ProviderType, RemoteEntry,
    ShareLinkCapabilities, ShareLinkOptions, ShareLinkResult, StorageProvider,
    AEROFTP_USER_AGENT,
};

const IMMICH_DEVICE_ID: &str = "aeroftp-desktop";

/// Virtual folder names rendered at the root level alongside real albums.
const VIRTUAL_ALL_ASSETS: &str = "[All Assets]";
const VIRTUAL_FAVORITES: &str = "[Favorites]";

// ---------------------------------------------------------------------------
// API response structs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImmichAlbum {
    id: String,
    album_name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    asset_count: u32,
    #[serde(default)]
    assets: Vec<ImmichAsset>,
    updated_at: Option<String>,
    #[allow(dead_code)]
    owner: Option<ImmichUser>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImmichAsset {
    id: String,
    #[serde(default)]
    original_file_name: Option<String>,
    #[serde(default)]
    original_mime_type: Option<String>,
    #[serde(rename = "type")]
    asset_type: Option<String>, // IMAGE, VIDEO, AUDIO, OTHER
    file_created_at: Option<String>,
    file_modified_at: Option<String>,
    #[serde(default)]
    is_favorite: bool,
    #[serde(default)]
    is_trashed: bool,
    #[serde(default)]
    exif_info: Option<ImmichExif>,
    #[serde(default)]
    checksum: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct ImmichExif {
    #[serde(alias = "fileSizeInByte")]
    file_size: Option<u64>,
    #[serde(default)]
    make: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    image_width: Option<u32>,
    #[serde(default)]
    image_height: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct ImmichUser {
    id: Option<String>,
    email: Option<String>,
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImmichServerPing {
    #[serde(default)]
    res: Option<String>, // "pong"
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImmichServerVersion {
    major: u32,
    minor: u32,
    patch: u32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImmichSharedLink {
    id: Option<String>,
    key: Option<String>,
    #[allow(dead_code)]
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchMetadataResponse {
    assets: SearchMetadataAssets,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchMetadataAssets {
    #[serde(default)]
    items: Vec<ImmichAsset>,
    #[allow(dead_code)]
    #[serde(default)]
    next_page: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImmichUploadResponse {
    id: String,
    #[allow(dead_code)]
    status: Option<String>, // "created" or "duplicate"
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImmichCreateAlbumResponse {
    id: String,
    #[allow(dead_code)]
    album_name: Option<String>,
}

// ---------------------------------------------------------------------------
// Config & Provider structs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ImmichConfig {
    pub base_url: String, // e.g. https://photos.example.com
    pub api_key: String,
}

impl ImmichConfig {
    pub fn new(base_url: &str, api_key: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
        }
    }

    pub fn from_provider_config(config: &ProviderConfig) -> Result<Self, ProviderError> {
        let base_url = if config.host.is_empty() {
            return Err(ProviderError::Other(
                "Missing Immich server URL".to_string(),
            ));
        } else if config.host.starts_with("http") {
            config.host.clone()
        } else {
            format!("https://{}", config.host)
        };

        let api_key = config
            .password
            .clone()
            .ok_or_else(|| ProviderError::Other("Missing API key".to_string()))?;

        Ok(Self::new(&base_url, &api_key))
    }
}

pub struct ImmichProvider {
    config: ImmichConfig,
    client: reqwest::Client,
    connected: bool,
    current_path: String,
    /// Album name -> album ID cache
    album_cache: HashMap<String, String>,
    /// Reverse album cache: album ID -> album name
    album_id_to_name: HashMap<String, String>,
    account_email: Option<String>,
    server_version: Option<String>,
}

impl ImmichProvider {
    pub fn new(config: ImmichConfig) -> Self {
        let mut headers = HeaderMap::new();
        if let Ok(val) = HeaderValue::from_str(&config.api_key) {
            headers.insert("x-api-key", val);
        }

        let client = reqwest::Client::builder()
            .user_agent(AEROFTP_USER_AGENT)
            .default_headers(headers)
            .connect_timeout(std::time::Duration::from_secs(15))
            .connect_timeout(std::time::Duration::from_secs(30))
            .read_timeout(std::time::Duration::from_secs(300))
            .build()
            .unwrap_or_default();

        Self {
            config,
            client,
            connected: false,
            current_path: "/".to_string(),
            album_cache: HashMap::new(),
            album_id_to_name: HashMap::new(),
            account_email: None,
            server_version: None,
        }
    }

    /// Build full API URL from a relative path.
    fn api_url(&self, path: &str) -> String {
        format!("{}/api{}", self.config.base_url, path)
    }

    /// Map an HTTP status code + body to a ProviderError.
    fn map_api_error(status: reqwest::StatusCode, body: &str, context: &str) -> ProviderError {
        let sanitized = sanitize_api_error(body);
        match status.as_u16() {
            401 => ProviderError::AuthenticationFailed(format!("{}: {}", context, sanitized)),
            403 => ProviderError::PermissionDenied(format!("{}: {}", context, sanitized)),
            404 => ProviderError::NotFound(format!("{}: {}", context, sanitized)),
            429 => ProviderError::Other(format!("Rate limited - {}: {}", context, sanitized)),
            500..=599 => {
                ProviderError::ConnectionFailed(format!("Server error {}: {}", status, sanitized))
            }
            _ => ProviderError::Other(format!("{} (HTTP {}): {}", context, status, sanitized)),
        }
    }

    // -----------------------------------------------------------------------
    // API helpers
    // -----------------------------------------------------------------------

    /// List all albums (GET /api/albums).
    async fn list_albums(&self) -> Result<Vec<ImmichAlbum>, ProviderError> {
        let url = self.api_url("/albums");
        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(Self::map_api_error(status, &text, "List albums"));
        }

        response
            .json::<Vec<ImmichAlbum>>()
            .await
            .map_err(|e| ProviderError::ParseError(format!("Parse albums: {}", e)))
    }

    /// Get all assets inside an album (GET /api/albums/{id}).
    async fn get_album_assets(
        &self,
        album_id: &str,
    ) -> Result<Vec<ImmichAsset>, ProviderError> {
        let url = self.api_url(&format!("/albums/{}", album_id));
        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(Self::map_api_error(status, &text, "Get album assets"));
        }

        let album: ImmichAlbum = response
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(format!("Parse album: {}", e)))?;

        Ok(album.assets)
    }

    /// Resolve album title to ID, using cache or refreshing from API.
    async fn resolve_album_id(&mut self, title: &str) -> Result<String, ProviderError> {
        if let Some(id) = self.album_cache.get(title) {
            return Ok(id.clone());
        }

        // Refresh cache from API
        let albums = self.list_albums().await?;
        for album in &albums {
            self.album_cache
                .insert(album.album_name.clone(), album.id.clone());
            self.album_id_to_name
                .insert(album.id.clone(), album.album_name.clone());
        }

        self.album_cache
            .get(title)
            .cloned()
            .ok_or_else(|| ProviderError::NotFound(format!("Album not found: {}", title)))
    }

    /// Find an asset by filename inside an album or virtual folder.
    async fn resolve_asset(
        &mut self,
        path: &str,
    ) -> Result<(String, ImmichAsset), ProviderError> {
        let (folder, filename) = Self::parse_path(path);
        let folder_name = folder.ok_or_else(|| {
            ProviderError::InvalidPath("Path must include an album/folder".to_string())
        })?;
        let filename = filename.ok_or_else(|| {
            ProviderError::InvalidPath("Path must include a filename".to_string())
        })?;

        let (album_id, items) = match folder_name {
            VIRTUAL_ALL_ASSETS => {
                let items = self.search_metadata(None, None, 1000).await?;
                (String::new(), items)
            }
            VIRTUAL_FAVORITES => {
                let items = self.search_metadata(None, Some(true), 1000).await?;
                (String::new(), items)
            }
            album_title => {
                let album_id = self.resolve_album_id(album_title).await?;
                let items = self.get_album_assets(&album_id).await?;
                (album_id, items)
            }
        };

        let asset = if let Some(asset) = items.into_iter().find(|a| {
            a.original_file_name
                .as_deref()
                .map(|f| f == filename)
                .unwrap_or(false)
        }) {
            asset
        } else {
            // Some Immich album responses expose a truncated embedded `assets` list.
            // Fall back to an exact filename search so single-file operations still work.
            self.search_metadata(Some(filename), None, 1000)
                .await?
                .into_iter()
                .find(|a| {
                    a.original_file_name
                        .as_deref()
                        .map(|f| f == filename)
                        .unwrap_or(false)
                })
                .ok_or_else(|| ProviderError::NotFound(format!("{}/{}", folder_name, filename)))?
        };

        Ok((album_id, asset))
    }

    /// Search assets via POST /api/search/metadata.
    async fn search_metadata(
        &self,
        original_filename: Option<&str>,
        is_favorite: Option<bool>,
        size: u32,
    ) -> Result<Vec<ImmichAsset>, ProviderError> {
        let url = self.api_url("/search/metadata");

        let mut body = serde_json::json!({
            "size": size,
            "page": 1
        });

        if let Some(fname) = original_filename {
            body["originalFileName"] = serde_json::Value::String(fname.to_string());
        }
        if let Some(fav) = is_favorite {
            body["isFavorite"] = serde_json::Value::Bool(fav);
        }

        let response = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(Self::map_api_error(status, &text, "Search metadata"));
        }

        let result: SearchMetadataResponse = response
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(format!("Parse search: {}", e)))?;

        Ok(result.assets.items)
    }

    /// Convert an ImmichAsset into a RemoteEntry.
    fn asset_to_remote_entry(&self, asset: &ImmichAsset, path_prefix: &str) -> RemoteEntry {
        let filename = asset
            .original_file_name
            .clone()
            .unwrap_or_else(|| format!("asset_{}", asset.id));
        let size = asset
            .exif_info
            .as_ref()
            .and_then(|e| e.file_size)
            .unwrap_or(0);
        let modified = asset
            .file_modified_at
            .clone()
            .or_else(|| asset.file_created_at.clone());
        let mime = asset.original_mime_type.clone();

        let path = format!("{}/{}", path_prefix.trim_end_matches('/'), filename);

        let mut metadata = HashMap::new();
        metadata.insert("id".to_string(), asset.id.clone());
        if let Some(ref t) = asset.asset_type {
            metadata.insert("assetType".to_string(), t.clone());
        }
        if asset.is_favorite {
            metadata.insert("favorite".to_string(), "true".to_string());
        }
        if let Some(ref checksum) = asset.checksum {
            metadata.insert("checksum".to_string(), checksum.clone());
        }
        if let Some(ref exif) = asset.exif_info {
            if let Some(ref make) = exif.make {
                metadata.insert("cameraMake".to_string(), make.clone());
            }
            if let Some(ref model) = exif.model {
                metadata.insert("cameraModel".to_string(), model.clone());
            }
            if let Some(w) = exif.image_width {
                metadata.insert("width".to_string(), w.to_string());
            }
            if let Some(h) = exif.image_height {
                metadata.insert("height".to_string(), h.to_string());
            }
        }

        RemoteEntry {
            name: filename,
            path,
            is_dir: false,
            size,
            modified,
            permissions: None,
            owner: None,
            group: None,
            is_symlink: false,
            link_target: None,
            mime_type: mime,
            metadata,
        }
    }

    /// Parse a path to extract the top-level segment (album name or virtual
    /// folder) and the remaining filename, if any.
    ///
    /// Examples:
    ///   "/" -> (None, None)
    ///   "/Vacation" -> (Some("Vacation"), None)
    ///   "/Vacation/photo.jpg" -> (Some("Vacation"), Some("photo.jpg"))
    ///   "/[All Assets]/photo.jpg" -> (Some("[All Assets]"), Some("photo.jpg"))
    fn parse_path(path: &str) -> (Option<&str>, Option<&str>) {
        let trimmed = path.trim_matches('/');
        if trimmed.is_empty() {
            return (None, None);
        }
        match trimmed.find('/') {
            Some(pos) => {
                let folder = &trimmed[..pos];
                let rest = &trimmed[pos + 1..];
                if rest.is_empty() {
                    (Some(folder), None)
                } else {
                    (Some(folder), Some(rest))
                }
            }
            None => (Some(trimmed), None),
        }
    }

    /// Generate a deviceAssetId from filename + file size using SHA-256.
    fn device_asset_id(filename: &str, file_size: u64) -> String {
        use sha2::{Digest, Sha256};
        let input = format!("{}{}", filename, file_size);
        let hash = Sha256::digest(input.as_bytes());
        format!("{:x}", hash)
    }
}

// ---------------------------------------------------------------------------
// StorageProvider trait implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl StorageProvider for ImmichProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn provider_type(&self) -> ProviderType {
        ProviderType::Immich
    }

    fn display_name(&self) -> String {
        format!("Immich ({})", self.config.base_url)
    }

    fn account_email(&self) -> Option<String> {
        self.account_email.clone()
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        // 1. Ping
        let ping_url = self.api_url("/server/ping");
        let ping_resp = self
            .client
            .get(&ping_url)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !ping_resp.status().is_success() {
            let status = ping_resp.status();
            let text = ping_resp.text().await.unwrap_or_default();
            return Err(Self::map_api_error(status, &text, "Server ping"));
        }

        let ping: ImmichServerPing = ping_resp
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(format!("Parse ping: {}", e)))?;

        if ping.res.as_deref() != Some("pong") {
            return Err(ProviderError::ConnectionFailed(
                "Immich server did not respond with pong".to_string(),
            ));
        }

        // 2. Server version
        let version_url = self.api_url("/server/version");
        if let Ok(resp) = self.client.get(&version_url).send().await {
            if resp.status().is_success() {
                if let Ok(ver) = resp.json::<ImmichServerVersion>().await {
                    self.server_version =
                        Some(format!("{}.{}.{}", ver.major, ver.minor, ver.patch));
                }
            }
        }

        // 3. Current user info (GET /api/users/me)
        let me_url = self.api_url("/users/me");
        if let Ok(resp) = self.client.get(&me_url).send().await {
            if resp.status().is_success() {
                if let Ok(user) = resp.json::<ImmichUser>().await {
                    self.account_email = user.email.clone();
                }
            }
        }

        // 4. Populate album cache
        let albums = self.list_albums().await?;
        for album in &albums {
            self.album_cache
                .insert(album.album_name.clone(), album.id.clone());
            self.album_id_to_name
                .insert(album.id.clone(), album.album_name.clone());
        }

        self.connected = true;
        self.current_path = "/".to_string();

        info!(
            "Connected to Immich {} ({} albums, user={})",
            self.server_version.as_deref().unwrap_or("unknown"),
            self.album_cache.len(),
            self.account_email.as_deref().unwrap_or("unknown"),
        );
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.connected = false;
        self.album_cache.clear();
        self.album_id_to_name.clear();
        self.account_email = None;
        self.server_version = None;
        info!("Disconnected from Immich");
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let effective_path = if path == "." || path.is_empty() {
            self.current_path.clone()
        } else {
            path.to_string()
        };

        let (folder, _filename) = Self::parse_path(&effective_path);

        match folder {
            // Root: list albums + virtual folders
            None => {
                let mut entries = Vec::new();

                // Virtual folders first
                entries.push(RemoteEntry {
                    name: VIRTUAL_ALL_ASSETS.to_string(),
                    path: format!("/{}", VIRTUAL_ALL_ASSETS),
                    is_dir: true,
                    size: 0,
                    modified: None,
                    permissions: None,
                    owner: None,
                    group: None,
                    is_symlink: false,
                    link_target: None,
                    mime_type: None,
                    metadata: HashMap::new(),
                });

                entries.push(RemoteEntry {
                    name: VIRTUAL_FAVORITES.to_string(),
                    path: format!("/{}", VIRTUAL_FAVORITES),
                    is_dir: true,
                    size: 0,
                    modified: None,
                    permissions: None,
                    owner: None,
                    group: None,
                    is_symlink: false,
                    link_target: None,
                    mime_type: None,
                    metadata: HashMap::new(),
                });

                // Real albums
                let albums = self.list_albums().await?;
                for album in &albums {
                    // Refresh cache
                    self.album_cache
                        .insert(album.album_name.clone(), album.id.clone());
                    self.album_id_to_name
                        .insert(album.id.clone(), album.album_name.clone());

                    let mut metadata = HashMap::new();
                    metadata.insert("id".to_string(), album.id.clone());
                    metadata
                        .insert("assetCount".to_string(), album.asset_count.to_string());
                    if let Some(ref desc) = album.description {
                        metadata.insert("description".to_string(), desc.clone());
                    }

                    entries.push(RemoteEntry {
                        name: album.album_name.clone(),
                        path: format!("/{}", album.album_name),
                        is_dir: true,
                        size: 0,
                        modified: album.updated_at.clone(),
                        permissions: None,
                        owner: None,
                        group: None,
                        is_symlink: false,
                        link_target: None,
                        mime_type: None,
                        metadata,
                    });
                }

                info!("Listed Immich root: {} entries", entries.len());
                Ok(entries)
            }
            // Inside a folder (album or virtual)
            Some(folder_name) => {
                let path_prefix = format!("/{}", folder_name);
                let items = match folder_name {
                    VIRTUAL_ALL_ASSETS => self.search_metadata(None, None, 1000).await?,
                    VIRTUAL_FAVORITES => {
                        self.search_metadata(None, Some(true), 1000).await?
                    }
                    album_title => {
                        let album_id = self.resolve_album_id(album_title).await?;
                        self.get_album_assets(&album_id).await?
                    }
                };

                let entries: Vec<RemoteEntry> = items
                    .iter()
                    .filter(|a| !a.is_trashed)
                    .map(|asset| self.asset_to_remote_entry(asset, &path_prefix))
                    .collect();

                info!("Listed {}: {} items", folder_name, entries.len());
                Ok(entries)
            }
        }
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> {
        Ok(self.current_path.clone())
    }

    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        let new_path = if path.starts_with('/') {
            path.to_string()
        } else if path == ".." {
            let mut parts: Vec<&str> = self
                .current_path
                .split('/')
                .filter(|s| !s.is_empty())
                .collect();
            parts.pop();
            if parts.is_empty() {
                "/".to_string()
            } else {
                format!("/{}", parts.join("/"))
            }
        } else {
            format!(
                "{}/{}",
                self.current_path.trim_end_matches('/'),
                path
            )
        };

        // Validate the path exists
        let (folder, filename) = Self::parse_path(&new_path);
        match folder {
            None => {
                // Root - always valid
            }
            Some(folder_name) => {
                if filename.is_some() {
                    return Err(ProviderError::InvalidPath(
                        "Cannot cd into a file".to_string(),
                    ));
                }
                match folder_name {
                    VIRTUAL_ALL_ASSETS | VIRTUAL_FAVORITES => { /* always valid */ }
                    album_title => {
                        self.resolve_album_id(album_title).await?;
                    }
                }
            }
        }

        self.current_path = if new_path.is_empty() {
            "/".to_string()
        } else {
            new_path
        };

        Ok(())
    }

    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        self.cd("..").await
    }

    async fn download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        use futures_util::StreamExt;

        let (_album_id, asset) = self.resolve_asset(remote_path).await?;
        let total_size = asset
            .exif_info
            .as_ref()
            .and_then(|e| e.file_size)
            .unwrap_or(0);

        // GET /api/assets/{id}/original
        let url = self.api_url(&format!("/assets/{}/original", asset.id));
        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(Self::map_api_error(status, &text, "Download asset"));
        }

        let mut stream = response.bytes_stream();
        let mut atomic = super::atomic_write::AtomicFile::new(local_path)
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        let mut downloaded: u64 = 0;
        while let Some(chunk) = stream.next().await {
            let chunk =
                chunk.map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
            atomic
                .write_all(&chunk)
                .await
                .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
            downloaded += chunk.len() as u64;
            if let Some(ref cb) = on_progress {
                cb(downloaded, total_size);
            }
        }

        atomic.commit().await.map_err(|e| {
            ProviderError::TransferFailed(format!("Failed to finalize download: {}", e))
        })?;

        info!("Downloaded {} to {}", remote_path, local_path);
        Ok(())
    }

    async fn download_to_bytes(
        &mut self,
        remote_path: &str,
    ) -> Result<Vec<u8>, ProviderError> {
        let (_album_id, asset) = self.resolve_asset(remote_path).await?;

        // GET /api/assets/{id}/original
        let url = self.api_url(&format!("/assets/{}/original", asset.id));
        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(Self::map_api_error(status, &text, "Download asset"));
        }

        // H2: Size-limited download to prevent OOM
        super::response_bytes_with_limit(response, super::MAX_DOWNLOAD_TO_BYTES).await
    }

    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let file_meta = tokio::fs::metadata(local_path)
            .await
            .map_err(|e| ProviderError::Other(format!("Read error: {}", e)))?;
        let total_size = file_meta.len();

        // If remote_path is a directory (ends with / or is a virtual folder),
        // derive filename from the local file path.
        let (folder, parsed_filename) = Self::parse_path(remote_path);
        let filename = if let Some(f) = parsed_filename {
            f.to_string()
        } else {
            // No filename in remote path — derive from local path
            std::path::Path::new(local_path)
                .file_name()
                .map(|f| f.to_string_lossy().to_string())
                .ok_or_else(|| {
                    ProviderError::InvalidPath("Upload path must include a filename".to_string())
                })?
        };

        // Determine target album.
        // Root or virtual folder → auto-create "Album-NN" album.
        // Filename-looking folder (has extension) in root → same auto-create.
        let album_id: Option<String> = match folder {
            None | Some(VIRTUAL_ALL_ASSETS) | Some(VIRTUAL_FAVORITES) => None,
            Some(name) if name.contains('.') && parsed_filename.is_none() => None,
            Some(album_title) => Some(self.resolve_album_id(album_title).await?),
        };

        // Read file bytes
        let content = tokio::fs::read(local_path)
            .await
            .map_err(|e| ProviderError::Other(format!("Read error: {}", e)))?;

        // Guess MIME type from extension
        let mime_type = mime_guess::from_path(&filename)
            .first_or_octet_stream()
            .to_string();

        // Build timestamps from file metadata
        let now = chrono::Utc::now().to_rfc3339();
        let file_created_at = file_meta
            .created()
            .ok()
            .map(|t| {
                let dt: chrono::DateTime<chrono::Utc> = t.into();
                dt.to_rfc3339()
            })
            .unwrap_or_else(|| now.clone());
        let file_modified_at = file_meta
            .modified()
            .ok()
            .map(|t| {
                let dt: chrono::DateTime<chrono::Utc> = t.into();
                dt.to_rfc3339()
            })
            .unwrap_or_else(|| now.clone());

        // Generate deviceAssetId
        let device_asset_id = Self::device_asset_id(&filename, total_size);

        // Build multipart form
        let file_part = reqwest::multipart::Part::bytes(content)
            .file_name(filename.clone())
            .mime_str(&mime_type)
            .map_err(|e| ProviderError::Other(format!("MIME error: {}", e)))?;

        let form = reqwest::multipart::Form::new()
            .part("assetData", file_part)
            .text("deviceAssetId", device_asset_id)
            .text("deviceId", IMMICH_DEVICE_ID.to_string())
            .text("fileCreatedAt", file_created_at)
            .text("fileModifiedAt", file_modified_at);

        // POST /api/assets
        let upload_url = self.api_url("/assets");
        let response = self
            .client
            .post(&upload_url)
            .multipart(form)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(Self::map_api_error(status, &text, "Upload asset"));
        }

        let upload_result: ImmichUploadResponse = response
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(format!("Parse upload: {}", e)))?;

        // If uploading to a specific album, add the asset to it
        if let Some(ref aid) = album_id {
            let add_url = self.api_url(&format!("/albums/{}/assets", aid));
            let add_body = serde_json::json!({
                "ids": [upload_result.id]
            });

            let add_resp = self
                .client
                .put(&add_url)
                .header("Content-Type", "application/json")
                .body(add_body.to_string())
                .send()
                .await
                .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

            if !add_resp.status().is_success() {
                let status = add_resp.status();
                let text = add_resp.text().await.unwrap_or_default();
                return Err(Self::map_api_error(
                    status,
                    &text,
                    "Add asset to album",
                ));
            }
        }

        if let Some(ref cb) = on_progress {
            cb(total_size, total_size);
        }

        info!(
            "Uploaded {} to {} (asset_id={})",
            local_path, remote_path, upload_result.id
        );
        Ok(())
    }

    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let trimmed = path.trim_matches('/');
        if trimmed.is_empty() {
            return Err(ProviderError::InvalidPath(
                "Album name cannot be empty".to_string(),
            ));
        }

        // Immich albums are flat - no nested albums
        if trimmed.contains('/') {
            return Err(ProviderError::NotSupported(
                "Immich does not support nested albums".to_string(),
            ));
        }

        // Reject virtual folder names
        if trimmed == VIRTUAL_ALL_ASSETS || trimmed == VIRTUAL_FAVORITES {
            return Err(ProviderError::InvalidPath(format!(
                "'{}' is a reserved virtual folder name",
                trimmed
            )));
        }

        let body = serde_json::json!({
            "albumName": trimmed
        });

        let url = self.api_url("/albums");
        let response = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(Self::map_api_error(status, &text, "Create album"));
        }

        let created: ImmichCreateAlbumResponse = response
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(format!("Parse create album: {}", e)))?;

        // Update cache
        self.album_cache
            .insert(trimmed.to_string(), created.id.clone());
        self.album_id_to_name
            .insert(created.id.clone(), trimmed.to_string());

        info!("Created album '{}' (id={})", trimmed, created.id);
        Ok(())
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        let (_album_id, asset) = self.resolve_asset(path).await?;

        // DELETE /api/assets with body { "ids": [...], "force": false }
        let url = self.api_url("/assets");
        let body = serde_json::json!({
            "ids": [asset.id],
            "force": false
        });

        let response = self
            .client
            .delete(&url)
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(Self::map_api_error(status, &text, "Delete asset"));
        }

        info!("Deleted asset {} (moved to trash)", asset.id);
        Ok(())
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let trimmed = path.trim_matches('/');
        if trimmed.is_empty() {
            return Err(ProviderError::InvalidPath(
                "Cannot delete root".to_string(),
            ));
        }

        if trimmed == VIRTUAL_ALL_ASSETS || trimmed == VIRTUAL_FAVORITES {
            return Err(ProviderError::InvalidPath(
                "Cannot delete virtual folders".to_string(),
            ));
        }

        let album_id = self.resolve_album_id(trimmed).await?;

        // DELETE /api/albums/{id} - deletes album but NOT its assets
        let url = self.api_url(&format!("/albums/{}", album_id));
        let response = self
            .client
            .delete(&url)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(Self::map_api_error(status, &text, "Delete album"));
        }

        // Clean cache
        self.album_cache.remove(trimmed);
        self.album_id_to_name.remove(&album_id);

        info!("Deleted album '{}' (id={})", trimmed, album_id);
        Ok(())
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        // Albums don't contain sub-albums, so same as rmdir
        self.rmdir(path).await
    }

    async fn rename(&mut self, _from: &str, _to: &str) -> Result<(), ProviderError> {
        Err(ProviderError::NotSupported(
            "Immich does not support renaming assets".to_string(),
        ))
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        let (folder, filename) = Self::parse_path(path);

        match (folder, filename) {
            // Root
            (None, _) => Ok(RemoteEntry {
                name: "/".to_string(),
                path: "/".to_string(),
                is_dir: true,
                size: 0,
                modified: None,
                permissions: None,
                owner: None,
                group: None,
                is_symlink: false,
                link_target: None,
                mime_type: None,
                metadata: HashMap::new(),
            }),
            // A folder (album or virtual)
            (Some(folder_name), None) => match folder_name {
                VIRTUAL_ALL_ASSETS | VIRTUAL_FAVORITES => Ok(RemoteEntry {
                    name: folder_name.to_string(),
                    path: format!("/{}", folder_name),
                    is_dir: true,
                    size: 0,
                    modified: None,
                    permissions: None,
                    owner: None,
                    group: None,
                    is_symlink: false,
                    link_target: None,
                    mime_type: None,
                    metadata: HashMap::new(),
                }),
                album_title => {
                    let album_id = self.resolve_album_id(album_title).await?;
                    let mut metadata = HashMap::new();
                    metadata.insert("id".to_string(), album_id);
                    Ok(RemoteEntry {
                        name: album_title.to_string(),
                        path: format!("/{}", album_title),
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
                    })
                }
            },
            // A file inside a folder
            (Some(folder_name), Some(_filename)) => {
                let (_album_id, asset) = self.resolve_asset(path).await?;
                let path_prefix = format!("/{}", folder_name);
                Ok(self.asset_to_remote_entry(&asset, &path_prefix))
            }
        }
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
        let url = self.api_url("/server/ping");
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(ProviderError::ConnectionFailed(
                "Ping failed".to_string(),
            ));
        }
        Ok(())
    }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        Ok(format!(
            "Immich v{}",
            self.server_version.as_deref().unwrap_or("unknown")
        ))
    }

    fn supports_thumbnails(&self) -> bool {
        true
    }

    async fn get_thumbnail(&mut self, path: &str) -> Result<String, ProviderError> {
        let (_album_id, asset) = self.resolve_asset(path).await?;

        // GET /api/assets/{id}/thumbnail?size=thumbnail
        let url = self.api_url(&format!(
            "/assets/{}/thumbnail?size=thumbnail",
            asset.id
        ));

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(Self::map_api_error(status, &text, "Get thumbnail"));
        }

        let bytes = response
            .bytes()
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        Ok(format!("data:image/jpeg;base64,{}", b64))
    }

    fn supports_share_links(&self) -> bool {
        true
    }

    fn share_link_capabilities(&self) -> ShareLinkCapabilities {
        ShareLinkCapabilities {
            supports_expiration: true,
            supports_password: true,
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
        let (folder, filename) = Self::parse_path(path);

        // Build share link body depending on whether it's an album or an asset
        let body = match (folder, filename) {
            // Sharing an album
            (Some(folder_name), None) => {
                if folder_name == VIRTUAL_ALL_ASSETS || folder_name == VIRTUAL_FAVORITES {
                    return Err(ProviderError::InvalidPath(
                        "Cannot share virtual folders".to_string(),
                    ));
                }
                let album_id = self.resolve_album_id(folder_name).await?;
                let mut obj = serde_json::json!({
                    "type": "ALBUM",
                    "albumId": album_id,
                    "allowDownload": true,
                    "showMetadata": true
                });
                if let Some(ref pw) = options.password {
                    obj["password"] = serde_json::Value::String(pw.clone());
                }
                if let Some(secs) = options.expires_in_secs {
                    let expires_at = chrono::Utc::now()
                        + chrono::Duration::seconds(secs as i64);
                    obj["expiresAt"] =
                        serde_json::Value::String(expires_at.to_rfc3339());
                }
                obj
            }
            // Sharing an individual asset
            (Some(_folder_name), Some(_fname)) => {
                let (_album_id, asset) = self.resolve_asset(path).await?;
                let mut obj = serde_json::json!({
                    "type": "INDIVIDUAL",
                    "assetIds": [asset.id],
                    "allowDownload": true,
                    "showMetadata": true
                });
                if let Some(ref pw) = options.password {
                    obj["password"] = serde_json::Value::String(pw.clone());
                }
                if let Some(secs) = options.expires_in_secs {
                    let expires_at = chrono::Utc::now()
                        + chrono::Duration::seconds(secs as i64);
                    obj["expiresAt"] =
                        serde_json::Value::String(expires_at.to_rfc3339());
                }
                obj
            }
            (None, _) => {
                return Err(ProviderError::InvalidPath(
                    "Cannot share root directory".to_string(),
                ));
            }
        };

        let url = self.api_url("/shared-links");
        let response = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(Self::map_api_error(status, &text, "Create share link"));
        }

        let shared: ImmichSharedLink = response
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(format!("Parse share link: {}", e)))?;

        let key = shared.key.unwrap_or_else(|| {
            shared.id.unwrap_or_else(|| "unknown".to_string())
        });

        let share_url = format!("{}/share/{}", self.config.base_url, key);

        Ok(ShareLinkResult {
            url: share_url,
            password: options.password,
            expires_at: options.expires_in_secs.map(|secs| {
                let expires_at =
                    chrono::Utc::now() + chrono::Duration::seconds(secs as i64);
                expires_at.to_rfc3339()
            }),
        })
    }

    fn supports_find(&self) -> bool {
        true
    }

    async fn find(
        &mut self,
        path: &str,
        pattern: &str,
    ) -> Result<Vec<RemoteEntry>, ProviderError> {
        // When searching from root, search [All Assets] which contains everything.
        let search_path = if path == "/" || path.is_empty() {
            &format!("/{}", VIRTUAL_ALL_ASSETS)
        } else {
            path
        };
        let all_entries = self.list(search_path).await?;

        let entries: Vec<RemoteEntry> = all_entries
            .into_iter()
            .filter(|e| !e.is_dir)
            .filter(|e| super::matches_find_pattern(&e.name, pattern))
            .collect();

        info!("Find '{}' in {}: {} results", pattern, path, entries.len());
        Ok(entries)
    }

    fn supports_checksum(&self) -> bool {
        true
    }

    async fn checksum(
        &mut self,
        path: &str,
    ) -> Result<HashMap<String, String>, ProviderError> {
        let (_album_id, asset) = self.resolve_asset(path).await?;
        let mut result = HashMap::new();
        if let Some(ref checksum) = asset.checksum {
            result.insert("sha1".to_string(), checksum.clone());
        }
        Ok(result)
    }
}
