//! Google Photos Storage Provider
//!
//! Implements StorageProvider for Google Photos using the Photos Library API v1.
//! Uses OAuth2 for authentication (same Google Cloud app as Google Drive,
//! different scopes).
//!
//! Limitations:
//! - No delete (Google Photos API does not allow deleting media)
//! - No rename
//! - Upload creates new media only (cannot overwrite)
//! - baseUrl for downloads expires after ~60 minutes (auto-refreshed)

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

use async_trait::async_trait;
use reqwest::header::{HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::Deserialize;
use std::collections::HashMap;
use std::time::Instant;
use tracing::info;

use super::{
    oauth2::{OAuth2Manager, OAuthConfig, OAuthProvider},
    sanitize_api_error, ProviderConfig, ProviderError, ProviderType, RemoteEntry,
    StorageProvider, AEROFTP_USER_AGENT,
};

const PHOTOS_API_BASE: &str = "https://photoslibrary.googleapis.com/v1";
/// baseUrl is valid for ~60 minutes - refresh before download
const BASE_URL_TTL_SECS: u64 = 3300; // 55 min (5 min safety buffer)

// ---------------------------------------------------------------------------
// API response structs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PhotosAlbum {
    id: String,
    title: Option<String>,
    #[serde(default)]
    media_items_count: Option<String>,
    #[allow(dead_code)]
    cover_photo_base_url: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AlbumList {
    albums: Option<Vec<PhotosAlbum>>,
    next_page_token: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MediaItem {
    id: String,
    filename: Option<String>,
    mime_type: Option<String>,
    base_url: Option<String>,
    #[serde(default)]
    media_metadata: Option<MediaMetadata>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MediaMetadata {
    creation_time: Option<String>,
    width: Option<String>,
    height: Option<String>,
    #[allow(dead_code)]
    photo: Option<PhotoMetadata>,
    video: Option<VideoMetadata>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct PhotoMetadata {
    camera_make: Option<String>,
    camera_model: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct VideoMetadata {
    status: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MediaItemList {
    media_items: Option<Vec<MediaItem>>,
    next_page_token: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchMediaResponse {
    media_items: Option<Vec<MediaItem>>,
    next_page_token: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BatchCreateResponse {
    new_media_item_results: Option<Vec<NewMediaItemResult>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct NewMediaItemResult {
    upload_token: Option<String>,
    status: Option<BatchStatus>,
    media_item: Option<MediaItem>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct BatchStatus {
    message: Option<String>,
    code: Option<i32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateAlbumResponse {
    id: String,
    #[allow(dead_code)]
    title: Option<String>,
}

// ---------------------------------------------------------------------------
// Config & Provider structs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct GooglePhotosConfig {
    pub client_id: String,
    pub client_secret: String,
}

impl GooglePhotosConfig {
    pub fn new(client_id: &str, client_secret: &str) -> Self {
        Self {
            client_id: client_id.to_string(),
            client_secret: client_secret.to_string(),
        }
    }

    #[allow(dead_code)]
    pub fn from_provider_config(config: &ProviderConfig) -> Result<Self, ProviderError> {
        let client_id = config
            .extra
            .get("client_id")
            .ok_or_else(|| ProviderError::Other("Missing client_id".to_string()))?;
        let client_secret = config
            .extra
            .get("client_secret")
            .ok_or_else(|| ProviderError::Other("Missing client_secret".to_string()))?;
        Ok(Self::new(client_id, client_secret))
    }
}

/// Virtual folder names rendered at the root level alongside real albums.
const VIRTUAL_ALL_PHOTOS: &str = "[All Photos]";
const VIRTUAL_FAVORITES: &str = "[Favorites]";

pub struct GooglePhotosProvider {
    config: GooglePhotosConfig,
    oauth_manager: OAuth2Manager,
    client: reqwest::Client,
    connected: bool,
    current_path: String,
    /// Album ID cache: album_title -> album_id
    album_cache: HashMap<String, String>,
    /// Media item baseUrl cache: media_id -> (base_url, fetched_at)
    base_url_cache: HashMap<String, (String, Instant)>,
    account_email: Option<String>,
}

impl GooglePhotosProvider {
    pub fn new(config: GooglePhotosConfig) -> Self {
        Self {
            config,
            oauth_manager: OAuth2Manager::new(),
            client: reqwest::Client::builder()
                .user_agent(AEROFTP_USER_AGENT)
                .connect_timeout(std::time::Duration::from_secs(30))
                .read_timeout(std::time::Duration::from_secs(300))
                .build()
                .unwrap_or_default(),
            connected: false,
            current_path: "/".to_string(),
            album_cache: HashMap::new(),
            base_url_cache: HashMap::new(),
            account_email: None,
        }
    }

    /// Build OAuth2 config for Google Photos (separate scopes from Drive).
    fn oauth_config(&self) -> OAuthConfig {
        OAuthConfig {
            provider: OAuthProvider::GooglePhotos,
            client_id: self.config.client_id.clone(),
            client_secret: Some(self.config.client_secret.clone()),
            auth_url: "https://accounts.google.com/o/oauth2/v2/auth".to_string(),
            token_url: "https://oauth2.googleapis.com/token".to_string(),
            scopes: vec![
                "https://www.googleapis.com/auth/photoslibrary.readonly".to_string(),
                "https://www.googleapis.com/auth/photoslibrary.appendonly".to_string(),
            ],
            redirect_uri: "http://127.0.0.1:0/callback".to_string(),
            extra_auth_params: vec![("access_type".to_string(), "offline".to_string())],
        }
    }

    /// Get Authorization header from the current OAuth2 token.
    async fn auth_header(&self) -> Result<HeaderValue, ProviderError> {
        use secrecy::ExposeSecret;
        let token = self
            .oauth_manager
            .get_valid_token(&self.oauth_config())
            .await?;
        HeaderValue::from_str(&format!("Bearer {}", token.expose_secret()))
            .map_err(|e| ProviderError::Other(format!("Invalid token: {}", e)))
    }

    /// Check if authenticated.
    pub fn is_authenticated(&self) -> bool {
        self.oauth_manager.has_tokens(OAuthProvider::GooglePhotos)
    }

    /// Start OAuth flow - returns (auth_url, state).
    #[allow(dead_code)]
    pub async fn start_auth(&self) -> Result<(String, String), ProviderError> {
        self.oauth_manager
            .start_auth_flow(&self.oauth_config())
            .await
    }

    /// Complete OAuth flow with code.
    #[allow(dead_code)]
    pub async fn complete_auth(&self, code: &str, state: &str) -> Result<(), ProviderError> {
        self.oauth_manager
            .complete_auth_flow(&self.oauth_config(), code, state)
            .await?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // API helpers
    // -----------------------------------------------------------------------

    /// List all albums (paginated).
    async fn list_albums(&self) -> Result<Vec<PhotosAlbum>, ProviderError> {
        let mut all_albums = Vec::new();
        let mut page_token: Option<String> = None;

        loop {
            let mut url = format!("{}/albums?pageSize=50", PHOTOS_API_BASE);
            if let Some(ref token) = page_token {
                url.push_str(&format!("&pageToken={}", token));
            }

            let response = self
                .client
                .get(&url)
                .header(AUTHORIZATION, self.auth_header().await?)
                .send()
                .await
                .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

            if !response.status().is_success() {
                let status = response.status();
                let text = response.text().await.unwrap_or_default();
                return Err(ProviderError::Other(format!(
                    "Albums list failed {}: {}",
                    status,
                    sanitize_api_error(&text)
                )));
            }

            let list: AlbumList = response
                .json()
                .await
                .map_err(|e| ProviderError::Other(format!("Parse error: {}", e)))?;

            if let Some(albums) = list.albums {
                all_albums.extend(albums);
            }

            page_token = list.next_page_token;
            if page_token.is_none() {
                break;
            }
        }

        Ok(all_albums)
    }

    /// List media items inside an album (paginated).
    async fn list_media_in_album(
        &self,
        album_id: &str,
    ) -> Result<Vec<MediaItem>, ProviderError> {
        let mut all_items = Vec::new();
        let mut page_token: Option<String> = None;

        loop {
            let mut body = serde_json::json!({
                "albumId": album_id,
                "pageSize": 100
            });
            if let Some(ref token) = page_token {
                body["pageToken"] = serde_json::Value::String(token.clone());
            }

            let url = format!("{}/mediaItems:search", PHOTOS_API_BASE);
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
                let text = response.text().await.unwrap_or_default();
                return Err(ProviderError::Other(format!(
                    "Search album media failed {}: {}",
                    status,
                    sanitize_api_error(&text)
                )));
            }

            let resp: SearchMediaResponse = response
                .json()
                .await
                .map_err(|e| ProviderError::Other(format!("Parse error: {}", e)))?;

            if let Some(items) = resp.media_items {
                all_items.extend(items);
            }

            page_token = resp.next_page_token;
            if page_token.is_none() {
                break;
            }
        }

        Ok(all_items)
    }

    /// List all media items in the library (paginated).
    async fn list_all_media(&self) -> Result<Vec<MediaItem>, ProviderError> {
        let mut all_items = Vec::new();
        let mut page_token: Option<String> = None;

        loop {
            let mut url = format!("{}/mediaItems?pageSize=100", PHOTOS_API_BASE);
            if let Some(ref token) = page_token {
                url.push_str(&format!("&pageToken={}", token));
            }

            let response = self
                .client
                .get(&url)
                .header(AUTHORIZATION, self.auth_header().await?)
                .send()
                .await
                .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

            if !response.status().is_success() {
                let status = response.status();
                let text = response.text().await.unwrap_or_default();
                return Err(ProviderError::Other(format!(
                    "List all media failed {}: {}",
                    status,
                    sanitize_api_error(&text)
                )));
            }

            let resp: MediaItemList = response
                .json()
                .await
                .map_err(|e| ProviderError::Other(format!("Parse error: {}", e)))?;

            if let Some(items) = resp.media_items {
                all_items.extend(items);
            }

            page_token = resp.next_page_token;
            if page_token.is_none() {
                break;
            }
        }

        Ok(all_items)
    }

    /// List media items marked as favorites.
    async fn list_favorites(&self) -> Result<Vec<MediaItem>, ProviderError> {
        let mut all_items = Vec::new();
        let mut page_token: Option<String> = None;

        loop {
            let mut body = serde_json::json!({
                "pageSize": 100,
                "filters": {
                    "featureFilter": {
                        "includedFeatures": ["FAVORITES"]
                    }
                }
            });
            if let Some(ref token) = page_token {
                body["pageToken"] = serde_json::Value::String(token.clone());
            }

            let url = format!("{}/mediaItems:search", PHOTOS_API_BASE);
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
                let text = response.text().await.unwrap_or_default();
                return Err(ProviderError::Other(format!(
                    "List favorites failed {}: {}",
                    status,
                    sanitize_api_error(&text)
                )));
            }

            let resp: SearchMediaResponse = response
                .json()
                .await
                .map_err(|e| ProviderError::Other(format!("Parse error: {}", e)))?;

            if let Some(items) = resp.media_items {
                all_items.extend(items);
            }

            page_token = resp.next_page_token;
            if page_token.is_none() {
                break;
            }
        }

        Ok(all_items)
    }

    /// Convert a `MediaItem` into a `RemoteEntry`, caching the baseUrl.
    fn media_to_remote_entry(
        &mut self,
        item: &MediaItem,
        path_prefix: &str,
    ) -> RemoteEntry {
        let filename = item
            .filename
            .clone()
            .unwrap_or_else(|| format!("media_{}", item.id));
        let size = 0u64; // Google Photos API does not expose file size directly
        let modified = item
            .media_metadata
            .as_ref()
            .and_then(|m| m.creation_time.clone());
        let mime = item.mime_type.clone();

        // Cache the baseUrl for later download
        if let Some(ref base_url) = item.base_url {
            self.base_url_cache
                .insert(item.id.clone(), (base_url.clone(), Instant::now()));
        }

        let path = format!(
            "{}/{}",
            path_prefix.trim_end_matches('/'),
            filename
        );

        let mut metadata = HashMap::new();
        metadata.insert("id".to_string(), item.id.clone());
        if let Some(ref m) = item.media_metadata {
            if let Some(ref w) = m.width {
                metadata.insert("width".to_string(), w.clone());
            }
            if let Some(ref h) = m.height {
                metadata.insert("height".to_string(), h.clone());
            }
            if m.video.is_some() {
                metadata.insert("isVideo".to_string(), "true".to_string());
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
    ///   "/MyAlbum" -> (Some("MyAlbum"), None)
    ///   "/MyAlbum/photo.jpg" -> (Some("MyAlbum"), Some("photo.jpg"))
    ///   "/[All Photos]/photo.jpg" -> (Some("[All Photos]"), Some("photo.jpg"))
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

    /// Resolve an album title to its ID.
    /// Uses `album_cache` first; falls back to listing albums from the API.
    async fn resolve_album_id(&mut self, title: &str) -> Result<String, ProviderError> {
        if let Some(id) = self.album_cache.get(title) {
            return Ok(id.clone());
        }

        // Refresh cache from API
        let albums = self.list_albums().await?;
        for album in &albums {
            if let Some(ref t) = album.title {
                self.album_cache.insert(t.clone(), album.id.clone());
            }
        }

        self.album_cache
            .get(title)
            .cloned()
            .ok_or_else(|| ProviderError::NotFound(format!("Album not found: {}", title)))
    }

    /// Find a media item by filename inside an album / virtual folder.
    async fn resolve_media_item(
        &mut self,
        folder: &str,
        filename: &str,
    ) -> Result<MediaItem, ProviderError> {
        let items = match folder {
            VIRTUAL_ALL_PHOTOS => self.list_all_media().await?,
            VIRTUAL_FAVORITES => self.list_favorites().await?,
            album_title => {
                let album_id = self.resolve_album_id(album_title).await?;
                self.list_media_in_album(&album_id).await?
            }
        };

        items
            .into_iter()
            .find(|it| {
                it.filename
                    .as_deref()
                    .map(|f| f == filename)
                    .unwrap_or(false)
            })
            .ok_or_else(|| {
                ProviderError::NotFound(format!("{}/{}", folder, filename))
            })
    }

    /// Get a fresh (non-expired) `baseUrl` for a media item, re-fetching from
    /// the API when the cached value has exceeded `BASE_URL_TTL_SECS`.
    async fn get_fresh_base_url(
        &mut self,
        media_id: &str,
    ) -> Result<String, ProviderError> {
        // Check cache
        if let Some((url, fetched_at)) = self.base_url_cache.get(media_id) {
            if fetched_at.elapsed().as_secs() < BASE_URL_TTL_SECS {
                return Ok(url.clone());
            }
        }

        // Re-fetch from the API
        let url = format!("{}/mediaItems/{}", PHOTOS_API_BASE, media_id);
        let response = self
            .client
            .get(&url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!(
                "Fetch media item failed {}: {}",
                status,
                sanitize_api_error(&text)
            )));
        }

        let item: MediaItem = response
            .json()
            .await
            .map_err(|e| ProviderError::Other(format!("Parse error: {}", e)))?;

        let base_url = item
            .base_url
            .ok_or_else(|| ProviderError::Other("Media item has no baseUrl".to_string()))?;

        self.base_url_cache
            .insert(media_id.to_string(), (base_url.clone(), Instant::now()));

        Ok(base_url)
    }

    /// Determine whether a media item is a video (from cached metadata or MIME).
    fn is_video_item(item: &MediaItem) -> bool {
        if let Some(ref meta) = item.media_metadata {
            if meta.video.is_some() {
                return true;
            }
        }
        item.mime_type
            .as_deref()
            .map(|m| m.starts_with("video/"))
            .unwrap_or(false)
    }

    /// Build the download URL from a baseUrl.
    /// Photos: `{baseUrl}=d`  (original quality)
    /// Videos: `{baseUrl}=dv` (original quality)
    fn download_url(base_url: &str, is_video: bool) -> String {
        if is_video {
            format!("{}=dv", base_url)
        } else {
            format!("{}=d", base_url)
        }
    }

    /// MIME types accepted by Google Photos for upload.
    pub fn accepted_upload_types() -> Vec<String> {
        vec![
            "image/jpeg".into(),
            "image/png".into(),
            "image/gif".into(),
            "image/webp".into(),
            "image/heic".into(),
            "image/heif".into(),
            "image/bmp".into(),
            "image/tiff".into(),
            "image/x-icon".into(),
            "video/mp4".into(),
            "video/quicktime".into(),
            "video/x-msvideo".into(),
            "video/mpeg".into(),
            "video/3gpp".into(),
            "video/3gpp2".into(),
            "video/x-matroska".into(),
            "video/webm".into(),
            "image/x-nikon-nef".into(),
            "image/x-canon-cr2".into(),
            "image/x-sony-arw".into(),
            "image/x-adobe-dng".into(),
        ]
    }
}

// ---------------------------------------------------------------------------
// StorageProvider trait implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl StorageProvider for GooglePhotosProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn provider_type(&self) -> ProviderType {
        ProviderType::GooglePhotos
    }

    fn display_name(&self) -> String {
        "Google Photos".to_string()
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

        // Validate token by listing albums (lightweight call)
        let albums = self.list_albums().await?;
        for album in &albums {
            if let Some(ref title) = album.title {
                self.album_cache.insert(title.clone(), album.id.clone());
            }
        }

        self.connected = true;
        self.current_path = "/".to_string();

        // Fetch user email from Google userinfo endpoint
        if let Ok(auth) = self.auth_header().await {
            if let Ok(resp) = self
                .client
                .get("https://www.googleapis.com/userinfo/v2/me")
                .header(AUTHORIZATION, auth)
                .send()
                .await
            {
                if let Ok(body) = resp.json::<serde_json::Value>().await {
                    if let Some(email) = body["email"].as_str() {
                        self.account_email = Some(email.to_string());
                    }
                }
            }
        }

        info!(
            "Connected to Google Photos ({} albums)",
            self.album_cache.len()
        );
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.connected = false;
        self.album_cache.clear();
        self.base_url_cache.clear();
        self.account_email = None;
        info!("Disconnected from Google Photos");
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
                    name: VIRTUAL_ALL_PHOTOS.to_string(),
                    path: format!("/{}", VIRTUAL_ALL_PHOTOS),
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
                    let title = album
                        .title
                        .clone()
                        .unwrap_or_else(|| format!("album_{}", album.id));

                    // Refresh cache
                    self.album_cache.insert(title.clone(), album.id.clone());

                    let count = album
                        .media_items_count
                        .as_deref()
                        .unwrap_or("0")
                        .to_string();

                    let mut metadata = HashMap::new();
                    metadata.insert("id".to_string(), album.id.clone());
                    metadata.insert("mediaItemsCount".to_string(), count);

                    entries.push(RemoteEntry {
                        name: title.clone(),
                        path: format!("/{}", title),
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

                info!("Listed root: {} entries", entries.len());
                Ok(entries)
            }
            // Inside a folder (album or virtual)
            Some(folder_name) => {
                let path_prefix = format!("/{}", folder_name);
                let items = match folder_name {
                    VIRTUAL_ALL_PHOTOS => self.list_all_media().await?,
                    VIRTUAL_FAVORITES => self.list_favorites().await?,
                    album_title => {
                        let album_id = self.resolve_album_id(album_title).await?;
                        self.list_media_in_album(&album_id).await?
                    }
                };

                let entries: Vec<RemoteEntry> = items
                    .iter()
                    .map(|item| self.media_to_remote_entry(item, &path_prefix))
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

        // Validate the path exists by attempting to list it
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
                // Verify the folder exists
                match folder_name {
                    VIRTUAL_ALL_PHOTOS | VIRTUAL_FAVORITES => { /* always valid */ }
                    album_title => {
                        // Will return NotFound if album doesn't exist
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
        _on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        use futures_util::StreamExt;

        let (folder, filename) = Self::parse_path(remote_path);
        let folder_name = folder.ok_or_else(|| {
            ProviderError::InvalidPath("Cannot download root directory".to_string())
        })?;
        let filename = filename.ok_or_else(|| {
            ProviderError::InvalidPath("Path must point to a media file, not a folder".to_string())
        })?;

        let item = self
            .resolve_media_item(folder_name, filename)
            .await?;

        let is_video = Self::is_video_item(&item);
        let media_id = item.id.clone();

        // Cache baseUrl from the resolved item
        if let Some(ref base_url) = item.base_url {
            self.base_url_cache
                .insert(media_id.clone(), (base_url.clone(), Instant::now()));
        }

        let base_url = self.get_fresh_base_url(&media_id).await?;
        let download_url = Self::download_url(&base_url, is_video);

        let response = self
            .client
            .get(&download_url)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!(
                "Download failed {}: {}",
                status,
                sanitize_api_error(&text)
            )));
        }

        let mut stream = response.bytes_stream();
        let mut atomic = super::atomic_write::AtomicFile::new(local_path)
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        while let Some(chunk) = stream.next().await {
            let chunk =
                chunk.map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
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

    async fn download_to_bytes(
        &mut self,
        remote_path: &str,
    ) -> Result<Vec<u8>, ProviderError> {
        let (folder, filename) = Self::parse_path(remote_path);
        let folder_name = folder.ok_or_else(|| {
            ProviderError::InvalidPath("Cannot download root directory".to_string())
        })?;
        let filename = filename.ok_or_else(|| {
            ProviderError::InvalidPath("Path must point to a media file, not a folder".to_string())
        })?;

        let item = self
            .resolve_media_item(folder_name, filename)
            .await?;

        let is_video = Self::is_video_item(&item);
        let media_id = item.id.clone();

        if let Some(ref base_url) = item.base_url {
            self.base_url_cache
                .insert(media_id.clone(), (base_url.clone(), Instant::now()));
        }

        let base_url = self.get_fresh_base_url(&media_id).await?;
        let download_url = Self::download_url(&base_url, is_video);

        let response = self
            .client
            .get(&download_url)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!(
                "Download failed {}: {}",
                status,
                sanitize_api_error(&text)
            )));
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
        let total_size = tokio::fs::metadata(local_path)
            .await
            .map_err(|e| ProviderError::Other(format!("Read error: {}", e)))?
            .len();

        let (folder, filename) = Self::parse_path(remote_path);
        let filename = filename
            .or(folder)
            .ok_or_else(|| {
                ProviderError::InvalidPath("Upload path must include a filename".to_string())
            })?
            .to_string();

        // Determine target album (if uploading into a real album)
        let album_id: Option<String> = match folder {
            None => None,
            Some(VIRTUAL_ALL_PHOTOS) | Some(VIRTUAL_FAVORITES) => None,
            Some(album_title) => Some(self.resolve_album_id(album_title).await?),
        };

        // Guess MIME type from extension
        let mime_type = mime_guess::from_path(&filename)
            .first_or_octet_stream()
            .to_string();

        // Step 1: Upload raw bytes to get an upload token
        let content = tokio::fs::read(local_path)
            .await
            .map_err(|e| ProviderError::Other(format!("Read error: {}", e)))?;

        let upload_url = format!("{}/uploads", PHOTOS_API_BASE);
        let upload_response = self
            .client
            .post(&upload_url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header(CONTENT_TYPE, "application/octet-stream")
            .header("X-Goog-Upload-Content-Type", &mime_type)
            .header("X-Goog-Upload-Protocol", "raw")
            .body(content)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !upload_response.status().is_success() {
            let status = upload_response.status();
            let text = upload_response.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!(
                "Upload bytes failed {}: {}",
                status,
                sanitize_api_error(&text)
            )));
        }

        let upload_token = upload_response
            .text()
            .await
            .map_err(|e| ProviderError::Other(format!("Read upload token: {}", e)))?;

        // Step 2: Create media item via batchCreate
        let mut batch_body = serde_json::json!({
            "newMediaItems": [{
                "simpleMediaItem": {
                    "uploadToken": upload_token,
                    "fileName": filename
                }
            }]
        });

        if let Some(ref aid) = album_id {
            batch_body["albumId"] = serde_json::Value::String(aid.clone());
        }

        let batch_url = format!("{}/mediaItems:batchCreate", PHOTOS_API_BASE);
        let batch_response = self
            .client
            .post(&batch_url)
            .header(AUTHORIZATION, self.auth_header().await?)
            .header(CONTENT_TYPE, "application/json")
            .body(batch_body.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !batch_response.status().is_success() {
            let status = batch_response.status();
            let text = batch_response.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!(
                "batchCreate failed {}: {}",
                status,
                sanitize_api_error(&text)
            )));
        }

        let batch_result: BatchCreateResponse = batch_response
            .json()
            .await
            .map_err(|e| ProviderError::Other(format!("Parse batchCreate: {}", e)))?;

        // Check individual item status
        if let Some(results) = &batch_result.new_media_item_results {
            for result in results {
                if let Some(ref st) = result.status {
                    if let Some(code) = st.code {
                        if code != 0 {
                            let msg = st
                                .message
                                .as_deref()
                                .unwrap_or("unknown error");
                            return Err(ProviderError::Other(format!(
                                "Media item creation failed (code {}): {}",
                                code, msg
                            )));
                        }
                    }
                }
            }
        }

        if let Some(ref cb) = on_progress {
            cb(total_size, total_size);
        }

        info!("Uploaded {} to {}", local_path, remote_path);
        Ok(())
    }

    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let trimmed = path.trim_matches('/');
        if trimmed.is_empty() {
            return Err(ProviderError::InvalidPath(
                "Album name cannot be empty".to_string(),
            ));
        }

        // Google Photos albums are flat - no nested albums
        if trimmed.contains('/') {
            return Err(ProviderError::NotSupported(
                "Google Photos does not support nested albums".to_string(),
            ));
        }

        // Reject virtual folder names
        if trimmed == VIRTUAL_ALL_PHOTOS || trimmed == VIRTUAL_FAVORITES {
            return Err(ProviderError::InvalidPath(format!(
                "'{}' is a reserved virtual folder name",
                trimmed
            )));
        }

        let body = serde_json::json!({
            "album": {
                "title": trimmed
            }
        });

        let url = format!("{}/albums", PHOTOS_API_BASE);
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
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Other(format!(
                "Create album failed {}: {}",
                status,
                sanitize_api_error(&text)
            )));
        }

        let created: CreateAlbumResponse = response
            .json()
            .await
            .map_err(|e| ProviderError::Other(format!("Parse error: {}", e)))?;

        // Update cache
        self.album_cache
            .insert(trimmed.to_string(), created.id.clone());

        info!("Created album '{}' (id={})", trimmed, created.id);
        Ok(())
    }

    async fn delete(&mut self, _path: &str) -> Result<(), ProviderError> {
        Err(ProviderError::NotSupported(
            "Google Photos API does not allow deleting media items".to_string(),
        ))
    }

    async fn rmdir(&mut self, _path: &str) -> Result<(), ProviderError> {
        Err(ProviderError::NotSupported(
            "Google Photos API does not allow deleting albums via the Library API".to_string(),
        ))
    }

    async fn rmdir_recursive(&mut self, _path: &str) -> Result<(), ProviderError> {
        Err(ProviderError::NotSupported(
            "Google Photos API does not allow deleting albums via the Library API".to_string(),
        ))
    }

    async fn rename(&mut self, _from: &str, _to: &str) -> Result<(), ProviderError> {
        Err(ProviderError::NotSupported(
            "Google Photos API does not support renaming media items".to_string(),
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
            (Some(folder_name), None) => {
                match folder_name {
                    VIRTUAL_ALL_PHOTOS | VIRTUAL_FAVORITES => Ok(RemoteEntry {
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
                }
            }
            // A file inside a folder
            (Some(folder_name), Some(fname)) => {
                let item = self.resolve_media_item(folder_name, fname).await?;
                let path_prefix = format!("/{}", folder_name);
                Ok(self.media_to_remote_entry(&item, &path_prefix))
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
        // No-op for stateless HTTP REST API
        Ok(())
    }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        Ok("Google Photos Library API v1".to_string())
    }

    fn supports_thumbnails(&self) -> bool {
        true
    }

    async fn get_thumbnail(&mut self, path: &str) -> Result<String, ProviderError> {
        let (folder, filename) = Self::parse_path(path);
        let folder_name = folder.ok_or_else(|| {
            ProviderError::InvalidPath("Cannot get thumbnail for root".to_string())
        })?;
        let filename = filename.ok_or_else(|| {
            ProviderError::InvalidPath("Path must point to a media file".to_string())
        })?;

        let item = self.resolve_media_item(folder_name, filename).await?;
        let media_id = item.id.clone();

        if let Some(ref base_url) = item.base_url {
            self.base_url_cache
                .insert(media_id.clone(), (base_url.clone(), Instant::now()));
        }

        let base_url = self.get_fresh_base_url(&media_id).await?;

        // Thumbnail: crop to 256x256
        Ok(format!("{}=w256-h256-c", base_url))
    }
}
