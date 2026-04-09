//! OpenStack Swift provider (Blomp, OVH, Rackspace)
//!
//! TempAuth v1 authentication + Swift Object Store REST API.
//! Blomp-specific constraints: single container, segments in .file-segments/.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

use async_trait::async_trait;
use log::{debug, info, warn};
use reqwest::{Client, Method, StatusCode};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use std::time::{Duration, Instant};

use super::{
    http_retry::{send_with_retry, HttpRetryConfig},
    ProviderError, ProviderType, RemoteEntry, StorageInfo, StorageProvider,
};

// ─── Configuration ─────────────────────────────────────────────────

/// OpenStack Swift configuration (extracted from ProviderConfig)
pub struct SwiftConfig {
    pub auth_url: String,
    pub username: String,
    pub password: SecretString,
    pub verify_cert: bool,
}

impl SwiftConfig {
    pub fn from_provider_config(config: &super::ProviderConfig) -> Result<Self, ProviderError> {
        let auth_url = if config.host.starts_with("http") {
            config.host.clone()
        } else {
            format!("https://{}", config.host)
        };
        Ok(Self {
            auth_url,
            username: config.username.clone().unwrap_or_default(),
            password: SecretString::from(config.password.clone().unwrap_or_default()),
            verify_cert: config
                .extra
                .get("verify_cert")
                .map(|v| v != "false")
                .unwrap_or(true),
        })
    }
}

// ─── Internal types ────────────────────────────────────────────────

/// Detected auth version
#[derive(Debug)]
enum AuthVersion {
    V1, // TempAuth
    V2, // Keystone v2
}

/// Auth state (works for both v1 and v2)
struct SwiftAuth {
    token: SecretString,
    storage_url: String,
    obtained_at: Instant,
}

impl SwiftAuth {
    fn is_valid(&self) -> bool {
        self.obtained_at.elapsed() < Duration::from_secs(23 * 3600)
    }
}

#[derive(Debug, Deserialize)]
struct ContainerEntry {
    name: String,
    #[allow(dead_code)]
    count: u64,
    #[allow(dead_code)]
    bytes: u64,
}

#[derive(Debug, Deserialize)]
struct ObjectEntry {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    bytes: Option<u64>,
    #[serde(default)]
    content_type: Option<String>,
    #[serde(default)]
    hash: Option<String>,
    #[serde(default)]
    last_modified: Option<String>,
    #[serde(default)]
    subdir: Option<String>,
}

#[derive(serde::Serialize)]
struct SloSegment {
    path: String,
    etag: String,
    size_bytes: u64,
}

/// Compute MD5 hex digest for a byte slice
fn md5_hex(data: &[u8]) -> String {
    use md5::{Digest, Md5};
    let hash = Md5::digest(data);
    format!("{:x}", hash)
}

// ─── Provider ──────────────────────────────────────────────────────

pub struct SwiftProvider {
    config: SwiftConfig,
    client: Client,
    auth: Option<SwiftAuth>,
    container: String,
    current_path: String,
    connected: bool,
}

impl SwiftProvider {
    pub fn new(config: SwiftConfig) -> Self {
        let client = Client::builder()
            .user_agent(crate::providers::AEROFTP_USER_AGENT)
            .timeout(Duration::from_secs(300))
            .connect_timeout(Duration::from_secs(30))
            .danger_accept_invalid_certs(!config.verify_cert)
            .build()
            .unwrap_or_else(|_| Client::new());

        Self {
            config,
            client,
            auth: None,
            container: String::new(),
            current_path: "/".to_string(),
            connected: false,
        }
    }

    // ─── Auth ──────────────────────────────────────────────────

    /// Authenticate against OpenStack Swift.
    /// Auto-detects auth version by probing the endpoint:
    ///   - If root JSON contains "v2.0" → Keystone v2 (POST /v2.0/tokens)
    ///   - Otherwise → TempAuth v1 (GET /auth/v1.0)
    async fn authenticate(&mut self) -> Result<(), ProviderError> {
        let base = self.config.auth_url.trim_end_matches('/').to_string();

        // Probe root to detect auth version
        let version = self.detect_auth_version(&base).await;
        debug!("Swift auth version detected: {:?}", version);

        match version {
            AuthVersion::V2 => self.auth_keystone_v2(&base).await,
            AuthVersion::V1 => self.auth_tempauth_v1(&base).await,
        }
    }

    /// Detect auth version from root JSON response
    async fn detect_auth_version(&self, base: &str) -> AuthVersion {
        if let Ok(resp) = self.client.get(base).send().await {
            if let Ok(text) = resp.text().await {
                if text.contains("v2.0") {
                    return AuthVersion::V2;
                }
            }
        }
        AuthVersion::V1
    }

    /// TempAuth v1: GET {base}/auth/v1.0 with X-Auth-User + X-Auth-Key
    async fn auth_tempauth_v1(&mut self, base: &str) -> Result<(), ProviderError> {
        let url = format!("{base}/auth/v1.0");
        debug!("Swift TempAuth v1: {}", url);

        let resp = self
            .client
            .get(&url)
            .header("X-Auth-User", &self.config.username)
            .header("X-Auth-Key", self.config.password.expose_secret())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("Auth request failed: {e}")))?;

        match resp.status() {
            StatusCode::OK | StatusCode::NO_CONTENT => {
                let token = resp
                    .headers()
                    .get("x-auth-token")
                    .or_else(|| resp.headers().get("x-storage-token"))
                    .ok_or_else(|| {
                        ProviderError::AuthenticationFailed("No X-Auth-Token in response".into())
                    })?
                    .to_str()
                    .map_err(|_| {
                        ProviderError::AuthenticationFailed("Invalid token header encoding".into())
                    })?
                    .to_string();

                let storage_url = resp
                    .headers()
                    .get("x-storage-url")
                    .ok_or_else(|| {
                        ProviderError::AuthenticationFailed("No X-Storage-Url in response".into())
                    })?
                    .to_str()
                    .map_err(|_| {
                        ProviderError::AuthenticationFailed(
                            "Invalid storage URL header encoding".into(),
                        )
                    })?
                    .trim_end_matches('/')
                    .to_string();

                info!("Swift TempAuth OK — storage: {}", storage_url);
                self.auth = Some(SwiftAuth {
                    token: SecretString::from(token),
                    storage_url,
                    obtained_at: Instant::now(),
                });
                Ok(())
            }
            StatusCode::UNAUTHORIZED => Err(ProviderError::AuthenticationFailed(
                "Invalid credentials".into(),
            )),
            StatusCode::FORBIDDEN => Err(ProviderError::AuthenticationFailed(
                "Account suspended or forbidden".into(),
            )),
            status => Err(ProviderError::AuthenticationFailed(format!(
                "TempAuth failed: HTTP {status}"
            ))),
        }
    }

    /// Keystone v2: POST {base}/v2.0/tokens
    /// Body: {"auth":{"passwordCredentials":{"username":"...","password":"..."}}}
    /// Response: token in access.token.id, storage URL in access.serviceCatalog
    async fn auth_keystone_v2(&mut self, base: &str) -> Result<(), ProviderError> {
        let url = format!("{base}/v2.0/tokens");
        debug!("Swift Keystone v2: {}", url);

        // Blomp uses tenantName = "storage" (fixed for all accounts).
        // Other Swift providers may use the username or project name.
        let body = serde_json::json!({
            "auth": {
                "passwordCredentials": {
                    "username": self.config.username,
                    "password": self.config.password.expose_secret()
                },
                "tenantName": "storage"
            }
        });

        let resp = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                ProviderError::ConnectionFailed(format!("Keystone v2 request failed: {e}"))
            })?;

        match resp.status() {
            StatusCode::OK => {
                let json: serde_json::Value = resp.json().await.map_err(|e| {
                    ProviderError::AuthenticationFailed(format!("Invalid Keystone response: {e}"))
                })?;

                // Extract token
                let token = json["access"]["token"]["id"]
                    .as_str()
                    .ok_or_else(|| {
                        ProviderError::AuthenticationFailed("No token in Keystone response".into())
                    })?
                    .to_string();

                // Extract storage URL from service catalog
                // Try "object-store" first, fall back to any service with a publicURL
                let catalog = json["access"]["serviceCatalog"].as_array();
                let storage_url_str = catalog
                    .and_then(|cat| {
                        // First try object-store
                        cat.iter()
                            .find(|svc| svc["type"].as_str() == Some("object-store"))
                            .or_else(|| {
                                // Log available types for debugging
                                let types: Vec<&str> =
                                    cat.iter().filter_map(|s| s["type"].as_str()).collect();
                                debug!("Keystone catalog types: {:?}", types);
                                // Fall back to first service with a publicURL
                                cat.iter().find(|svc| {
                                    svc["endpoints"]
                                        .as_array()
                                        .and_then(|eps| eps.first())
                                        .and_then(|ep| ep["publicURL"].as_str())
                                        .is_some()
                                })
                            })
                    })
                    .and_then(|svc| svc["endpoints"].as_array())
                    .and_then(|endpoints| endpoints.first())
                    .and_then(|ep| {
                        ep["publicURL"]
                            .as_str()
                            .or_else(|| ep["internalURL"].as_str())
                    })
                    .ok_or_else(|| {
                        // Dump catalog for debugging
                        let cat_debug = catalog
                            .map(|c| {
                                c.iter()
                                    .map(|s| {
                                        format!(
                                            "type={}, endpoints={}",
                                            s["type"].as_str().unwrap_or("?"),
                                            s["endpoints"]
                                        )
                                    })
                                    .collect::<Vec<_>>()
                                    .join("; ")
                            })
                            .unwrap_or_else(|| "empty catalog".to_string());
                        warn!("Keystone catalog: {}", cat_debug);
                        ProviderError::AuthenticationFailed(
                            "No storage endpoint in Keystone service catalog".into(),
                        )
                    })?;
                let storage_url = storage_url_str.trim_end_matches('/').to_string();

                info!("Swift Keystone v2 OK — storage: {}", storage_url);
                self.auth = Some(SwiftAuth {
                    token: SecretString::from(token),
                    storage_url,
                    obtained_at: Instant::now(),
                });
                Ok(())
            }
            StatusCode::UNAUTHORIZED => Err(ProviderError::AuthenticationFailed(
                "Invalid credentials".into(),
            )),
            StatusCode::FORBIDDEN => Err(ProviderError::AuthenticationFailed(
                "Account suspended or forbidden".into(),
            )),
            status => Err(ProviderError::AuthenticationFailed(format!(
                "Keystone v2 failed: HTTP {status}"
            ))),
        }
    }

    /// Ensure we have a valid token, re-auth if expired
    async fn ensure_auth(&mut self) -> Result<(), ProviderError> {
        if self.auth.as_ref().is_none_or(|a| !a.is_valid()) {
            self.authenticate().await?;
        }
        Ok(())
    }

    fn token(&self) -> Result<&str, ProviderError> {
        self.auth
            .as_ref()
            .map(|a| a.token.expose_secret())
            .ok_or_else(|| ProviderError::AuthenticationFailed("Not authenticated".into()))
    }

    fn storage_url(&self) -> Result<&str, ProviderError> {
        self.auth
            .as_ref()
            .map(|a| a.storage_url.as_str())
            .ok_or_else(|| ProviderError::AuthenticationFailed("Not authenticated".into()))
    }

    // ─── Container discovery ───────────────────────────────────

    /// GET {storage_url}?format=json -> first container name.
    /// Blomp has exactly one container per account.
    async fn discover_container(&mut self) -> Result<String, ProviderError> {
        let url = format!("{}?format=json", self.storage_url()?);
        debug!("Swift container discovery: {}", url);

        let resp = self.swift_request(Method::GET, &url, None, &[]).await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Container list failed: HTTP {status} — {}",
                &body[..body.len().min(200)]
            )));
        }

        let text = resp
            .text()
            .await
            .map_err(|e| ProviderError::ServerError(format!("Read body failed: {e}")))?;
        debug!(
            "Container list (HTTP {}): {}",
            status,
            &text[..text.len().min(300)]
        );

        let containers: Vec<ContainerEntry> = serde_json::from_str(&text).map_err(|e| {
            ProviderError::ServerError(format!(
                "Invalid container JSON: {e} — body: {}",
                &text[..text.len().min(200)]
            ))
        })?;

        containers
            .first()
            .map(|c| {
                info!("Swift using container: {}", c.name);
                c.name.clone()
            })
            .ok_or_else(|| ProviderError::ServerError("No containers found in account".into()))
    }

    // ─── URL helpers ───────────────────────────────────────────

    fn object_url(&self, path: &str) -> Result<String, ProviderError> {
        let storage = self.storage_url()?;
        let clean = path.trim_start_matches('/');
        if clean.is_empty() {
            Ok(format!("{}/{}", storage, self.container))
        } else {
            Ok(format!(
                "{}/{}/{}",
                storage,
                self.container,
                urlencoding::encode(clean).replace("%2F", "/")
            ))
        }
    }

    fn normalize_path(path: &str) -> String {
        path.trim_matches('/').to_string()
    }

    // ─── Request with 401 retry ────────────────────────────────

    async fn swift_request(
        &mut self,
        method: Method,
        url: &str,
        body: Option<Vec<u8>>,
        extra_headers: &[(String, String)],
    ) -> Result<reqwest::Response, ProviderError> {
        self.ensure_auth().await?;

        let mut req = self
            .client
            .request(method.clone(), url)
            .header("X-Auth-Token", self.token()?);
        for (k, v) in extra_headers {
            req = req.header(k.as_str(), v.as_str());
        }
        if let Some(ref data) = body {
            req = req.body(data.clone());
        }

        let request = req
            .build()
            .map_err(|e| ProviderError::NetworkError(format!("Failed to build request: {e}")))?;
        let resp = send_with_retry(&self.client, request, &HttpRetryConfig::default())
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("Request failed: {e}")))?;

        if resp.status() == StatusCode::UNAUTHORIZED {
            // Re-auth and retry once
            self.authenticate().await?;
            let mut req2 = self
                .client
                .request(method, url)
                .header("X-Auth-Token", self.token()?);
            for (k, v) in extra_headers {
                req2 = req2.header(k.as_str(), v.as_str());
            }
            if let Some(data) = body {
                req2 = req2.body(data);
            }
            let request2 = req2
                .build()
                .map_err(|e| ProviderError::NetworkError(format!("Failed to build request: {e}")))?;
            send_with_retry(&self.client, request2, &HttpRetryConfig::default())
                .await
                .map_err(|e| ProviderError::ConnectionFailed(format!("Retry failed: {e}")))
        } else {
            Ok(resp)
        }
    }

    // ─── SLO upload ────────────────────────────────────────────

    /// Upload file >5GB via Static Large Objects.
    /// 1. Split into 1GiB chunks
    /// 2. PUT each to {container}/.file-segments/{object}/{seq:010}
    /// 3. PUT SLO manifest to {container}/{object}?multipart-manifest=put
    async fn upload_slo(
        &mut self,
        local_path: &str,
        remote_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        use tokio::io::AsyncReadExt;

        let file = tokio::fs::File::open(local_path)
            .await
            .map_err(|e| ProviderError::TransferFailed(format!("Open failed: {e}")))?;
        let file_size = file
            .metadata()
            .await
            .map_err(|e| ProviderError::TransferFailed(format!("Metadata failed: {e}")))?
            .len();

        let chunk_size: usize = 1024 * 1024 * 1024; // 1 GiB
        let object_name = Self::normalize_path(remote_path);
        let mut segments: Vec<SloSegment> = Vec::new();
        let mut reader = tokio::io::BufReader::new(file);
        let mut seq: u64 = 1;
        let mut uploaded: u64 = 0;

        loop {
            let mut buf = vec![0u8; chunk_size];
            let mut total_read = 0usize;

            loop {
                let n = reader.read(&mut buf[total_read..]).await.map_err(|e| {
                    ProviderError::TransferFailed(format!("Read chunk failed: {e}"))
                })?;
                if n == 0 {
                    break;
                }
                total_read += n;
                if total_read >= chunk_size {
                    break;
                }
            }

            if total_read == 0 {
                break;
            }
            buf.truncate(total_read);

            let segment_path = format!(".file-segments/{object_name}/{seq:010}");
            let segment_url = self.object_url(&segment_path)?;
            let digest = md5_hex(&buf);
            let segment_size = buf.len() as u64;

            let headers = vec![
                (
                    "Content-Type".to_string(),
                    "application/octet-stream".to_string(),
                ),
                ("ETag".to_string(), digest.clone()),
            ];

            let resp = self
                .swift_request(Method::PUT, &segment_url, Some(buf), &headers)
                .await?;
            if resp.status() != StatusCode::CREATED {
                return Err(ProviderError::ServerError(format!(
                    "Segment {seq} upload failed: HTTP {}",
                    resp.status()
                )));
            }

            let etag = resp
                .headers()
                .get("etag")
                .and_then(|v| v.to_str().ok())
                .map(|v| v.trim_matches('"').to_string())
                .unwrap_or(digest);

            segments.push(SloSegment {
                path: format!("/{}/{}", self.container, segment_path),
                etag,
                size_bytes: segment_size,
            });

            uploaded += segment_size;
            if let Some(ref cb) = on_progress {
                cb(uploaded, file_size);
            }
            seq += 1;
        }

        // PUT SLO manifest
        let manifest_url = format!("{}?multipart-manifest=put", self.object_url(&object_name)?);
        let manifest_json = serde_json::to_vec(&segments)
            .map_err(|e| ProviderError::ServerError(format!("Manifest JSON failed: {e}")))?;

        let headers = vec![("Content-Type".to_string(), "application/json".to_string())];
        let resp = self
            .swift_request(Method::PUT, &manifest_url, Some(manifest_json), &headers)
            .await?;
        if !resp.status().is_success() {
            return Err(ProviderError::ServerError(format!(
                "SLO manifest upload failed: HTTP {}",
                resp.status()
            )));
        }

        info!(
            "SLO upload complete: {} ({} segments)",
            object_name,
            segments.len()
        );
        Ok(())
    }
}

// ─── StorageProvider trait ──────────────────────────────────────────

#[async_trait]
impl StorageProvider for SwiftProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
    fn provider_type(&self) -> ProviderType {
        ProviderType::Swift
    }
    fn display_name(&self) -> String {
        format!("Swift ({})", self.config.username)
    }
    fn account_email(&self) -> Option<String> {
        Some(self.config.username.clone())
    }
    fn is_connected(&self) -> bool {
        self.connected
    }

    /// Connect: authenticate + discover default container
    async fn connect(&mut self) -> Result<(), ProviderError> {
        self.authenticate().await?;
        self.container = self.discover_container().await?;
        self.connected = true;
        info!("Swift connected — container: {}", self.container);
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.auth = None;
        self.connected = false;
        Ok(())
    }

    /// List objects with virtual directory simulation.
    /// GET {storage_url}/{container}?prefix={path}/&delimiter=/&format=json&limit=10000
    /// Paginates via marker parameter.
    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let prefix = Self::normalize_path(path);
        let prefix_query = if prefix.is_empty() {
            String::new()
        } else {
            format!("{prefix}/")
        };

        let mut all_entries = Vec::new();
        let mut marker = String::new();

        loop {
            let base = format!("{}/{}", self.storage_url()?, self.container);
            let mut url = format!("{base}?format=json&delimiter=/&limit=10000");
            if !prefix_query.is_empty() {
                url.push_str(&format!("&prefix={}", urlencoding::encode(&prefix_query)));
            }
            if !marker.is_empty() {
                url.push_str(&format!("&marker={}", urlencoding::encode(&marker)));
            }

            let resp = self.swift_request(Method::GET, &url, None, &[]).await?;
            if !resp.status().is_success() {
                return Err(ProviderError::ServerError(format!(
                    "List failed: HTTP {}",
                    resp.status()
                )));
            }

            let entries: Vec<ObjectEntry> = resp
                .json()
                .await
                .map_err(|e| ProviderError::ServerError(format!("List JSON parse failed: {e}")))?;

            if entries.is_empty() {
                break;
            }

            let last_name = entries
                .last()
                .and_then(|e| e.name.as_ref().or(e.subdir.as_ref()))
                .cloned()
                .unwrap_or_default();

            for entry in &entries {
                if let Some(ref subdir) = entry.subdir {
                    // Virtual directory pseudo-entry
                    let dir_name = subdir
                        .trim_end_matches('/')
                        .rsplit('/')
                        .next()
                        .unwrap_or(subdir);
                    if !dir_name.is_empty() {
                        all_entries.push(RemoteEntry::directory(
                            dir_name.to_string(),
                            format!("/{}", subdir.trim_end_matches('/')),
                        ));
                    }
                } else if let Some(ref name) = entry.name {
                    // Skip .file-segments/ (SLO internals)
                    if name.contains("/.file-segments/") || name.starts_with(".file-segments/") {
                        continue;
                    }

                    if name.ends_with('/') && entry.bytes.unwrap_or(0) == 0 {
                        // Directory marker object
                        let dir_name = name
                            .trim_end_matches('/')
                            .rsplit('/')
                            .next()
                            .unwrap_or(name);
                        if !dir_name.is_empty() {
                            all_entries.push(RemoteEntry::directory(
                                dir_name.to_string(),
                                format!("/{}", name.trim_end_matches('/')),
                            ));
                        }
                    } else {
                        // Regular file
                        let file_name = name.rsplit('/').next().unwrap_or(name);
                        let mut re = RemoteEntry::file(
                            file_name.to_string(),
                            format!("/{name}"),
                            entry.bytes.unwrap_or(0),
                        );
                        re.modified = entry.last_modified.clone();
                        re.mime_type = entry.content_type.clone();
                        if let Some(ref hash) = entry.hash {
                            re.metadata.insert("etag".to_string(), hash.clone());
                        }
                        all_entries.push(re);
                    }
                }
            }

            if entries.len() < 10000 {
                break;
            }
            marker = last_name;
        }

        Ok(all_entries)
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> {
        Ok(self.current_path.clone())
    }

    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        self.current_path = if path.starts_with('/') {
            path.to_string()
        } else {
            format!("{}/{}", self.current_path.trim_end_matches('/'), path)
        };
        Ok(())
    }

    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        if self.current_path != "/" {
            if let Some((parent, _)) = self.current_path.rsplit_once('/') {
                self.current_path = if parent.is_empty() {
                    "/".to_string()
                } else {
                    parent.to_string()
                };
            }
        }
        Ok(())
    }

    /// GET {storage_url}/{container}/{object}
    async fn download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let url = self.object_url(remote_path)?;
        let resp = self.swift_request(Method::GET, &url, None, &[]).await?;

        if !resp.status().is_success() {
            return Err(ProviderError::ServerError(format!(
                "Download failed: HTTP {}",
                resp.status()
            )));
        }

        let total = resp.content_length().unwrap_or(0);

        // Read full response body
        let bytes = resp.bytes().await.map_err(|e| {
            ProviderError::TransferFailed(format!("Download body read failed: {e}"))
        })?;
        let bytes_written = bytes.len() as u64;

        // Ensure parent directory exists
        if let Some(parent) = std::path::Path::new(local_path).parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| ProviderError::TransferFailed(format!("Create dir failed: {e}")))?;
        }

        tokio::fs::write(local_path, &bytes)
            .await
            .map_err(|e| ProviderError::TransferFailed(format!("Write file failed: {e}")))?;

        if let Some(cb) = on_progress {
            cb(bytes_written, total.max(bytes_written));
        }

        Ok(())
    }

    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        let url = self.object_url(remote_path)?;
        let resp = self.swift_request(Method::GET, &url, None, &[]).await?;

        if !resp.status().is_success() {
            return Err(ProviderError::ServerError(format!(
                "Download failed: HTTP {}",
                resp.status()
            )));
        }

        resp.bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| ProviderError::TransferFailed(format!("Read bytes failed: {e}")))
    }

    /// PUT {storage_url}/{container}/{object}
    /// Headers: Content-Type, ETag (MD5), X-Object-Meta-Mtime
    /// For files >5GB, delegates to upload_slo()
    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let metadata = tokio::fs::metadata(local_path)
            .await
            .map_err(|e| ProviderError::TransferFailed(format!("Stat local file failed: {e}")))?;
        let file_size = metadata.len();

        // SLO for files > 5 GiB
        if file_size > 5 * 1024 * 1024 * 1024 {
            return self.upload_slo(local_path, remote_path, on_progress).await;
        }

        let data = tokio::fs::read(local_path)
            .await
            .map_err(|e| ProviderError::TransferFailed(format!("Read local file failed: {e}")))?;

        let url = self.object_url(remote_path)?;

        // Content-Type from filename
        let mime = mime_guess::from_path(remote_path)
            .first_or_octet_stream()
            .to_string();

        // MD5 for integrity
        let digest = md5_hex(&data);

        // Preserve local mtime
        let mtime = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| format!("{}.000000", d.as_secs()));

        let mut headers = vec![
            ("Content-Type".to_string(), mime),
            ("ETag".to_string(), digest),
        ];
        if let Some(mt) = mtime {
            headers.push(("X-Object-Meta-Mtime".to_string(), mt));
        }

        let resp = self
            .swift_request(Method::PUT, &url, Some(data), &headers)
            .await?;

        match resp.status() {
            StatusCode::CREATED => {
                if let Some(cb) = on_progress {
                    cb(file_size, file_size);
                }
                Ok(())
            }
            StatusCode::UNPROCESSABLE_ENTITY => Err(ProviderError::ServerError(
                "ETag mismatch — data corrupted in transit".into(),
            )),
            status => Err(ProviderError::ServerError(format!(
                "Upload failed: HTTP {status}"
            ))),
        }
    }

    /// PUT zero-byte object with trailing / as directory marker
    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let dir_path = format!("{}/", Self::normalize_path(path));
        let url = self.object_url(&dir_path)?;

        let headers = vec![
            (
                "Content-Type".to_string(),
                "application/directory".to_string(),
            ),
            ("Content-Length".to_string(), "0".to_string()),
        ];

        let resp = self
            .swift_request(Method::PUT, &url, Some(vec![]), &headers)
            .await?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(ProviderError::ServerError(format!(
                "Mkdir failed: HTTP {}",
                resp.status()
            )))
        }
    }

    /// DELETE {storage_url}/{container}/{object}
    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        let url = self.object_url(path)?;
        let resp = self.swift_request(Method::DELETE, &url, None, &[]).await?;

        match resp.status() {
            StatusCode::NO_CONTENT | StatusCode::OK => Ok(()),
            StatusCode::NOT_FOUND => Err(ProviderError::NotFound(path.to_string())),
            status => Err(ProviderError::ServerError(format!(
                "Delete failed: HTTP {status}"
            ))),
        }
    }

    /// Delete directory marker
    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        // Try with trailing slash (directory marker convention)
        let dir_path = format!("{}/", Self::normalize_path(path));
        let url = self.object_url(&dir_path)?;
        let resp = self.swift_request(Method::DELETE, &url, None, &[]).await?;
        match resp.status() {
            StatusCode::NO_CONTENT | StatusCode::OK | StatusCode::NOT_FOUND => Ok(()),
            status => Err(ProviderError::ServerError(format!(
                "Rmdir failed: HTTP {status}"
            ))),
        }
    }

    /// Recursive delete via bulk-delete.
    /// POST {storage_url}?bulk-delete
    ///   Content-Type: text/plain
    ///   Body: /{container}/path1\n/{container}/path2\n...
    /// Max 10000 per request.
    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        let prefix = Self::normalize_path(path);

        // List all objects under prefix (no delimiter = flat recursive listing)
        let base = format!("{}/{}", self.storage_url()?, self.container);
        let url = format!(
            "{}?format=json&prefix={}/&limit=10000",
            base,
            urlencoding::encode(&prefix)
        );

        let resp = self.swift_request(Method::GET, &url, None, &[]).await?;
        let entries: Vec<ObjectEntry> = resp
            .json()
            .await
            .map_err(|e| ProviderError::ServerError(format!("List for delete failed: {e}")))?;

        if entries.is_empty() {
            let _ = self.rmdir(path).await;
            return Ok(());
        }

        // Collect all object paths for bulk delete
        let mut object_paths: Vec<String> = entries
            .iter()
            .filter_map(|e| e.name.as_ref())
            .map(|n| format!("/{}/{n}", self.container))
            .collect();

        // Also delete the directory marker itself
        object_paths.push(format!("/{}/{prefix}/", self.container));

        // Bulk delete in chunks of 10000
        for chunk in object_paths.chunks(10000) {
            let body = chunk.join("\n");
            let bulk_url = format!("{}?bulk-delete", self.storage_url()?);

            let headers = vec![
                ("Content-Type".to_string(), "text/plain".to_string()),
                ("Accept".to_string(), "application/json".to_string()),
            ];

            let resp = self
                .swift_request(Method::POST, &bulk_url, Some(body.into_bytes()), &headers)
                .await?;

            if !resp.status().is_success() {
                warn!("Bulk delete returned HTTP {}", resp.status());
            }
        }

        Ok(())
    }

    /// Rename via server-side COPY + DELETE (Swift has no atomic rename).
    /// PUT {dest_url} with X-Copy-From: /{container}/{source}
    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let from_clean = Self::normalize_path(from);
        let to_clean = Self::normalize_path(to);

        let dest_url = self.object_url(&to_clean)?;
        let copy_from = format!("/{}/{from_clean}", self.container);

        let headers = vec![
            ("X-Copy-From".to_string(), copy_from),
            ("Content-Length".to_string(), "0".to_string()),
        ];

        let resp = self
            .swift_request(Method::PUT, &dest_url, Some(vec![]), &headers)
            .await?;
        if !resp.status().is_success() {
            return Err(ProviderError::ServerError(format!(
                "Copy for rename failed: HTTP {}",
                resp.status()
            )));
        }

        self.delete(from).await
    }

    /// HEAD {storage_url}/{container}/{object}
    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        let url = self.object_url(path)?;
        let resp = self.swift_request(Method::HEAD, &url, None, &[]).await?;

        match resp.status() {
            StatusCode::OK | StatusCode::NO_CONTENT => {
                let size = resp
                    .headers()
                    .get("content-length")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(0);

                let modified = resp
                    .headers()
                    .get("last-modified")
                    .and_then(|v| v.to_str().ok())
                    .map(|v| v.to_string());

                let mime = resp
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .map(|v| v.to_string());

                let is_dir =
                    mime.as_deref() == Some("application/directory") || path.ends_with('/');
                let name = path
                    .trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .unwrap_or(path)
                    .to_string();

                let mut entry = if is_dir {
                    RemoteEntry::directory(name, format!("/{}", path.trim_matches('/')))
                } else {
                    RemoteEntry::file(name, format!("/{}", path.trim_matches('/')), size)
                };
                entry.modified = modified;
                entry.mime_type = mime;

                if let Some(etag) = resp.headers().get("etag").and_then(|v| v.to_str().ok()) {
                    entry
                        .metadata
                        .insert("etag".to_string(), etag.trim_matches('"').to_string());
                }
                if let Some(mtime) = resp
                    .headers()
                    .get("x-object-meta-mtime")
                    .and_then(|v| v.to_str().ok())
                {
                    entry
                        .metadata
                        .insert("mtime".to_string(), mtime.to_string());
                }

                Ok(entry)
            }
            StatusCode::NOT_FOUND => Err(ProviderError::NotFound(path.to_string())),
            status => Err(ProviderError::ServerError(format!(
                "Stat failed: HTTP {status}"
            ))),
        }
    }

    async fn size(&mut self, path: &str) -> Result<u64, ProviderError> {
        self.stat(path).await.map(|e| e.size)
    }

    async fn exists(&mut self, path: &str) -> Result<bool, ProviderError> {
        match self.stat(path).await {
            Ok(_) => Ok(true),
            Err(ProviderError::NotFound(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// HEAD {storage_url} — lightweight, validates token
    async fn keep_alive(&mut self) -> Result<(), ProviderError> {
        self.ensure_auth().await?;
        let url = self.storage_url()?.to_string();
        let resp = self.swift_request(Method::HEAD, &url, None, &[]).await?;
        if resp.status().is_success() || resp.status() == StatusCode::NO_CONTENT {
            Ok(())
        } else {
            Err(ProviderError::ConnectionFailed("Keep-alive failed".into()))
        }
    }

    /// HEAD {storage_url} -> account info headers
    async fn server_info(&mut self) -> Result<String, ProviderError> {
        let url = self.storage_url()?.to_string();
        let resp = self.swift_request(Method::HEAD, &url, None, &[]).await?;

        let get_header = |name: &str| -> String {
            resp.headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("?")
                .to_string()
        };

        Ok(format!(
            "OpenStack Swift\nContainer: {}\nContainers: {}\nObjects: {}\nStorage used: {} bytes",
            self.container,
            get_header("x-account-container-count"),
            get_header("x-account-object-count"),
            get_header("x-account-bytes-used"),
        ))
    }

    fn supports_server_copy(&self) -> bool {
        true
    }

    async fn server_copy(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let from_clean = Self::normalize_path(from);
        let to_clean = Self::normalize_path(to);
        let dest_url = self.object_url(&to_clean)?;
        let copy_from = format!("/{}/{from_clean}", self.container);

        let headers = vec![
            ("X-Copy-From".to_string(), copy_from),
            ("Content-Length".to_string(), "0".to_string()),
        ];

        let resp = self
            .swift_request(Method::PUT, &dest_url, Some(vec![]), &headers)
            .await?;
        if resp.status() == StatusCode::CREATED || resp.status().is_success() {
            Ok(())
        } else {
            Err(ProviderError::ServerError(format!(
                "Server copy failed: HTTP {}",
                resp.status()
            )))
        }
    }

    /// HEAD {storage_url} -> X-Account-Bytes-Used + X-Account-Meta-Quota-Bytes.
    /// Default 40GB for Blomp free tier if quota header absent.
    async fn storage_info(&mut self) -> Result<StorageInfo, ProviderError> {
        let url = self.storage_url()?.to_string();
        let resp = self.swift_request(Method::HEAD, &url, None, &[]).await?;

        let used: u64 = resp
            .headers()
            .get("x-account-bytes-used")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);

        let total: u64 = resp
            .headers()
            .get("x-account-meta-quota-bytes")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
            .unwrap_or(42_949_672_960); // 40 GB default

        Ok(StorageInfo {
            used,
            total,
            free: total.saturating_sub(used),
        })
    }
}
