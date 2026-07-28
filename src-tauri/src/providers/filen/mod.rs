//! Filen.io Storage Provider
//!
//! Implements StorageProvider for Filen using their REST API.
//! Uses client-side AES-256-GCM encryption (zero-knowledge).
//! All file names, metadata, and content are encrypted locally.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

pub mod notes;
mod statfs;

use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use futures_util::StreamExt;
use reqwest::header::{HeaderValue, CONTENT_TYPE};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use sha1::Sha1;
use sha2::{Digest, Sha512};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::debug;

/// Debug logging through tracing infrastructure (no file I/O)
fn filen_log(msg: &str) {
    debug!(target: "filen", "{}", msg);
}

use super::http_retry::{send_with_retry, HttpRetryConfig};
use super::types::FilenConfig;
use super::{
    MultipartHandle, ProviderError, ProviderTransferExecutorKind, ProviderType, RemoteEntry,
    ShareLinkCapabilities, ShareLinkOptions, ShareLinkResult, StorageInfo, StorageProvider,
    UploadedPart,
};
use serde::Serialize;

/// Side-band metadata embedded in `MultipartHandle.upload_id` for the
/// Filen v3 chunked upload trait wiring (S3-T02). The runner pre-resolves
/// the parent folder UUID, generates the per-upload random file UUID,
/// file key, and upload key in `begin_multipart_upload`, then passes them
/// through every chunk and the closing `/v3/upload/done` call without
/// touching the gateway again until completion.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct FilenMultipartMeta {
    file_uuid: String,
    parent_uuid: String,
    file_key: String,
    upload_key: String,
    file_name: String,
    mime: String,
    total: u64,
    part: u64,
    total_chunks: u64,
    /// Source mtime in epoch ms, captured when the session opened. The commit
    /// happens long after the local file was read, so the mtime has to travel
    /// with the handle: without it the multipart path re-dated large files to
    /// their upload time while the single-shot path preserved them, and the two
    /// disagreed on the same folder (#347). `None` for handles created before
    /// this field existed, which then fall back to the commit time.
    #[serde(default)]
    last_modified_ms: Option<i64>,
}

impl FilenMultipartMeta {
    fn encode(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    fn decode(raw: &str) -> Result<Self, ProviderError> {
        serde_json::from_str(raw).map_err(|e| {
            ProviderError::Other(format!("Filen multipart handle decode failed: {}", e))
        })
    }
}

/// Compute the per-chunk size the runner should slice `total` bytes into.
fn filen_runner_part_size(total: u64) -> u64 {
    (FILEN_CHUNK_SIZE as u64).min(total.max(1))
}

/// KE-E3: Filen rate-limit detection.
///
/// Filen's REST API (`gateway.filen.io`) and ingest CDN (`ingest.filen.io`)
/// signal throttling via:
///
/// - **429 Too Many Requests** on either endpoint when the per-account or
///   per-IP rate cap is exceeded.
/// - **503 Service Unavailable** during sustained ingest pressure on the
///   shared free-tier nodes.
///
/// In both cases the `Retry-After` header is sent (numeric seconds). We do
/// NOT inspect the JSON body: Filen's `{"status": false, "message": "..."}`
/// envelope is used for application errors (auth failure, invalid path,
/// file too large, etc.) that are NOT rate-limit signals.
fn filen_is_rate_limited(status: u16) -> bool {
    matches!(status, 429 | 503)
}

/// Whether a failed Filen egest/ingest HTTP status is a transient class worth
/// retrying. Server errors (5xx) plus the two recoverable 4xx signals (408
/// Request Timeout, 429 Too Many Requests) are retried with backoff; every
/// other 4xx is authoritative and surfaced immediately. Extracted as a pure fn
/// from `download_filen_chunk` so the retry policy boundary is unit-tested.
fn filen_status_is_retryable(status: u16) -> bool {
    (500..=599).contains(&status) || status == 408 || status == 429
}

/// KE-E3: Compute the marker tail to append to a Filen `ProviderError`
/// message when the response was rate-limited and a usable `Retry-After`
/// header was present. Pure-fn for test coverage.
fn filen_retry_marker_tail(status: u16, retry_header: Option<&str>) -> Option<String> {
    if !filen_is_rate_limited(status) {
        return None;
    }
    let hint = super::retry_after::parse_retry_after_seconds(retry_header.unwrap_or(""))?;
    Some(crate::transfer_dag::adaptive::embed_retry_after_marker(
        hint.as_secs(),
    ))
}

/// KE-E3: Format a Filen HTTP error message with optional Retry-After
/// marker. The `prefix` is prepended verbatim; `status` and a body
/// preview (truncated to keep error logs bounded) are appended; the
/// marker ` [retry-after-secs=NN]` is appended last when present.
fn format_filen_error(
    prefix: &str,
    status: reqwest::StatusCode,
    body_preview: &str,
    retry_header: Option<&str>,
) -> String {
    let mut msg = format!("{}: {} - {}", prefix, status, body_preview);
    if let Some(tail) = filen_retry_marker_tail(status.as_u16(), retry_header) {
        msg.push_str(&tail);
    }
    msg
}

/// Filen API gateway
const GATEWAY: &str = "https://gateway.filen.io";

/// Filen ingest CDN (chunk upload POST target).
const INGEST: &str = "https://ingest.filen.io";

/// Honest file/session ceiling for clone-backed Filen workers (DAG-P1-05D).
/// Matches `multipart_max_parallel` / `FILEN_PARALLEL_CHUNK_UPLOADS`.
const FILEN_TRANSFER_MAX_SESSIONS: u16 = 4;

/// Plaintext chunk size used by the Filen v3 upload pipeline. Matches the
/// `CHUNK_SIZE` constant from the official `filen-sdk-rs` crate. The Filen
/// ingest API rejects bodies larger than 1 MiB plus the AES-GCM overhead
/// (12-byte nonce + 16-byte auth tag = 28 bytes) per request.
const FILEN_CHUNK_SIZE: usize = 1024 * 1024;

/// Cap on the number of in-flight chunk uploads. The official SDK uses 16;
/// we deliberately stay lower to keep pressure off shared free-tier ingest
/// nodes and to avoid 429 rate-limit responses on slow uplinks. 4 is plenty
/// to saturate a residential connection while still leaving headroom for the
/// rest of the application.
const FILEN_PARALLEL_CHUNK_UPLOADS: usize = 4;

/// Hard cap on the size of a single Filen upload. With chunked streaming the
/// process holds at most a handful of 1 MiB buffers in memory regardless of
/// file size, so the cap exists purely as a sanity guard against pathological
/// inputs (a 256 GiB file already saturates Filen's per-account quota many
/// times over).
const FILEN_MAX_UPLOAD_SIZE: u64 = 256 * 1024 * 1024 * 1024;

/// Number of retry attempts for a single chunk download from `egest.filen.io`.
/// Filen's egest CDN occasionally returns a truncated response body or closes
/// the TCP connection mid-response on long sequential download sessions
/// (observed reliably on 1024-chunk sequences = 1 GiB files). Aborting the
/// whole download on the first transient body-decode error means losing every
/// chunk that already succeeded, so we retry just the offending chunk a few
/// times with exponential backoff before propagating the failure upwards.
const FILEN_DOWNLOAD_CHUNK_RETRIES: u32 = 4;

/// Filen auth info response
#[derive(Debug, Deserialize)]
struct AuthInfoResponse {
    status: bool,
    data: Option<AuthInfoData>,
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct AuthInfoData {
    #[serde(rename = "authVersion")]
    auth_version: u32,
    salt: String,
}

/// Filen login response
#[derive(Debug, Deserialize)]
struct LoginResponse {
    status: bool,
    data: Option<LoginData>,
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct LoginData {
    #[serde(rename = "apiKey")]
    api_key: String,
    #[serde(rename = "masterKeys")]
    master_keys: String,
}

/// Filen dir content response
#[derive(Debug, Deserialize)]
struct DirContentResponse {
    status: bool,
    data: Option<DirContentData>,
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DirContentData {
    #[serde(default)]
    folders: Vec<FilenFolder>,
    #[serde(default)]
    uploads: Vec<FilenFile>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct FilenFolder {
    uuid: String,
    name: String, // encrypted
    parent: String,
    timestamp: u64,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct FilenFile {
    uuid: String,
    metadata: String, // encrypted JSON: {name, size, mime, key, lastModified}
    bucket: String,
    region: String,
    parent: String,
    timestamp: u64,
    chunks: u32,
    size: u64,
}

/// Decrypted file metadata
#[derive(Debug, Deserialize)]
struct FileMetadata {
    name: String,
    size: u64,
    #[serde(default)]
    mime: String,
    key: String,
    #[serde(rename = "lastModified")]
    last_modified: Option<u64>,
}

/// Filen user info response
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct UserInfoResponse {
    status: bool,
    data: Option<UserInfoData>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct UserInfoData {
    #[serde(rename = "storageUsed")]
    storage_used: u64,
    #[serde(rename = "maxStorage")]
    max_storage: u64,
}

/// Filen generic response
#[derive(Debug, Deserialize)]
struct GenericResponse {
    status: bool,
    message: Option<String>,
}

/// Response for POST /v3/file/versions (F-FEAT-02).
#[derive(Debug, Deserialize)]
struct FileVersionsResponse {
    status: bool,
    message: Option<String>,
    data: Option<FileVersionsData>,
}

#[derive(Debug, Deserialize)]
struct FileVersionsData {
    #[serde(default)]
    versions: Vec<FilenFileVersion>,
}

/// One entry from the file-versions list. Each version is a self-contained
/// encrypted blob: its `metadata` carries that version's own per-file key, so
/// an old version can be downloaded independently of the current file.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct FilenFileVersion {
    uuid: String,
    region: String,
    bucket: String,
    chunks: u32,
    metadata: String, // encrypted JSON: {name, size, mime, key, lastModified}
    timestamp: u64,   // seconds (matches folder/file top-level timestamps)
    version: u32,
    #[serde(default)]
    rm: String,
}

/// Filen link status response
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct LinkStatusResponse {
    status: bool,
    data: Option<LinkStatusData>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct LinkStatusData {
    enabled: Option<bool>,
    uuid: Option<String>,
}

/// Filen link edit response
#[derive(Debug, Deserialize)]
struct LinkEditResponse {
    status: bool,
    message: Option<String>,
}

/// Filen create folder response
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct CreateFolderResponse {
    status: bool,
    data: Option<CreateFolderData>,
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct CreateFolderData {
    uuid: String,
}

/// Directory info in our cache
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct DirInfo {
    uuid: String,
    name: String,
}

/// Immutable connected auth/crypto snapshot shared by primary and transfer clones.
///
/// Replaced wholesale on connect/disconnect. Workers never run login, KDF,
/// token refresh, or network reconnect. They only read this snapshot.
#[derive(Clone)]
struct FilenAuthSnapshot {
    /// F-SEC-01: API key wrapped in SecretString for memory zeroization on drop
    api_key: SecretString,
    /// F-SEC-02: Master encryption keys wrapped in SecretString for memory zeroization on drop
    master_keys: Vec<SecretString>,
}

impl FilenAuthSnapshot {
    fn empty() -> Self {
        Self {
            api_key: SecretString::from(String::new()),
            master_keys: Vec::new(),
        }
    }
}

/// Filen Storage Provider
pub struct FilenProvider {
    /// Shared immutable config (password/email/optional API key). Arc so transfer
    /// clones do not independently copy secret material.
    config: Arc<FilenConfig>,
    client: reqwest::Client,
    connected: bool,
    /// Shared immutable auth/crypto snapshot (API key + master-key ring).
    auth: Arc<FilenAuthSnapshot>,
    current_path: String,
    current_folder_uuid: String,
    root_uuid: String,
    /// Cache: path -> DirInfo
    /// M3: Capped at DIR_CACHE_MAX_ENTRIES to prevent unbounded memory growth
    dir_cache: HashMap<String, DirInfo>,
    /// Backend-only cache: file UUID -> encryption key (never sent to frontend)
    file_key_cache: HashMap<String, String>,
    /// F-ERR-01: Retry configuration for HTTP requests
    retry_config: HttpRetryConfig,
    /// User UUID for Notes participant operations (from /v3/user/account)
    user_uuid: String,
    /// Last auth version returned by /v3/auth/info after a successful connect.
    auth_version: Option<u32>,
    /// Test-only gateway base. `None` keeps production `GATEWAY`.
    #[cfg(test)]
    gateway_base_override: Option<String>,
    /// Test-only ingest base. `None` keeps production `INGEST`.
    #[cfg(test)]
    ingest_base_override: Option<String>,
}

/// M3: Maximum number of cached directory/file-key entries to prevent unbounded memory growth.
const DIR_CACHE_MAX_ENTRIES: usize = 10_000;
const FILE_KEY_CACHE_MAX_ENTRIES: usize = 10_000;

impl FilenProvider {
    pub fn new(config: FilenConfig) -> Self {
        let client = reqwest::Client::builder()
            .user_agent(crate::providers::AEROFTP_USER_AGENT)
            .connect_timeout(std::time::Duration::from_secs(30))
            .read_timeout(std::time::Duration::from_secs(1800))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            config: Arc::new(config),
            client,
            connected: false,
            auth: Arc::new(FilenAuthSnapshot::empty()),
            current_path: "/".to_string(),
            current_folder_uuid: String::new(),
            root_uuid: String::new(),
            dir_cache: HashMap::new(),
            file_key_cache: HashMap::new(),
            retry_config: HttpRetryConfig::default(),
            user_uuid: String::new(),
            auth_version: None,
            #[cfg(test)]
            gateway_base_override: None,
            #[cfg(test)]
            ingest_base_override: None,
        }
    }

    pub fn auth_version(&self) -> Option<u32> {
        self.auth_version
    }

    /// Connected worker for unit tests (no network). Production clones use
    /// [`StorageProvider::clone_for_transfer`] after a real `connect`.
    #[cfg(test)]
    fn connected_for_test(config: FilenConfig) -> Self {
        let mut p = Self::new(config);
        p.connected = true;
        p.auth = Arc::new(FilenAuthSnapshot {
            api_key: SecretString::from("test-api-key-not-for-production".to_string()),
            master_keys: vec![SecretString::from(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string(),
            )],
        });
        p.root_uuid = "root-uuid-test".to_string();
        p.current_folder_uuid = "root-uuid-test".to_string();
        p.current_path = "/".to_string();
        p.dir_cache.insert(
            "/".to_string(),
            DirInfo {
                uuid: "root-uuid-test".to_string(),
                name: "/".to_string(),
            },
        );
        p
    }

    /// Gateway base (`https://gateway.filen.io` in production).
    fn gateway_base(&self) -> &str {
        #[cfg(test)]
        if let Some(ref base) = self.gateway_base_override {
            return base.as_str();
        }
        GATEWAY
    }

    /// Ingest base (`https://ingest.filen.io` in production).
    fn ingest_base(&self) -> &str {
        #[cfg(test)]
        if let Some(ref base) = self.ingest_base_override {
            return base.as_str();
        }
        INGEST
    }

    /// Bound transfer-worker clone: shares config + auth Arc snapshots and the
    /// cloneable `reqwest::Client`, seeds only root + current path/folder
    /// navigation state (never copies the full `dir_cache` / `file_key_cache`).
    fn clone_transfer_worker(&self) -> Self {
        let mut dir_cache = HashMap::new();
        if !self.root_uuid.is_empty() {
            dir_cache.insert(
                "/".to_string(),
                DirInfo {
                    uuid: self.root_uuid.clone(),
                    name: "/".to_string(),
                },
            );
        }
        if self.current_path != "/" && !self.current_folder_uuid.is_empty() {
            let leaf = self
                .current_path
                .rsplit('/')
                .next()
                .unwrap_or("")
                .to_string();
            dir_cache.insert(
                self.current_path.clone(),
                DirInfo {
                    uuid: self.current_folder_uuid.clone(),
                    name: leaf,
                },
            );
        }
        Self {
            config: Arc::clone(&self.config),
            client: self.client.clone(),
            connected: self.connected,
            auth: Arc::clone(&self.auth),
            current_path: self.current_path.clone(),
            current_folder_uuid: self.current_folder_uuid.clone(),
            root_uuid: self.root_uuid.clone(),
            dir_cache,
            file_key_cache: HashMap::new(),
            retry_config: self.retry_config.clone(),
            user_uuid: self.user_uuid.clone(),
            auth_version: self.auth_version,
            #[cfg(test)]
            gateway_base_override: self.gateway_base_override.clone(),
            #[cfg(test)]
            ingest_base_override: self.ingest_base_override.clone(),
        }
    }

    /// M3: Insert into dir_cache with eviction when cap is reached.
    fn dir_cache_insert(&mut self, key: String, value: DirInfo) {
        if self.dir_cache.len() >= DIR_CACHE_MAX_ENTRIES {
            debug!(target: "filen", "dir_cache reached {} entries, evicting all", self.dir_cache.len());
            self.dir_cache.clear();
        }
        self.dir_cache.insert(key, value);
    }

    /// M3: Insert into file_key_cache with eviction when cap is reached.
    fn file_key_cache_insert(&mut self, key: String, value: String) {
        if self.file_key_cache.len() >= FILE_KEY_CACHE_MAX_ENTRIES {
            debug!(target: "filen", "file_key_cache reached {} entries, evicting all", self.file_key_cache.len());
            self.file_key_cache.clear();
        }
        self.file_key_cache.insert(key, value);
    }

    /// Send a request with automatic retry on 429/5xx errors.
    /// F-ERR-01/F-ERR-02: Integrates send_with_retry for automatic rate-limit and server error handling.
    async fn send_retry(
        &self,
        request: reqwest::Request,
    ) -> Result<reqwest::Response, ProviderError> {
        send_with_retry(&self.client, request, &self.retry_config)
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))
    }

    /// Derive password hash and master key for authentication.
    /// Returns (login_password, master_key).
    ///
    /// Filen v3 auth parameters are verified from the official Filen SDK:
    /// - Repository: FilenCloudDienste/filen-sdk-ts
    /// - Commit: 8d1291a4bda76718a5dc94253f92bf20e42f1696
    /// - Source: src/crypto/utils.ts:374-382 (Argon2id v3 params)
    /// - Source: src/index.ts:642-646 (salt from auth/info passed to derivation)
    ///
    /// v3 uses Argon2id with:
    /// - memory: 65536 KiB
    /// - iterations: 3
    /// - parallelism: 4
    /// - version: 0x13
    /// - output length: 64 bytes
    ///
    /// Salt handling differs by auth version:
    /// - v2: use salt as UTF-8 string bytes (PBKDF2-SHA512)
    /// - v3: decode salt as hex bytes before Argon2id
    fn derive_auth_credentials(
        password: &str,
        salt: &str,
        auth_version: u32,
    ) -> Result<(String, String), ProviderError> {
        if auth_version >= 3 {
            // v3: Argon2id(password, hex_decode(salt), t=3, m=65536, p=4, v=0x13, dkLen=64)
            let salt_bytes = hex::decode(salt).map_err(|e| {
                ProviderError::AuthenticationFailed(format!("Invalid Filen v3 salt hex: {}", e))
            })?;
            let params = argon2::Params::new(65_536, 3, 4, Some(64)).map_err(|e| {
                ProviderError::AuthenticationFailed(format!("Argon2id params error: {}", e))
            })?;
            let argon2 =
                argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
            let mut derived = [0u8; 64];
            argon2
                .hash_password_into(password.as_bytes(), &salt_bytes, &mut derived)
                .map_err(|e| {
                    ProviderError::AuthenticationFailed(format!("Argon2id derivation error: {}", e))
                })?;

            let derived_hex = hex::encode(derived);
            // First half = master key, second half = login password (per Filen SDK v3)
            let master_key = derived_hex[..derived_hex.len() / 2].to_string();
            let login_password = derived_hex[derived_hex.len() / 2..].to_string();
            return Ok((login_password, master_key));
        }
        if auth_version >= 2 {
            // v2: PBKDF2-SHA512, 200000 iterations → 64 bytes
            let mut derived = [0u8; 64];
            pbkdf2::pbkdf2_hmac::<Sha512>(
                password.as_bytes(),
                salt.as_bytes(),
                200_000,
                &mut derived,
            );
            let derived_hex = hex::encode(derived);
            // First half = master key, second half = login password (per Filen docs)
            let master_key = derived_hex[..derived_hex.len() / 2].to_string();
            let login_password_raw = &derived_hex[derived_hex.len() / 2..];
            // Login password must be re-hashed with SHA-512
            let mut hasher = Sha512::new();
            hasher.update(login_password_raw.as_bytes());
            let login_password = hex::encode(hasher.finalize());
            Ok((login_password, master_key))
        } else {
            // v1: Simple SHA512 (legacy)
            let mut hasher = Sha512::new();
            hasher.update(password.as_bytes());
            let hash_hex = hex::encode(hasher.finalize());
            // v1: password hash is used as both auth and master key
            Ok((hash_hex.clone(), hash_hex))
        }
    }

    /// Decrypt metadata string using master keys
    fn decrypt_metadata(&self, encrypted: &str) -> Option<String> {
        for key in &self.auth.master_keys {
            if let Some(decrypted) = Self::try_decrypt_aes_gcm(encrypted, key.expose_secret()) {
                return Some(decrypted);
            }
        }
        None
    }

    /// Decrypt folder name: handles both JSON {"name":"..."} and raw string formats
    fn decrypt_folder_name(&self, encrypted: &str) -> Option<String> {
        let decrypted = self.decrypt_metadata(encrypted)?;
        // Try JSON format first (Filen SDK wraps folder names in {"name":"..."})
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&decrypted) {
            if let Some(name) = v.get("name").and_then(|n| n.as_str()) {
                return Some(name.to_string());
            }
        }
        // Fall back to raw string (our older format or simple names)
        Some(decrypted)
    }

    /// The `lastModified` a write must carry, in the epoch milliseconds Filen
    /// stores in the encrypted file metadata.
    ///
    /// It is the SOURCE file's modification time, not the moment of the write.
    /// Filen's `lastModified` is the only mtime the account holds, so stamping
    /// the upload instant re-dates every file to when it was transferred: a
    /// folder uploaded through AeroFTP then compares as entirely out of sync
    /// against the very local folder it came from, and stays that way, because
    /// each re-upload stamps a new "now" (#347). The same wrong value is what
    /// the Filen desktop bridge then serves over WebDAV, which is why the
    /// symptom shows on both transports.
    ///
    /// Falls back to the current time only when the source has no readable
    /// mtime, which is the best a write can do and is still better than
    /// nothing for a file that never had one.
    fn source_last_modified_ms(source: Option<&std::fs::Metadata>) -> i64 {
        source
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or_else(|| chrono::Utc::now().timestamp_millis())
    }

    /// Epoch milliseconds for an existing entry's timestamp, so a metadata-only
    /// write (a rename) can carry the mtime the file already had instead of
    /// re-dating it. `None` when the entry never had one.
    fn entry_last_modified_ms(modified: Option<&str>) -> Option<i64> {
        let raw = modified?;
        chrono::DateTime::parse_from_rfc3339(raw)
            .ok()
            .map(|dt| dt.timestamp_millis())
    }

    /// Hash file/folder name for Filen API: SHA-1(SHA-512(name.toLowerCase()).hex()).hex()
    fn hash_name(name: &str) -> String {
        let sha512_hex = hex::encode(Sha512::digest(name.to_lowercase().as_bytes()));
        hex::encode(Sha1::digest(sha512_hex.as_bytes()))
    }

    /// Derive AES-256 key from master key, matching Filen SDK:
    /// PBKDF2-SHA512(password=key, salt=key, iterations=1, keylen=32)
    fn derive_aes_key(key: &str) -> Vec<u8> {
        let mut derived = vec![0u8; 32];
        pbkdf2::pbkdf2_hmac::<Sha512>(key.as_bytes(), key.as_bytes(), 1, &mut derived);
        derived
    }

    /// Try to decrypt AES-256-GCM encrypted data
    /// Filen format: "002" + 12-char IV (UTF-8 bytes) + base64(ciphertext+tag)
    ///
    /// TODO (F-ENC-01): Filen SDK v3 introduced a "003" metadata encryption format that uses
    /// AES-256-GCM with a different key derivation (direct HKDF instead of PBKDF2 with 1
    /// iteration). Files encrypted with the 003 format will fail to decrypt here. When Filen
    /// migrates accounts to 003 format, this function needs to be extended with the new
    /// key derivation path.
    fn try_decrypt_aes_gcm(encrypted: &str, key: &str) -> Option<String> {
        if encrypted.len() < 16 {
            filen_log(&format!("try_decrypt: too short ({})", encrypted.len()));
            return None;
        }

        let version = &encrypted[..3];
        filen_log(&format!(
            "try_decrypt: version={}, len={}",
            version,
            encrypted.len()
        ));

        let (nonce_bytes_vec, ciphertext) = match version {
            "002" => {
                // 002 format: 002{12-char-IV}{base64(ciphertext+tag)} - no separators
                let iv_str = &encrypted[3..15];
                let data_b64 = &encrypted[15..];
                let ct = BASE64.decode(data_b64).ok()?;
                (iv_str.as_bytes().to_vec(), ct)
            }
            "001" => {
                // 001 format: 001|iv|ciphertext+tag (pipe-separated, base64)
                let parts: Vec<&str> = encrypted.splitn(3, '|').collect();
                if parts.len() != 3 {
                    filen_log(&format!(
                        "try_decrypt: 001 format but {} parts",
                        parts.len()
                    ));
                    return None;
                }
                let iv_bytes = BASE64.decode(parts[1]).ok()?;
                let ct = BASE64.decode(parts[2]).ok()?;
                (iv_bytes, ct)
            }
            "003" => {
                // 003 format: not yet implemented (requires HKDF key derivation)
                filen_log("try_decrypt: 003 format not yet supported");
                return None;
            }
            _ => {
                filen_log(&format!("try_decrypt: unknown version '{}'", version));
                return None;
            }
        };

        let aes_key = Self::derive_aes_key(key);
        let cipher = Aes256Gcm::new_from_slice(&aes_key).ok()?;
        let nonce = Nonce::from_slice(&nonce_bytes_vec);

        match cipher.decrypt(nonce, ciphertext.as_ref()) {
            Ok(plaintext) => {
                let result = String::from_utf8(plaintext).ok()?;
                filen_log(&format!("try_decrypt: SUCCESS, len={}", result.len()));
                Some(result)
            }
            Err(e) => {
                filen_log(&format!("try_decrypt: decrypt FAILED: {}", e));
                None
            }
        }
    }

    /// Encrypt metadata with AES-256-GCM
    /// Filen format: "002" + 12-char IV (random ASCII alphanumeric) + base64(ciphertext+tag)
    fn encrypt_metadata(&self, data: &str) -> Result<String, ProviderError> {
        let key = self
            .auth
            .master_keys
            .first()
            .ok_or_else(|| ProviderError::Other("No master key".to_string()))?;

        let aes_key = Self::derive_aes_key(key.expose_secret());
        let cipher = Aes256Gcm::new_from_slice(&aes_key)
            .map_err(|e| ProviderError::Other(format!("Cipher error: {}", e)))?;

        // F-ENC-02: Use gen_range for unbiased random char generation (no modulo bias)
        let iv_chars: String = (0..12).map(|_| Self::random_alphanumeric_char()).collect();
        let nonce_bytes = iv_chars.as_bytes();
        let nonce = Nonce::from_slice(nonce_bytes);

        let ciphertext = cipher
            .encrypt(nonce, data.as_bytes())
            .map_err(|e| ProviderError::Other(format!("Encrypt error: {}", e)))?;

        Ok(format!("002{}{}", iv_chars, BASE64.encode(ciphertext)))
    }

    /// Encrypt metadata with a specific key (static version for per-key encryption)
    fn encrypt_metadata_with_key(data: &str, key: &str) -> Result<String, ProviderError> {
        let aes_key = Self::derive_aes_key(key);
        let cipher = Aes256Gcm::new_from_slice(&aes_key)
            .map_err(|e| ProviderError::Other(format!("Cipher error: {}", e)))?;

        // F-ENC-02: Use gen_range for unbiased random char generation
        let iv_chars: String = (0..12).map(|_| Self::random_alphanumeric_char()).collect();
        let nonce = Nonce::from_slice(iv_chars.as_bytes());

        let ciphertext = cipher
            .encrypt(nonce, data.as_bytes())
            .map_err(|e| ProviderError::Other(format!("Encrypt error: {}", e)))?;

        Ok(format!("002{}{}", iv_chars, BASE64.encode(ciphertext)))
    }

    /// Fetch the canonical encrypted master-keys blob via POST /v3/user/masterKeys.
    ///
    /// The API-key connect path skips /v3/login, which is where the password
    /// path receives this blob for free. The request sends the current master
    /// key encrypted under itself (Filen "002" metadata format); the server
    /// replies with the full encrypted master-keys list in `data.keys`, the
    /// same representation /v3/login returns in `data.masterKeys`.
    ///
    /// Best-effort by contract: the caller treats any error or a missing blob
    /// as "no extra keys" and proceeds with the password-derived master key,
    /// which already decrypts everything stored under the current password.
    async fn fetch_master_keys_blob(
        &self,
        master_key: &str,
    ) -> Result<Option<String>, ProviderError> {
        let encrypted = Self::encrypt_metadata_with_key(master_key, master_key)?;
        let resp: serde_json::Value = self
            .client
            .post(format!("{}/v3/user/masterKeys", self.gateway_base()))
            .header(
                "Authorization",
                HeaderValue::from_str(&format!("Bearer {}", self.auth.api_key.expose_secret()))
                    .map_err(|e| ProviderError::Other(format!("Invalid auth header: {}", e)))?,
            )
            .json(&serde_json::json!({ "masterKeys": encrypted }))
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;
        if resp["status"].as_bool() == Some(false) {
            let msg = resp["message"]
                .as_str()
                .unwrap_or("masterKeys request rejected");
            return Err(ProviderError::AuthenticationFailed(msg.to_string()));
        }
        Ok(resp["data"]["keys"].as_str().map(|s| s.to_string()))
    }

    /// F-ENC-02: Generate a single random alphanumeric character without modulo bias.
    /// Uses `rand::Rng::gen_range` which implements rejection sampling internally.
    fn random_alphanumeric_char() -> char {
        use rand::Rng;
        const CHARSET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        let idx = rand::thread_rng().gen_range(0..CHARSET.len());
        CHARSET[idx] as char
    }

    /// Derive the 32-byte AES-256 key from a Filen file key.
    ///
    /// Filen file keys come in two on-disk formats, and a container can hold
    /// files in either (depending on which client uploaded them):
    ///  - **v3 metadata**: a 64-char hex string that decodes to 32 raw bytes.
    ///    This is also the format AeroFTP itself emits on upload (a 64-char hex
    ///    string), so AeroFTP-uploaded files always take this path.
    ///  - **v2 metadata**: a 32-char string used *directly* as the 32-byte
    ///    AES-256 key (its UTF-8 bytes). Files stored by other Filen clients
    ///    (the official app, rclone, RSAF) commonly use v2.
    ///
    /// The previous hex-only path rejected every v2 key with
    /// "Invalid file key hex: Invalid character ... at position ...", which is
    /// why previewing/downloading images uploaded elsewhere failed (#128). The
    /// length is unambiguous: a v2 key is always 32 chars, a v3 key 64 hex chars.
    fn derive_file_key(file_key: &str) -> Result<Vec<u8>, ProviderError> {
        match file_key.len() {
            64 => hex::decode(file_key)
                // F-11: do not echo the hex parser's char/position detail.
                .map_err(|_| ProviderError::Other("Invalid Filen file key (not valid hex)".into())),
            32 => Ok(file_key.as_bytes().to_vec()),
            n => Err(ProviderError::Other(format!(
                "Unsupported Filen file key length {} (expected 32 raw or 64 hex)",
                n
            ))),
        }
    }

    /// Encrypt file content with AES-256-GCM using a per-file key
    /// Format: nonce (12 bytes) + ciphertext + auth tag (no version prefix)
    fn encrypt_file_content(data: &[u8], file_key: &str) -> Result<Vec<u8>, ProviderError> {
        let aes_key = Self::derive_file_key(file_key)?;
        let cipher = Aes256Gcm::new_from_slice(&aes_key)
            .map_err(|e| ProviderError::Other(format!("Cipher error: {}", e)))?;

        let nonce_bytes: [u8; 12] = rand::random();
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = cipher
            .encrypt(nonce, data)
            .map_err(|e| ProviderError::Other(format!("Encrypt error: {}", e)))?;

        // Filen format: nonce + ciphertext (includes auth tag)
        let mut result = Vec::with_capacity(12 + ciphertext.len());
        result.extend_from_slice(&nonce_bytes);
        result.extend_from_slice(&ciphertext);
        Ok(result)
    }

    /// Decrypt file content
    /// Format: nonce (12 bytes) + ciphertext + auth tag
    fn decrypt_file_content(data: &[u8], file_key: &str) -> Result<Vec<u8>, ProviderError> {
        if data.len() < 12 {
            return Err(ProviderError::Other("Encrypted data too short".to_string()));
        }

        let nonce_bytes = &data[..12];
        let ciphertext = &data[12..];

        let aes_key = Self::derive_file_key(file_key)?;
        let cipher = Aes256Gcm::new_from_slice(&aes_key)
            .map_err(|e| ProviderError::Other(format!("Cipher error: {}", e)))?;

        let nonce = Nonce::from_slice(nonce_bytes);
        cipher
            .decrypt(nonce, ciphertext)
            .map_err(|e| ProviderError::Other(format!("Decrypt error: {}", e)))
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

    /// Resolve a folder path to its UUID
    async fn resolve_folder_uuid(&mut self, path: &str) -> Result<String, ProviderError> {
        let normalized = Self::normalize_path(path);

        if normalized == "/" {
            return Ok(self.root_uuid.clone());
        }

        if let Some(info) = self.dir_cache.get(&normalized) {
            return Ok(info.uuid.clone());
        }

        // Walk the path from root
        let parts: Vec<&str> = normalized.split('/').filter(|s| !s.is_empty()).collect();
        let mut current_uuid = self.root_uuid.clone();
        let mut built_path = String::new();

        for part in parts {
            built_path = format!("{}/{}", built_path, part);

            if let Some(info) = self.dir_cache.get(&built_path) {
                current_uuid = info.uuid.clone();
                continue;
            }

            // List current folder to find child
            let request = self
                .client
                .post(format!("{}/v3/dir/content", self.gateway_base()))
                .header(
                    "Authorization",
                    HeaderValue::from_str(&format!("Bearer {}", self.auth.api_key.expose_secret()))
                        .map_err(|e| ProviderError::Other(format!("Invalid auth header: {}", e)))?,
                )
                .json(&serde_json::json!({"uuid": current_uuid}))
                .build()
                .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
            let resp = self.send_retry(request).await?;

            let content: DirContentResponse = resp
                .json()
                .await
                .map_err(|e| ProviderError::ParseError(e.to_string()))?;

            if !content.status {
                return Err(ProviderError::NotFound(format!("Path not found: {}", path)));
            }

            let data = content.data.unwrap_or(DirContentData {
                folders: vec![],
                uploads: vec![],
            });

            let mut found = false;
            for folder in &data.folders {
                if let Some(name) = self.decrypt_folder_name(&folder.name) {
                    if name == part {
                        current_uuid = folder.uuid.clone();
                        self.dir_cache_insert(
                            built_path.clone(),
                            DirInfo {
                                uuid: folder.uuid.clone(),
                                name: name.clone(),
                            },
                        );
                        found = true;
                        break;
                    }
                }
            }

            if !found {
                return Err(ProviderError::NotFound(format!(
                    "Folder not found: {}",
                    part
                )));
            }
        }

        Ok(current_uuid)
    }

    /// Resolve a file path to its current Filen file uuid by listing the parent
    /// directory and matching the file name. As a side effect this re-populates
    /// the backend-only file-key cache for the directory (via `list`), which the
    /// download path relies on.
    async fn resolve_file_uuid(&mut self, path: &str) -> Result<String, ProviderError> {
        let normalized = Self::normalize_path(path);
        let (parent_path, file_name) = match normalized.rfind('/') {
            Some(pos) if pos > 0 => (
                normalized[..pos].to_string(),
                normalized[pos + 1..].to_string(),
            ),
            _ => (
                "/".to_string(),
                normalized.trim_start_matches('/').to_string(),
            ),
        };
        let entries = self.list(&parent_path).await?;
        let file_entry = entries
            .iter()
            .find(|e| !e.is_dir && e.name == file_name)
            .ok_or_else(|| ProviderError::NotFound(format!("File not found: {}", file_name)))?;
        file_entry
            .metadata
            .get("uuid")
            .cloned()
            .ok_or_else(|| ProviderError::Other("No UUID for file".to_string()))
    }

    /// Fetch the raw version list for a file uuid (POST /v3/file/versions).
    /// Verified against filen-sdk-ts `api/v3/file/versions` (POST, body `{uuid}`).
    async fn fetch_file_versions(
        &self,
        file_uuid: &str,
    ) -> Result<Vec<FilenFileVersion>, ProviderError> {
        let request = self
            .client
            .post(format!("{}/v3/file/versions", self.gateway_base()))
            .header(
                "Authorization",
                HeaderValue::from_str(&format!("Bearer {}", self.auth.api_key.expose_secret()))
                    .map_err(|e| ProviderError::Other(format!("Invalid auth header: {}", e)))?,
            )
            .json(&serde_json::json!({ "uuid": file_uuid }))
            .build()
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
        let resp: FileVersionsResponse = self
            .send_retry(request)
            .await?
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;
        if !resp.status {
            return Err(ProviderError::ServerError(
                resp.message
                    .unwrap_or_else(|| "Failed to list file versions".to_string()),
            ));
        }
        let mut versions = resp.data.map(|d| d.versions).unwrap_or_default();
        // Newest first: the version record timestamp is the canonical creation
        // time and gives a stable order regardless of API response ordering.
        versions.sort_by_key(|v| std::cmp::Reverse(v.timestamp));
        Ok(versions)
    }
}

#[async_trait]
impl StorageProvider for FilenProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn provider_type(&self) -> ProviderType {
        ProviderType::Filen
    }

    fn display_name(&self) -> String {
        format!("Filen ({})", self.config.email)
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        // F-SEC-04: Password is accessed via expose_secret() and used directly in KDF.
        // The derived strings (auth_hash, derived_master_key) are intermediate values
        // that cannot use SecretString without significant refactoring of the KDF pipeline.
        // The expose_secret() borrow is scoped to minimize exposure lifetime.
        let password = self.config.password.expose_secret();

        // Step 1: Get auth info
        let auth_info_resp: AuthInfoResponse = self
            .client
            .post(format!("{}/v3/auth/info", self.gateway_base()))
            .json(&serde_json::json!({"email": self.config.email}))
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        if !auth_info_resp.status {
            return Err(ProviderError::AuthenticationFailed(
                auth_info_resp
                    .message
                    .unwrap_or_else(|| "Auth info failed".to_string()),
            ));
        }

        let auth_data = auth_info_resp
            .data
            .ok_or_else(|| ProviderError::AuthenticationFailed("No auth info data".to_string()))?;

        self.auth_version = Some(auth_data.auth_version);

        // Step 2: Derive password hash and master key
        let (auth_hash, derived_master_key) =
            Self::derive_auth_credentials(password, &auth_data.salt, auth_data.auth_version)?;

        // Step 3: obtain the API key and the encrypted master-keys blob.
        //
        // Two paths:
        // - API-key path (config.api_key set): authenticate with the supplied
        //   Filen CLI API key and skip POST /v3/login. /v3/login is the only
        //   call that consumes a twoFactorCode, so this path never touches the
        //   30s TOTP window, matching what a Filen rclone profile carrying an
        //   api_key achieves. The master-keys blob is recovered from
        //   POST /v3/user/masterKeys instead.
        // - Password path (default): POST /v3/login returns both the API key
        //   and the master-keys blob, but always requires a 2FA code.
        //
        // The password is required either way: it derived the E2E master key
        // above. The api_key only authorises API transport, never decryption.
        Arc::make_mut(&mut self.auth).master_keys =
            vec![SecretString::from(derived_master_key.clone())];

        let configured_api_key = self
            .config
            .api_key
            .as_ref()
            .map(|k| k.expose_secret().trim().to_string())
            .filter(|k| !k.is_empty());
        let api_key_path = configured_api_key.is_some();

        let encrypted_master_keys: Option<String> = if let Some(api_key) = configured_api_key {
            Arc::make_mut(&mut self.auth).api_key = SecretString::from(api_key);
            filen_log("connect: API-key path, skipping /v3/login (no 2FA window)");
            // The API-key path replaces /v3/login, so we must obtain and
            // decrypt the canonical master-keys ring here. A partial ring
            // would silently break decryption for files encrypted under
            // previously-rotated keys.
            match self.fetch_master_keys_blob(&derived_master_key).await {
                Ok(Some(blob)) => Some(blob),
                Ok(None) => {
                    filen_log("connect: /v3/user/masterKeys returned no blob");
                    return Err(ProviderError::AuthenticationFailed(
                        "Filen returned no master keys; reconnect with the account password to refresh the ring".to_string(),
                    ));
                }
                Err(e) => {
                    filen_log(&format!("connect: /v3/user/masterKeys failed: {}", e));
                    return Err(ProviderError::AuthenticationFailed(format!(
                        "Cannot load Filen master keys via API key ({}); reconnect with the account password to refresh the ring",
                        e
                    )));
                }
            }
        } else {
            // Password path: POST /v3/login. Filen requires twoFactorCode
            // always; use "XXXXXX" when 2FA is not enabled.
            //
            // The user can persist either a single-use code (typed at
            // connection time) or a base32 TOTP secret (saved once, derived
            // on demand). The secret takes precedence because it removes the
            // manual prompt on every reconnect and matches what rclone does
            // with Filen profiles.
            let derived_totp = self
                .config
                .totp_secret
                .as_ref()
                .map(super::totp_helper::generate_totp_code)
                .transpose()?;
            let two_fa = derived_totp
                .as_deref()
                .or(self.config.two_factor_code.as_deref())
                .unwrap_or("XXXXXX");
            let login_body = serde_json::json!({
                "email": self.config.email,
                "password": auth_hash,
                "authVersion": auth_data.auth_version,
                "twoFactorCode": two_fa,
            });
            let login_resp: LoginResponse = self
                .client
                .post(format!("{}/v3/login", self.gateway_base()))
                .json(&login_body)
                .send()
                .await
                .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?
                .json()
                .await
                .map_err(|e| ProviderError::ParseError(e.to_string()))?;

            if !login_resp.status {
                return Err(ProviderError::AuthenticationFailed(
                    login_resp
                        .message
                        .unwrap_or_else(|| "Login failed".to_string()),
                ));
            }

            let login_data = login_resp
                .data
                .ok_or_else(|| ProviderError::AuthenticationFailed("No login data".to_string()))?;

            Arc::make_mut(&mut self.auth).api_key = SecretString::from(login_data.api_key);
            Some(login_data.master_keys)
        };

        // Step 4: decrypt the additional master keys, when a blob was obtained.
        filen_log(&format!(
            "derived_master_key len={}",
            derived_master_key.len()
        ));
        if let Some(blob) = encrypted_master_keys {
            filen_log(&format!("master_keys blob len={}", blob.len()));
            match Self::try_decrypt_aes_gcm(&blob, &derived_master_key) {
                Some(decrypted) => {
                    let decrypted_keys: Vec<SecretString> = decrypted
                        .split('|')
                        .map(|s| SecretString::from(s.to_string()))
                        .collect();
                    // Check if derived_master_key is already present
                    let already_present = decrypted_keys
                        .iter()
                        .any(|k| k.expose_secret() == derived_master_key);
                    {
                        let auth = Arc::make_mut(&mut self.auth);
                        auth.master_keys = decrypted_keys;
                        if !already_present {
                            auth.master_keys
                                .push(SecretString::from(derived_master_key));
                        }
                    }
                }
                None if api_key_path => {
                    filen_log("connect: master_keys blob decrypt failed on API-key path");
                    return Err(ProviderError::AuthenticationFailed(
                        "Cannot decrypt Filen master keys with the account password; reconnect to refresh the ring".to_string(),
                    ));
                }
                None => {
                    filen_log("connect: master_keys blob decrypt failed; proceeding with derived_master_key only");
                }
            }
        }

        // Step 5: Get root folder UUID from user info
        let user_resp: serde_json::Value = self
            .client
            .get(format!("{}/v3/user/baseFolder", self.gateway_base()))
            .header(
                "Authorization",
                HeaderValue::from_str(&format!("Bearer {}", self.auth.api_key.expose_secret()))
                    .map_err(|e| ProviderError::Other(format!("Invalid auth header: {}", e)))?,
            )
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        self.root_uuid = user_resp["data"]["uuid"].as_str().unwrap_or("").to_string();

        if self.root_uuid.is_empty() {
            return Err(ProviderError::ConnectionFailed(
                "Failed to get root folder UUID".to_string(),
            ));
        }

        self.current_folder_uuid = self.root_uuid.clone();
        self.current_path = "/".to_string();
        self.dir_cache_insert(
            "/".to_string(),
            DirInfo {
                uuid: self.root_uuid.clone(),
                name: "/".to_string(),
            },
        );

        // Step 6: Fetch user UUID from /v3/user/account (required for Notes participant operations)
        let account_request = self
            .client
            .get(format!("{}/v3/user/account", self.gateway_base()))
            .header(
                "Authorization",
                HeaderValue::from_str(&format!("Bearer {}", self.auth.api_key.expose_secret()))
                    .map_err(|e| ProviderError::Other(format!("Invalid auth header: {}", e)))?,
            )
            .build()
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
        let account_resp: serde_json::Value = self
            .send_retry(account_request)
            .await?
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;
        if let Some(uuid) = account_resp["data"]["uuid"].as_str() {
            self.user_uuid = uuid.to_string();
        }

        self.connected = true;
        filen_log(&format!(
            "Connected as {}, root_uuid={}, user_uuid={}, master_keys={}",
            self.config.email,
            self.root_uuid,
            self.user_uuid,
            self.auth.master_keys.len()
        ));
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.connected = false;
        // F-SEC-01/02: Drop previous auth Arc; replace with empty snapshot
        // (SecretString zeroizes on drop when the Arc is unique or last-owned).
        self.auth = Arc::new(FilenAuthSnapshot::empty());
        self.dir_cache.clear();
        // F-SEC-03: Clear cached file encryption keys on disconnect
        self.file_key_cache.clear();
        Ok(())
    }

    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        // F-LIST-01: Filen API does not support server-side pagination for dir/content.
        // The entire folder listing is returned in a single response. This is inherent
        // to Filen's zero-knowledge design: the server cannot sort/page encrypted entries.
        let folder_uuid = if path == "." || path.is_empty() {
            self.current_folder_uuid.clone()
        } else {
            self.resolve_folder_uuid(path).await?
        };

        let request = self
            .client
            .post(format!("{}/v3/dir/content", self.gateway_base()))
            .header(
                "Authorization",
                HeaderValue::from_str(&format!("Bearer {}", self.auth.api_key.expose_secret()))
                    .map_err(|e| ProviderError::Other(format!("Invalid auth header: {}", e)))?,
            )
            .json(&serde_json::json!({"uuid": folder_uuid}))
            .build()
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
        let resp = self.send_retry(request).await?;

        let resp_text = resp
            .text()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        // F-LOG-01: Log raw response at debug level, truncated to 200 chars max
        let preview_len = resp_text.len().min(200);
        let preview = &resp_text[..preview_len];
        filen_log(&format!(
            "dir/content uuid={} response ({}B): {}",
            folder_uuid,
            resp_text.len(),
            preview
        ));

        let content: DirContentResponse = serde_json::from_str(&resp_text).map_err(|e| {
            ProviderError::ParseError(format!("JSON parse error: {} - response: {}", e, preview))
        })?;

        if !content.status {
            return Err(ProviderError::ServerError(
                content.message.unwrap_or_else(|| "List failed".to_string()),
            ));
        }

        let data = content.data.unwrap_or(DirContentData {
            folders: vec![],
            uploads: vec![],
        });
        filen_log(&format!(
            "list '{}' uuid={}: {} folders, {} files",
            path,
            folder_uuid,
            data.folders.len(),
            data.uploads.len()
        ));
        let mut entries = Vec::new();

        let base_path = if path == "." || path.is_empty() {
            self.current_path.clone()
        } else {
            Self::normalize_path(path)
        };

        // Folders
        for folder in data.folders {
            if let Some(name) = self.decrypt_folder_name(&folder.name) {
                let entry_path = if base_path == "/" {
                    format!("/{}", name)
                } else {
                    format!("{}/{}", base_path, name)
                };

                self.dir_cache_insert(
                    entry_path.clone(),
                    DirInfo {
                        uuid: folder.uuid.clone(),
                        name: name.clone(),
                    },
                );

                entries.push(RemoteEntry {
                    name,
                    path: entry_path,
                    is_dir: true,
                    size: 0,
                    modified: Some(
                        chrono::DateTime::from_timestamp(folder.timestamp as i64, 0)
                            .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
                            .unwrap_or_default(),
                    ),
                    permissions: None,
                    owner: None,
                    group: None,
                    is_symlink: false,
                    link_target: None,
                    mime_type: None,
                    metadata: {
                        let mut m = HashMap::new();
                        m.insert("uuid".to_string(), folder.uuid);
                        m
                    },
                });
            } else {
                filen_log(&format!(
                    "FAILED decrypt folder: uuid={}, encrypted_len={}",
                    folder.uuid,
                    folder.name.len()
                ));
            }
        }

        // Files
        for file in data.uploads {
            if let Some(meta_str) = self.decrypt_metadata(&file.metadata) {
                if let Ok(meta) = serde_json::from_str::<FileMetadata>(&meta_str) {
                    let entry_path = if base_path == "/" {
                        format!("/{}", meta.name)
                    } else {
                        format!("{}/{}", base_path, meta.name)
                    };

                    let modified = meta.last_modified.and_then(|ts| {
                        chrono::DateTime::from_timestamp(ts as i64 / 1000, 0)
                            .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
                    });

                    entries.push(RemoteEntry {
                        name: meta.name,
                        path: entry_path,
                        is_dir: false,
                        size: meta.size,
                        modified,
                        permissions: None,
                        owner: None,
                        group: None,
                        is_symlink: false,
                        link_target: None,
                        mime_type: if meta.mime.is_empty() {
                            None
                        } else {
                            Some(meta.mime)
                        },
                        metadata: {
                            let file_uuid = file.uuid.clone();
                            // Store encryption key in backend-only cache (never sent to frontend via IPC)
                            self.file_key_cache_insert(file_uuid.clone(), meta.key);
                            let mut m = HashMap::new();
                            m.insert("uuid".to_string(), file_uuid);
                            m.insert("bucket".to_string(), file.bucket);
                            m.insert("region".to_string(), file.region);
                            m.insert("chunks".to_string(), file.chunks.to_string());
                            m
                        },
                    });
                }
            } else {
                filen_log(&format!(
                    "FAILED decrypt file: uuid={}, encrypted_len={}",
                    file.uuid,
                    file.metadata.len()
                ));
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

        let folder_uuid = self.resolve_folder_uuid(&new_path).await?;
        self.current_path = new_path;
        self.current_folder_uuid = folder_uuid;
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
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        // Find the file to get its metadata (uuid, key, region, bucket, chunks)
        let normalized = Self::normalize_path(remote_path);
        let (parent_path, file_name) = match normalized.rfind('/') {
            Some(pos) if pos > 0 => (&normalized[..pos], &normalized[pos + 1..]),
            _ => ("/", normalized.trim_start_matches('/')),
        };

        let entries = self.list(parent_path).await?;
        let file_entry = entries
            .iter()
            .find(|e| !e.is_dir && e.name == file_name)
            .ok_or_else(|| ProviderError::NotFound(format!("File not found: {}", file_name)))?;

        let uuid = file_entry
            .metadata
            .get("uuid")
            .ok_or_else(|| ProviderError::Other("No UUID for file".to_string()))?
            .clone();
        // Look up encryption key from backend-only cache (not from IPC-visible metadata)
        let file_key = self
            .file_key_cache
            .get(&uuid)
            .ok_or_else(|| {
                ProviderError::Other(
                    "No encryption key in cache (re-list directory first)".to_string(),
                )
            })?
            .clone();
        let region = file_entry
            .metadata
            .get("region")
            .ok_or_else(|| ProviderError::Other("No region for file".to_string()))?
            .clone();
        let bucket = file_entry
            .metadata
            .get("bucket")
            .ok_or_else(|| ProviderError::Other("No bucket for file".to_string()))?
            .clone();
        let chunks: u32 = file_entry
            .metadata
            .get("chunks")
            .and_then(|s| s.parse().ok())
            .unwrap_or(1);
        let total_size = file_entry.size;

        // F-XFER-02: Stream each chunk download progressively to reduce peak memory.
        // Note: AES-256-GCM requires the full chunk in memory for authenticated decryption,
        // but we stream the HTTP response into a buffer instead of using resp.bytes()
        // which may hold a second copy.
        let mut atomic = super::atomic_write::AtomicFile::new(local_path)
            .await
            .map_err(ProviderError::IoError)?;
        let mut transferred: u64 = 0;

        for chunk_idx in 0..chunks {
            let download_url = format!(
                "https://egest.filen.io/{}/{}/{}/{}",
                region, bucket, uuid, chunk_idx
            );

            // Per-chunk retry. Filen's egest CDN occasionally returns a
            // truncated body or closes the TCP connection mid-response on
            // long sequential downloads (manifests as `error decoding
            // response body` from reqwest after hundreds of consecutive
            // GETs). Aborting the whole download on the first such error
            // throws away all the chunks that already landed and leaves
            // the user with no path forward, so we retry the offending
            // chunk up to FILEN_DOWNLOAD_CHUNK_RETRIES times with
            // exponential backoff before giving up. The retry is on the
            // chunk only; chunks that already succeeded are not refetched.
            let encrypted = match download_filen_chunk(
                &self.client,
                &download_url,
                chunk_idx,
                FILEN_DOWNLOAD_CHUNK_RETRIES,
            )
            .await
            {
                Ok(buf) => buf,
                Err(e) => {
                    return Err(ProviderError::TransferFailed(format!(
                        "Download chunk {}/{} failed after {} retries: {}",
                        chunk_idx, chunks, FILEN_DOWNLOAD_CHUNK_RETRIES, e,
                    )))
                }
            };

            let decrypted = Self::decrypt_file_content(&encrypted, &file_key)?;
            atomic
                .write_all(&decrypted)
                .await
                .map_err(ProviderError::IoError)?;
            transferred += decrypted.len() as u64;

            if let Some(ref progress) = on_progress {
                progress(transferred, total_size);
            }
        }

        atomic.commit().await.map_err(ProviderError::IoError)?;

        Ok(())
    }

    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        // Find the file to get its metadata (uuid, region, bucket, chunks)
        let normalized = Self::normalize_path(remote_path);
        let (parent_path, file_name) = match normalized.rfind('/') {
            Some(pos) if pos > 0 => (&normalized[..pos], &normalized[pos + 1..]),
            _ => ("/", normalized.trim_start_matches('/')),
        };

        let entries = self.list(parent_path).await?;
        let file_entry = entries
            .iter()
            .find(|e| !e.is_dir && e.name == file_name)
            .ok_or_else(|| ProviderError::NotFound(format!("File not found: {}", file_name)))?;

        let uuid = file_entry
            .metadata
            .get("uuid")
            .ok_or_else(|| ProviderError::Other("No UUID for file".to_string()))?
            .clone();
        // Look up encryption key from backend-only cache (not from IPC-visible metadata)
        let file_key = self
            .file_key_cache
            .get(&uuid)
            .ok_or_else(|| {
                ProviderError::Other(
                    "No encryption key in cache (re-list directory first)".to_string(),
                )
            })?
            .clone();
        let region = file_entry
            .metadata
            .get("region")
            .ok_or_else(|| ProviderError::Other("No region for file".to_string()))?
            .clone();
        let bucket = file_entry
            .metadata
            .get("bucket")
            .ok_or_else(|| ProviderError::Other("No bucket for file".to_string()))?
            .clone();
        let chunks: u32 = file_entry
            .metadata
            .get("chunks")
            .and_then(|s| s.parse().ok())
            .unwrap_or(1);

        // H2: Download and decrypt each chunk (with retry): with size cap
        let limit = super::MAX_DOWNLOAD_TO_BYTES;
        let mut all_data = Vec::new();
        for chunk_idx in 0..chunks {
            let download_url = format!(
                "https://egest.filen.io/{}/{}/{}/{}",
                region, bucket, uuid, chunk_idx
            );

            let encrypted = match download_filen_chunk(
                &self.client,
                &download_url,
                chunk_idx,
                FILEN_DOWNLOAD_CHUNK_RETRIES,
            )
            .await
            {
                Ok(buf) => buf,
                Err(e) => {
                    return Err(ProviderError::TransferFailed(format!(
                        "Download chunk {}/{} failed after {} retries: {}",
                        chunk_idx, chunks, FILEN_DOWNLOAD_CHUNK_RETRIES, e,
                    )))
                }
            };

            let decrypted = Self::decrypt_file_content(&encrypted, &file_key)?;
            all_data.extend_from_slice(&decrypted);

            if all_data.len() as u64 > limit {
                return Err(ProviderError::TransferFailed(format!(
                    "Download exceeded {:.0} MB size limit. Use streaming download for large files.",
                    limit as f64 / 1_048_576.0,
                )));
            }
        }

        Ok(all_data)
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

        let parent_uuid = self.resolve_folder_uuid(parent_path).await?;

        let file_metadata = tokio::fs::metadata(local_path)
            .await
            .map_err(ProviderError::IoError)?;
        let file_size = file_metadata.len();

        // Filen rejects empty files with HTTP 400 ("Invalid request"). Match
        // the official SDK behavior and surface a clear error before any I/O.
        if file_size == 0 {
            return Err(ProviderError::TransferFailed(
                "Filen does not accept empty files".to_string(),
            ));
        }

        // Hard cap. Each in-flight chunk only holds 1 MiB of plaintext + 1 MiB
        // of ciphertext, so memory is no longer the constraint. The cap exists
        // purely as a sanity guard against pathological inputs.
        if file_size > FILEN_MAX_UPLOAD_SIZE {
            return Err(ProviderError::TransferFailed(format!(
                "File too large for Filen ({:.1} GiB). Max {:.0} GiB.",
                file_size as f64 / (1024.0 * 1024.0 * 1024.0),
                FILEN_MAX_UPLOAD_SIZE as f64 / (1024.0 * 1024.0 * 1024.0),
            )));
        }

        let mime_type = mime_guess::from_path(file_name)
            .first_or_octet_stream()
            .to_string();

        let file_key: String = (0..32)
            .map(|_| format!("{:02x}", rand::random::<u8>()))
            .collect();
        let upload_key: String = (0..32)
            .map(|_| format!("{:02x}", rand::random::<u8>()))
            .collect();
        let file_uuid = uuid::Uuid::new_v4().to_string();

        // ceil_div(file_size, CHUNK_SIZE), guaranteed >= 1 since size > 0.
        let total_chunks: u64 = file_size.div_ceil(FILEN_CHUNK_SIZE as u64);

        filen_log(&format!(
            "upload begin '{}' size={} chunks={} parallel={}",
            file_name, file_size, total_chunks, FILEN_PARALLEL_CHUNK_UPLOADS,
        ));

        // Read sequentially from disk and dispatch encrypt+POST in parallel.
        // The reader holds at most one plaintext buffer in memory at a time;
        // the FuturesUnordered set holds up to FILEN_PARALLEL_CHUNK_UPLOADS
        // in-flight encrypted chunks. Memory peak is bounded by the parallel
        // limit, independent of file size.
        use futures_util::stream::FuturesUnordered;
        use tokio::io::AsyncReadExt;
        let mut file = tokio::fs::File::open(local_path)
            .await
            .map_err(ProviderError::IoError)?;

        let mut in_flight: FuturesUnordered<_> = FuturesUnordered::new();
        let mut next_index: u64 = 0;
        let mut transferred: u64 = 0;
        let mut eof = false;

        loop {
            // Top up in_flight up to the parallel cap, draining each plaintext
            // chunk from disk just before we hand it to a worker.
            while !eof
                && in_flight.len() < FILEN_PARALLEL_CHUNK_UPLOADS
                && next_index < total_chunks
            {
                let mut buf = vec![0u8; FILEN_CHUNK_SIZE];
                let mut read_total = 0;
                while read_total < FILEN_CHUNK_SIZE {
                    match file.read(&mut buf[read_total..]).await {
                        Ok(0) => break,
                        Ok(n) => read_total += n,
                        Err(e) => return Err(ProviderError::IoError(e)),
                    }
                }
                buf.truncate(read_total);
                if buf.is_empty() {
                    eof = true;
                    break;
                }

                let index = next_index;
                next_index += 1;
                let plaintext_len = buf.len() as u64;
                let client = self.client.clone();
                let api_key = self.auth.api_key.clone();
                let ingest_base = self.ingest_base().to_string();
                let file_uuid = file_uuid.clone();
                let parent_uuid = parent_uuid.clone();
                let upload_key = upload_key.clone();
                let file_key = file_key.clone();
                in_flight.push(async move {
                    upload_filen_chunk(
                        &client,
                        &api_key,
                        &ingest_base,
                        &file_uuid,
                        &parent_uuid,
                        &upload_key,
                        &file_key,
                        index,
                        buf,
                    )
                    .await?;
                    Ok::<u64, ProviderError>(plaintext_len)
                });
            }

            match in_flight.next().await {
                Some(Ok(n)) => {
                    transferred += n;
                    if let Some(ref cb) = on_progress {
                        cb(transferred, file_size);
                    }
                }
                Some(Err(e)) => return Err(e),
                None => break,
            }
        }

        if next_index != total_chunks {
            return Err(ProviderError::TransferFailed(format!(
                "Filen upload chunked count mismatch: expected {} chunks, produced {}",
                total_chunks, next_index,
            )));
        }

        // Encrypt metadata using the file size in plaintext bytes (matches
        // what the official SDK and the existing download path expect).
        let last_modified = Self::source_last_modified_ms(Some(&file_metadata));
        let metadata = serde_json::json!({
            "name": file_name,
            "size": file_size,
            "mime": mime_type,
            "key": file_key,
            "lastModified": last_modified,
        });
        let encrypted_metadata = self.encrypt_metadata(&metadata.to_string())?;
        let encrypted_name = self.encrypt_metadata(file_name)?;
        let encrypted_size = self.encrypt_metadata(&file_size.to_string())?;
        let name_hashed = Self::hash_name(file_name);

        // Random rm parameter (matches official SDK; opaque token consumed by
        // /v3/upload/done).
        let rm: String = (0..32)
            .map(|_| format!("{:02x}", rand::random::<u8>()))
            .collect();

        let done_request = self
            .client
            .post(format!("{}/v3/upload/done", self.gateway_base()))
            .header(
                "Authorization",
                HeaderValue::from_str(&format!("Bearer {}", self.auth.api_key.expose_secret()))
                    .map_err(|e| ProviderError::Other(format!("Invalid auth header: {}", e)))?,
            )
            .header(CONTENT_TYPE, "application/json")
            .json(&serde_json::json!({
                "uuid": file_uuid,
                "name": encrypted_name,
                "nameHashed": name_hashed,
                "size": encrypted_size,
                "chunks": total_chunks,
                "mime": mime_type,
                "rm": rm,
                "metadata": encrypted_metadata,
                "version": 2,
                "uploadKey": upload_key,
            }))
            .build()
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
        let done_resp: serde_json::Value = self
            .send_retry(done_request)
            .await?
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        if done_resp["status"].as_bool() != Some(true) {
            return Err(ProviderError::TransferFailed(
                done_resp["message"]
                    .as_str()
                    .unwrap_or("Upload finalization failed")
                    .to_string(),
            ));
        }

        Ok(())
    }

    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let normalized = Self::normalize_path(path);
        let (parent_path, folder_name) = match normalized.rfind('/') {
            Some(pos) if pos > 0 => (&normalized[..pos], &normalized[pos + 1..]),
            _ => ("/", normalized.trim_start_matches('/')),
        };

        let parent_uuid = self.resolve_folder_uuid(parent_path).await?;
        let folder_uuid = uuid::Uuid::new_v4().to_string();

        // Filen SDK wraps folder name in JSON: {"name":"folder_name"}
        let name_json = serde_json::json!({"name": folder_name}).to_string();
        let encrypted_name = self.encrypt_metadata(&name_json)?;

        let request = self
            .client
            .post(format!("{}/v3/dir/create", self.gateway_base()))
            .header(
                "Authorization",
                HeaderValue::from_str(&format!("Bearer {}", self.auth.api_key.expose_secret()))
                    .map_err(|e| ProviderError::Other(format!("Invalid auth header: {}", e)))?,
            )
            .json(&serde_json::json!({
                "uuid": folder_uuid,
                "name": encrypted_name,
                "nameHashed": Self::hash_name(folder_name),
                "parent": parent_uuid,
            }))
            .build()
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
        let resp: CreateFolderResponse = self
            .send_retry(request)
            .await?
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        if !resp.status {
            let msg = resp.message.unwrap_or_else(|| "mkdir failed".to_string());
            filen_log(&format!("mkdir FAILED '{}': {}", path, msg));
            return Err(ProviderError::Other(msg));
        }

        // Call v3/dir/metadata for each master key (required for Filen webapp compatibility)
        let master_keys_exposed: Vec<String> = self
            .auth
            .master_keys
            .iter()
            .map(|k| k.expose_secret().to_string())
            .collect();
        for key in &master_keys_exposed {
            let encrypted_for_key = Self::encrypt_metadata_with_key(&name_json, key)?;
            let meta_request = self
                .client
                .post(format!("{}/v3/dir/metadata", self.gateway_base()))
                .header(
                    "Authorization",
                    HeaderValue::from_str(&format!("Bearer {}", self.auth.api_key.expose_secret()))
                        .map_err(|e| ProviderError::Other(format!("Invalid auth header: {}", e)))?,
                )
                .json(&serde_json::json!({
                    "uuid": folder_uuid,
                    "encrypted": encrypted_for_key,
                }))
                .build()
                .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
            let _ = self.send_retry(meta_request).await;
        }

        filen_log(&format!("mkdir OK '{}' uuid={}", path, folder_uuid));
        Ok(())
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        // Find the file UUID
        let normalized = Self::normalize_path(path);
        let (parent_path, file_name) = match normalized.rfind('/') {
            Some(pos) if pos > 0 => (&normalized[..pos], &normalized[pos + 1..]),
            _ => ("/", normalized.trim_start_matches('/')),
        };

        let entries = self.list(parent_path).await?;
        let entry = entries
            .iter()
            .find(|e| e.name == file_name)
            .ok_or_else(|| ProviderError::NotFound(file_name.to_string()))?;

        let uuid = entry
            .metadata
            .get("uuid")
            .ok_or_else(|| ProviderError::Other("No UUID".to_string()))?;

        let endpoint = if entry.is_dir {
            "v3/dir/trash"
        } else {
            "v3/file/trash"
        };

        let request = self
            .client
            .post(format!("{}/{}", self.gateway_base(), endpoint))
            .header(
                "Authorization",
                HeaderValue::from_str(&format!("Bearer {}", self.auth.api_key.expose_secret()))
                    .map_err(|e| ProviderError::Other(format!("Invalid auth header: {}", e)))?,
            )
            .json(&serde_json::json!({"uuid": uuid}))
            .build()
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
        let resp: GenericResponse = self
            .send_retry(request)
            .await?
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        if !resp.status {
            return Err(ProviderError::Other(
                resp.message.unwrap_or_else(|| "Delete failed".to_string()),
            ));
        }

        Ok(())
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let folder_uuid = self.resolve_folder_uuid(path).await?;

        let request = self
            .client
            .post(format!("{}/v3/dir/trash", self.gateway_base()))
            .header(
                "Authorization",
                HeaderValue::from_str(&format!("Bearer {}", self.auth.api_key.expose_secret()))
                    .map_err(|e| ProviderError::Other(format!("Invalid auth header: {}", e)))?,
            )
            .json(&serde_json::json!({"uuid": folder_uuid}))
            .build()
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
        let resp: GenericResponse = self
            .send_retry(request)
            .await?
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        if !resp.status {
            return Err(ProviderError::Other(
                resp.message.unwrap_or_else(|| "rmdir failed".to_string()),
            ));
        }

        // Clear from cache
        let normalized = Self::normalize_path(path);
        self.dir_cache.remove(&normalized);

        Ok(())
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        self.rmdir(path).await // Filen trash handles recursive
    }

    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let new_name = std::path::Path::new(to)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| to.to_string());

        // Find the item
        let normalized = Self::normalize_path(from);
        let (parent_path, old_name) = match normalized.rfind('/') {
            Some(pos) if pos > 0 => (&normalized[..pos], &normalized[pos + 1..]),
            _ => ("/", normalized.trim_start_matches('/')),
        };

        let entries = self.list(parent_path).await?;
        let entry = entries
            .iter()
            .find(|e| e.name == old_name)
            .ok_or_else(|| ProviderError::NotFound(old_name.to_string()))?;

        let uuid = entry
            .metadata
            .get("uuid")
            .ok_or_else(|| ProviderError::Other("No UUID".to_string()))?;

        filen_log(&format!(
            "rename: '{}' -> '{}', is_dir={}, uuid={}",
            from, to, entry.is_dir, uuid
        ));

        let name_hashed = Self::hash_name(&new_name);

        if entry.is_dir {
            // Folder rename: name is JSON {"name":"..."}, also call dir/metadata
            let name_json = serde_json::json!({"name": new_name}).to_string();
            let encrypted_name = self.encrypt_metadata(&name_json)?;

            let request = self
                .client
                .post(format!("{}/v3/dir/rename", self.gateway_base()))
                .header(
                    "Authorization",
                    HeaderValue::from_str(&format!("Bearer {}", self.auth.api_key.expose_secret()))
                        .map_err(|e| ProviderError::Other(format!("Invalid auth header: {}", e)))?,
                )
                .json(&serde_json::json!({
                    "uuid": uuid,
                    "name": encrypted_name,
                    "nameHashed": name_hashed,
                }))
                .build()
                .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
            let resp: GenericResponse = self
                .send_retry(request)
                .await?
                .json()
                .await
                .map_err(|e| ProviderError::ParseError(e.to_string()))?;

            if !resp.status {
                let msg = resp.message.unwrap_or_else(|| "Rename failed".to_string());
                filen_log(&format!("rename dir FAILED: {}", msg));
                return Err(ProviderError::Other(msg));
            }

            // Update dir/metadata for webapp compatibility
            let master_keys_exposed: Vec<String> = self
                .auth
                .master_keys
                .iter()
                .map(|k| k.expose_secret().to_string())
                .collect();
            for key in &master_keys_exposed {
                let enc = Self::encrypt_metadata_with_key(&name_json, key)?;
                let meta_request = self
                    .client
                    .post(format!("{}/v3/dir/metadata", self.gateway_base()))
                    .header(
                        "Authorization",
                        HeaderValue::from_str(&format!(
                            "Bearer {}",
                            self.auth.api_key.expose_secret()
                        ))
                        .map_err(|e| ProviderError::Other(format!("Invalid auth header: {}", e)))?,
                    )
                    .json(&serde_json::json!({"uuid": uuid, "encrypted": enc}))
                    .build()
                    .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
                let _ = self.send_retry(meta_request).await;
            }
        } else {
            // File rename: need encrypted name + metadata JSON with updated name
            let encrypted_name = self.encrypt_metadata(&new_name)?;
            // H4: Reject empty key: using an empty key would produce a ciphertext
            // that any attacker could decrypt. Require re-listing the directory.
            let file_key = self.file_key_cache.get(uuid).cloned().ok_or_else(|| {
                ProviderError::Other(
                    "No encryption key in cache for file rename (re-list directory first)"
                        .to_string(),
                )
            })?;
            if file_key.is_empty() {
                return Err(ProviderError::Other(
                    "Empty encryption key for file: cannot rename safely".to_string(),
                ));
            }
            let mime = entry
                .mime_type
                .clone()
                .unwrap_or_else(|| "application/octet-stream".to_string());
            // A rename changes the name, not the contents: carry the mtime the
            // file already had. Stamping "now" here re-dated every renamed file
            // and put it out of sync with its unchanged local twin.
            let last_modified = Self::entry_last_modified_ms(entry.modified.as_deref())
                .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());

            let meta_json = serde_json::json!({
                "name": new_name,
                "size": entry.size,
                "mime": mime,
                "key": file_key,
                "lastModified": last_modified,
            });
            let encrypted_metadata = self.encrypt_metadata(&meta_json.to_string())?;

            let request = self
                .client
                .post(format!("{}/v3/file/rename", self.gateway_base()))
                .header(
                    "Authorization",
                    HeaderValue::from_str(&format!("Bearer {}", self.auth.api_key.expose_secret()))
                        .map_err(|e| ProviderError::Other(format!("Invalid auth header: {}", e)))?,
                )
                .json(&serde_json::json!({
                    "uuid": uuid,
                    "name": encrypted_name,
                    "nameHashed": name_hashed,
                    "metadata": encrypted_metadata,
                }))
                .build()
                .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
            let resp: GenericResponse = self
                .send_retry(request)
                .await?
                .json()
                .await
                .map_err(|e| ProviderError::ParseError(e.to_string()))?;

            if !resp.status {
                let msg = resp.message.unwrap_or_else(|| "Rename failed".to_string());
                filen_log(&format!("rename file FAILED: {}", msg));
                return Err(ProviderError::Other(msg));
            }
        }

        Ok(())
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        let normalized = Self::normalize_path(path);
        let (parent_path, name) = match normalized.rfind('/') {
            Some(pos) if pos > 0 => (&normalized[..pos], &normalized[pos + 1..]),
            _ => ("/", normalized.trim_start_matches('/')),
        };

        let entries = self.list(parent_path).await?;
        entries
            .into_iter()
            .find(|e| e.name == name)
            .ok_or_else(|| ProviderError::NotFound(name.to_string()))
    }

    async fn exists(&mut self, path: &str) -> Result<bool, ProviderError> {
        match self.stat(path).await {
            Ok(_) => Ok(true),
            Err(ProviderError::NotFound(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    async fn size(&mut self, path: &str) -> Result<u64, ProviderError> {
        let entry = self.stat(path).await?;
        Ok(entry.size)
    }

    async fn keep_alive(&mut self) -> Result<(), ProviderError> {
        Ok(())
    }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        Ok("Filen.io (E2E Encrypted Cloud Storage)".to_string())
    }

    async fn storage_info(&mut self) -> Result<StorageInfo, ProviderError> {
        // W2.2 (#275): when the Filen CLI is installed, `filen statfs` is a
        // cleaner quota source than the REST call. Opportunistic and guarded:
        // any failure or implausible output falls through to the REST path
        // below. The CLI reports whatever account it is logged into, so this
        // is best-effort, never a hard dependency.
        if let Ok((used, total)) = statfs::filen_statfs_query().await {
            return Ok(StorageInfo {
                total,
                used,
                free: total.saturating_sub(used),
                versioning_bytes: None,
            });
        }

        let request = self
            .client
            .get(format!("{}/v3/user/info", self.gateway_base()))
            .header(
                "Authorization",
                HeaderValue::from_str(&format!("Bearer {}", self.auth.api_key.expose_secret()))
                    .map_err(|e| ProviderError::Other(format!("Invalid auth header: {}", e)))?,
            )
            .build()
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
        let resp: UserInfoResponse = self
            .send_retry(request)
            .await?
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        let data = resp
            .data
            .ok_or_else(|| ProviderError::Other("No user info data".to_string()))?;

        Ok(StorageInfo {
            total: data.max_storage,
            used: data.storage_used,
            free: data.max_storage.saturating_sub(data.storage_used),
            versioning_bytes: None,
        })
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
        // Find file/folder UUID from path
        let normalized = Self::normalize_path(path);
        let (parent_path, name) = match normalized.rfind('/') {
            Some(pos) if pos > 0 => (&normalized[..pos], &normalized[pos + 1..]),
            _ => ("/", normalized.trim_start_matches('/')),
        };

        let entries = self.list(parent_path).await?;
        let entry = entries
            .iter()
            .find(|e| e.name == name)
            .ok_or_else(|| ProviderError::NotFound(name.to_string()))?;

        let uuid = entry
            .metadata
            .get("uuid")
            .ok_or_else(|| ProviderError::Other("No UUID".to_string()))?;

        let endpoint = if entry.is_dir {
            "v3/dir/link/edit"
        } else {
            "v3/file/link/edit"
        };

        // Generate a link UUID
        let link_uuid = uuid::Uuid::new_v4().to_string();

        // F-SHARE-01: Generate a link key for the recipient to decrypt the shared content.
        // The link key is the first master key, which is used to encrypt the shared metadata.
        let link_key = self
            .auth
            .master_keys
            .first()
            .map(|k| k.expose_secret().to_string())
            .unwrap_or_default();

        // Map expires_in_secs to Filen preset: "never","1h","6h","1d","3d","7d","14d","30d"
        let expiration = match options.expires_in_secs {
            None => "never".to_string(),
            Some(secs) => if secs <= 3600 {
                "1h"
            } else if secs <= 21600 {
                "6h"
            } else if secs <= 86400 {
                "1d"
            } else if secs <= 259200 {
                "3d"
            } else if secs <= 604800 {
                "7d"
            } else if secs <= 1209600 {
                "14d"
            } else {
                "30d"
            }
            .to_string(),
        };

        // Filen password hashing: Argon2id v3 (client-side, zero-knowledge).
        //
        // The v3 API requires a 128-byte hex salt regardless of whether the
        // link is password-protected. Sending `""` as salt for a passwordless
        // link triggers an "Invalid salt" server-side validation error, so we
        // always generate a random salt even when no password is set.
        let salt_bytes: [u8; 128] = {
            let mut buf = [0u8; 128];
            use rand::RngCore;
            rand::thread_rng().fill_bytes(&mut buf);
            buf
        };
        let salt_hex: String = salt_bytes.iter().map(|b| format!("{:02x}", b)).collect();

        let (password_raw, password_hashed) = if let Some(ref pw) = options.password {
            let params = argon2::Params::new(65536, 3, 4, Some(64))
                .map_err(|e| ProviderError::Other(format!("Argon2 params: {}", e)))?;
            let argon2 =
                argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
            let mut hash_out = [0u8; 64];
            argon2
                .hash_password_into(pw.as_bytes(), &salt_bytes, &mut hash_out)
                .map_err(|e| ProviderError::Other(format!("Argon2 hash: {}", e)))?;
            let hash_hex: String = hash_out.iter().map(|b| format!("{:02x}", b)).collect();
            (pw.clone(), hash_hex)
        } else {
            ("empty".to_string(), "empty".to_string())
        };

        let mut link_body = serde_json::json!({
            "uuid": uuid,
            "linkUUID": link_uuid,
            "expiration": expiration,
            "password": password_raw,
            "passwordHashed": password_hashed,
            "salt": salt_hex,
            "downloadBtn": true,
            "type": "enable",
        });
        // fileUUID required by API for file links
        if !entry.is_dir {
            link_body["fileUUID"] = serde_json::json!(uuid);
        }

        let request = self
            .client
            .post(format!("{}/{}", self.gateway_base(), endpoint))
            .header(
                "Authorization",
                HeaderValue::from_str(&format!("Bearer {}", self.auth.api_key.expose_secret()))
                    .map_err(|e| ProviderError::Other(format!("Invalid auth header: {}", e)))?,
            )
            .json(&link_body)
            .build()
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
        let resp: LinkEditResponse = self
            .send_retry(request)
            .await?
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        if !resp.status {
            return Err(ProviderError::Other(
                resp.message
                    .unwrap_or_else(|| "Failed to create share link".to_string()),
            ));
        }

        // F-SHARE-01: Append #<linkKey> fragment so recipients can decrypt shared content.
        // M8 SECURITY WARNING: The link key IS the user's first master key.
        let url = if link_key.is_empty() {
            format!("https://filen.io/d/{}", link_uuid)
        } else {
            format!("https://filen.io/d/{}#{}", link_uuid, link_key)
        };
        Ok(ShareLinkResult {
            url,
            password: None,
            expires_at: None,
        })
    }

    async fn remove_share_link(&mut self, path: &str) -> Result<(), ProviderError> {
        let normalized = Self::normalize_path(path);
        let (parent_path, name) = match normalized.rfind('/') {
            Some(pos) if pos > 0 => (&normalized[..pos], &normalized[pos + 1..]),
            _ => ("/", normalized.trim_start_matches('/')),
        };

        let entries = self.list(parent_path).await?;
        let entry = entries
            .iter()
            .find(|e| e.name == name)
            .ok_or_else(|| ProviderError::NotFound(name.to_string()))?;

        let uuid = entry
            .metadata
            .get("uuid")
            .ok_or_else(|| ProviderError::Other("No UUID".to_string()))?;

        let endpoint = if entry.is_dir {
            "v3/dir/link/edit"
        } else {
            "v3/file/link/edit"
        };

        let request = self
            .client
            .post(format!("{}/{}", self.gateway_base(), endpoint))
            .header(
                "Authorization",
                HeaderValue::from_str(&format!("Bearer {}", self.auth.api_key.expose_secret()))
                    .map_err(|e| ProviderError::Other(format!("Invalid auth header: {}", e)))?,
            )
            .json(&serde_json::json!({
                "uuid": uuid,
                "type": "disable",
            }))
            .build()
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
        let resp: GenericResponse = self
            .send_retry(request)
            .await?
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        if !resp.status {
            return Err(ProviderError::Other(
                resp.message
                    .unwrap_or_else(|| "Failed to remove share link".to_string()),
            ));
        }
        Ok(())
    }

    // TODO (F-FEAT-01): Filen supports trash operations via v3/file/trash and v3/dir/trash
    // (already used in delete/rmdir). To implement list_trash/restore_from_trash/permanent_delete,
    // use: GET v3/trash (list trash items), POST v3/file/restore / v3/dir/restore (restore),
    // POST v3/file/delete/permanent / v3/dir/delete/permanent (permanent delete).

    // F-FEAT-02: Filen file versioning. Versioning is server-side and always on
    // for Filen accounts; AeroFTP only needs the four generic trait methods. The
    // FileVersionsDialog frontend and provider_* commands are protocol-agnostic.
    fn supports_versions(&self) -> bool {
        true
    }

    async fn list_versions(
        &mut self,
        path: &str,
    ) -> Result<Vec<super::FileVersion>, ProviderError> {
        let file_uuid = self.resolve_file_uuid(path).await?;
        let versions = self.fetch_file_versions(&file_uuid).await?;

        let mut out = Vec::with_capacity(versions.len());
        for v in versions {
            // Each version's own metadata carries its size + content mtime,
            // encrypted under the master key like any file's metadata.
            let (size, content_modified) = match self.decrypt_metadata(&v.metadata) {
                Some(meta_str) => match serde_json::from_str::<FileMetadata>(&meta_str) {
                    Ok(meta) => {
                        let modified = meta.last_modified.and_then(|ts| {
                            chrono::DateTime::from_timestamp(ts as i64 / 1000, 0)
                                .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
                        });
                        (meta.size, modified)
                    }
                    Err(_) => (0, None),
                },
                None => (0, None),
            };
            // Prefer the content mtime; fall back to the version record's own
            // creation timestamp (seconds) when metadata carried none.
            let modified = content_modified.or_else(|| {
                chrono::DateTime::from_timestamp(v.timestamp as i64, 0)
                    .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
            });
            out.push(super::FileVersion {
                id: v.uuid,
                modified,
                size,
                modified_by: None,
            });
        }
        Ok(out)
    }

    async fn download_version(
        &mut self,
        path: &str,
        version_id: &str,
        local_path: &str,
    ) -> Result<(), ProviderError> {
        let file_uuid = self.resolve_file_uuid(path).await?;
        let version = self
            .fetch_file_versions(&file_uuid)
            .await?
            .into_iter()
            .find(|v| v.uuid == version_id)
            .ok_or_else(|| ProviderError::NotFound(format!("Version not found: {}", version_id)))?;

        // The version's per-file key lives inside its own encrypted metadata.
        let meta_str = self.decrypt_metadata(&version.metadata).ok_or_else(|| {
            ProviderError::Other("Failed to decrypt version metadata".to_string())
        })?;
        let meta: FileMetadata = serde_json::from_str(&meta_str)
            .map_err(|e| ProviderError::ParseError(format!("Version metadata parse: {}", e)))?;
        let file_key = meta.key;

        let mut atomic = super::atomic_write::AtomicFile::new(local_path)
            .await
            .map_err(ProviderError::IoError)?;

        // A zero-chunk version is an empty file: commit the empty atomic file.
        for chunk_idx in 0..version.chunks {
            let download_url = format!(
                "https://egest.filen.io/{}/{}/{}/{}",
                version.region, version.bucket, version.uuid, chunk_idx
            );
            let encrypted = download_filen_chunk(
                &self.client,
                &download_url,
                chunk_idx,
                FILEN_DOWNLOAD_CHUNK_RETRIES,
            )
            .await
            .map_err(|e| {
                ProviderError::TransferFailed(format!(
                    "Download version chunk {}/{} failed after {} retries: {}",
                    chunk_idx, version.chunks, FILEN_DOWNLOAD_CHUNK_RETRIES, e
                ))
            })?;
            let decrypted = Self::decrypt_file_content(&encrypted, &file_key)?;
            atomic
                .write_all(&decrypted)
                .await
                .map_err(ProviderError::IoError)?;
        }

        atomic.commit().await.map_err(ProviderError::IoError)?;
        Ok(())
    }

    async fn restore_version(&mut self, path: &str, version_id: &str) -> Result<(), ProviderError> {
        let current_uuid = self.resolve_file_uuid(path).await?;
        // POST /v3/file/version/restore { uuid: <version>, current: <live file> }
        // (filen-sdk-ts api/v3/file/version/restore).
        let request = self
            .client
            .post(format!("{}/v3/file/version/restore", self.gateway_base()))
            .header(
                "Authorization",
                HeaderValue::from_str(&format!("Bearer {}", self.auth.api_key.expose_secret()))
                    .map_err(|e| ProviderError::Other(format!("Invalid auth header: {}", e)))?,
            )
            .json(&serde_json::json!({ "uuid": version_id, "current": current_uuid }))
            .build()
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
        let resp: GenericResponse = self
            .send_retry(request)
            .await?
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;
        if !resp.status {
            return Err(ProviderError::ServerError(
                resp.message
                    .unwrap_or_else(|| "Failed to restore file version".to_string()),
            ));
        }
        Ok(())
    }

    fn supports_find(&self) -> bool {
        true
    }

    async fn find(&mut self, path: &str, pattern: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        // TODO (F-FEAT-03): Filen has no server-side search API due to zero-knowledge design.
        // The current implementation recursively lists and decrypts directories client-side.
        // This is inherently slow for large directory trees. Consider caching the decrypted
        // directory tree for faster subsequent searches.
        let mut results = Vec::new();
        let mut dirs_to_scan = vec![if path == "." || path.is_empty() {
            self.current_path.clone()
        } else {
            Self::normalize_path(path)
        }];

        let max_depth = 10;
        let mut depth = 0;

        while !dirs_to_scan.is_empty() && depth < max_depth {
            depth += 1;
            let mut next_dirs = Vec::new();

            for dir in &dirs_to_scan {
                if let Ok(entries) = self.list(dir).await {
                    for entry in entries {
                        if crate::providers::matches_find_pattern(&entry.name, pattern) {
                            results.push(entry.clone());
                        }
                        if entry.is_dir && results.len() < 500 {
                            next_dirs.push(entry.path.clone());
                        }
                    }
                }
                if results.len() >= 500 {
                    break;
                }
            }

            dirs_to_scan = next_dirs;
        }

        Ok(results)
    }

    fn transfer_optimization_hints(&self) -> super::TransferOptimizationHints {
        super::TransferOptimizationHints {
            supports_multipart: true,
            // Filen rejects empty files; surface the chunked path from the
            // first non-zero byte.
            multipart_threshold: FILEN_CHUNK_SIZE as u64,
            multipart_part_size: FILEN_CHUNK_SIZE as u64,
            // upload_filen_chunk uses an independent POST per chunk
            // against `/v3/upload?index=N`; chunks parallelise freely.
            // Match the legacy FILEN_PARALLEL_CHUNK_UPLOADS cap so the
            // fan-out does not exceed Filen's per-session connection
            // budget.
            multipart_max_parallel: FILEN_PARALLEL_CHUNK_UPLOADS as u8,
            supports_resume_download: true,
            supports_resume_upload: true,
            ..Default::default()
        }
    }

    // DAG-P1-05D: each shaped part is independently encrypted and POSTed to
    // ingest with the opaque handle's crypto snapshot, so part workers may
    // run on connected clones that share the Arc auth/config snapshot and
    // their own reqwest client. Begin and complete stay primary-owned;
    // abort is a documented no-op.
    fn transfer_executor_kind(&self) -> ProviderTransferExecutorKind {
        ProviderTransferExecutorKind::HttpClonePool
    }

    fn transfer_executor_max_sessions(&self) -> u16 {
        FILEN_TRANSFER_MAX_SESSIONS
    }

    fn clone_for_transfer(&self) -> Result<Box<dyn StorageProvider>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        Ok(Box::new(self.clone_transfer_worker()))
    }

    // Shaped-graph multipart trait wiring (S3-T02).
    //
    // Filen's v3 chunked upload maps onto the trait as:
    //   1. `begin_multipart_upload` → no server round-trip: generate the
    //      per-upload random `file_uuid`, `file_key`, `upload_key`,
    //      resolve the parent folder UUID, and stash everything in the
    //      handle alongside the total / part-size / total_chunks
    //      derived from `FILEN_CHUNK_SIZE`.
    //   2. `upload_part` → reuse the existing `upload_filen_chunk`
    //      helper (AES-256-GCM encryption + SHA-512 chunk hash + POST
    //      `/v3/upload?uuid=&index=&parent=&uploadKey=&hash=`). The
    //      `index` field uses 0-based numbering, so the trait's
    //      1-based `part_number` is decremented before the call.
    //   3. `complete_multipart_upload` → POST `/v3/upload/done` with
    //      the encrypted metadata bundle (name, size, mime, key,
    //      lastModified) plus the original `upload_key` and `rm`
    //      tokens. Mirrors the legacy `upload()` finaliser byte-for-byte.
    //   4. `abort_multipart_upload` → no-op. Filen GCs orphaned uploads
    //      automatically.
    //
    // Per-chunk nonces are random (12 bytes from `rand::random`) per
    // `encrypt_file_content`. Retries from the runner always re-encrypt
    // with a fresh nonce, matching the legacy upload() retry semantics.
    async fn begin_multipart_upload(
        &mut self,
        remote_path: &str,
        total_size: u64,
        _content_type: Option<&str>,
        local_source_path: Option<&str>,
    ) -> Result<MultipartHandle, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        if total_size == 0 {
            return Err(ProviderError::TransferFailed(
                "Filen does not accept empty files".to_string(),
            ));
        }
        if total_size > FILEN_MAX_UPLOAD_SIZE {
            return Err(ProviderError::TransferFailed(format!(
                "File too large for Filen ({:.1} GiB). Max {:.0} GiB.",
                total_size as f64 / (1024.0 * 1024.0 * 1024.0),
                FILEN_MAX_UPLOAD_SIZE as f64 / (1024.0 * 1024.0 * 1024.0),
            )));
        }

        let normalized = Self::normalize_path(remote_path);
        let (parent_path, file_name) = match normalized.rfind('/') {
            Some(pos) if pos > 0 => (&normalized[..pos], &normalized[pos + 1..]),
            _ => ("/", normalized.trim_start_matches('/')),
        };
        if file_name.is_empty() {
            return Err(ProviderError::InvalidPath(format!(
                "Filen multipart: cannot resolve file name from path '{}'",
                remote_path
            )));
        }

        let parent_uuid = self.resolve_folder_uuid(parent_path).await?;

        let file_key: String = (0..32)
            .map(|_| format!("{:02x}", rand::random::<u8>()))
            .collect();
        let upload_key: String = (0..32)
            .map(|_| format!("{:02x}", rand::random::<u8>()))
            .collect();
        let file_uuid = uuid::Uuid::new_v4().to_string();
        let mime = mime_guess::from_path(file_name)
            .first_or_octet_stream()
            .to_string();
        let part = filen_runner_part_size(total_size);
        let total_chunks = total_size.div_ceil(part);

        // Capture the source mtime now, while the local file is still the one
        // being read: the commit runs after every part has landed and has no
        // access to it. The runner does not always know the source path (a
        // stream with no file behind it), in which case the commit stamps its
        // own time, which is the only thing left to stamp.
        let last_modified_ms = match local_source_path {
            Some(p) => tokio::fs::metadata(p)
                .await
                .ok()
                .map(|m| Self::source_last_modified_ms(Some(&m))),
            None => None,
        };

        let meta = FilenMultipartMeta {
            file_uuid,
            parent_uuid,
            file_key,
            upload_key,
            file_name: file_name.to_string(),
            mime,
            total: total_size,
            part,
            total_chunks,
            last_modified_ms,
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
                "Filen upload_part requires 1-based part_number".to_string(),
            ));
        }
        if data.is_empty() {
            return Err(ProviderError::Other(
                "Filen upload_part received empty data".to_string(),
            ));
        }
        let meta = FilenMultipartMeta::decode(&handle.upload_id)?;
        let zero_index = (part_number as u64) - 1;
        if zero_index >= meta.total_chunks {
            return Err(ProviderError::Other(format!(
                "Filen part {} exceeds declared total_chunks {}",
                part_number, meta.total_chunks
            )));
        }
        let offset = zero_index * meta.part;
        let end = offset
            .checked_add(data.len() as u64)
            .ok_or_else(|| ProviderError::Other("Filen part offset overflow".to_string()))?;
        if end > meta.total {
            return Err(ProviderError::Other(format!(
                "Filen part {} exceeds declared total: offset {} + len {} > total {}",
                part_number,
                offset,
                data.len(),
                meta.total
            )));
        }

        upload_filen_chunk(
            &self.client,
            &self.auth.api_key,
            self.ingest_base(),
            &meta.file_uuid,
            &meta.parent_uuid,
            &meta.upload_key,
            &meta.file_key,
            zero_index,
            data,
        )
        .await?;

        Ok(UploadedPart {
            part_number,
            etag: String::new(),
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
        let meta = FilenMultipartMeta::decode(&handle.upload_id)?;
        if parts.len() != meta.total_chunks as usize {
            return Err(ProviderError::TransferFailed(format!(
                "Filen complete: expected {} chunks, runner committed {}",
                meta.total_chunks,
                parts.len()
            )));
        }

        // Build the same /v3/upload/done envelope as the legacy upload()
        // path so the file metadata and the file table stay byte-compatible,
        // including the source mtime the handle carried from the session start.
        let last_modified = meta
            .last_modified_ms
            .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
        let metadata = serde_json::json!({
            "name": meta.file_name,
            "size": meta.total,
            "mime": meta.mime,
            "key": meta.file_key,
            "lastModified": last_modified,
        });
        let encrypted_metadata = self.encrypt_metadata(&metadata.to_string())?;
        let encrypted_name = self.encrypt_metadata(&meta.file_name)?;
        let encrypted_size = self.encrypt_metadata(&meta.total.to_string())?;
        let name_hashed = Self::hash_name(&meta.file_name);

        let rm: String = (0..32)
            .map(|_| format!("{:02x}", rand::random::<u8>()))
            .collect();

        let done_request = self
            .client
            .post(format!("{}/v3/upload/done", self.gateway_base()))
            .header(
                "Authorization",
                HeaderValue::from_str(&format!("Bearer {}", self.auth.api_key.expose_secret()))
                    .map_err(|e| ProviderError::Other(format!("Invalid auth header: {}", e)))?,
            )
            .header(CONTENT_TYPE, "application/json")
            .json(&serde_json::json!({
                "uuid": meta.file_uuid,
                "name": encrypted_name,
                "nameHashed": name_hashed,
                "size": encrypted_size,
                "chunks": meta.total_chunks,
                "mime": meta.mime,
                "rm": rm,
                "metadata": encrypted_metadata,
                "version": 2,
                "uploadKey": meta.upload_key,
            }))
            .build()
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
        let done_resp: serde_json::Value = self
            .send_retry(done_request)
            .await?
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        if done_resp["status"].as_bool() != Some(true) {
            return Err(ProviderError::TransferFailed(
                done_resp["message"]
                    .as_str()
                    .unwrap_or("Upload finalization failed")
                    .to_string(),
            ));
        }
        Ok(())
    }

    async fn abort_multipart_upload(
        &mut self,
        _handle: MultipartHandle,
    ) -> Result<(), ProviderError> {
        // Filen has no documented abort endpoint for incomplete uploads;
        // orphans are GCed by the gateway after their TTL expires.
        Ok(())
    }
}

/// Redact Filen secrets that may appear in transport/URL error strings.
///
/// The ingest URL carries `uploadKey=` in the query string; a reqwest error can
/// echo the full URL. Authorization stays in the header (never the URL), but we
/// still scrub bearer tokens and known secret-bearing query keys.
fn redact_filen_secrets_in_text(input: &str) -> String {
    let mut out = input.to_string();
    // Query-style: uploadKey=<value>, hash=<value> (hash is not a secret but
    // is long; leave hash and only scrub credential-like params).
    for key in [
        "uploadKey",
        "upload_key",
        "apiKey",
        "api_key",
        "fileKey",
        "file_key",
    ] {
        // key=... until & or end or whitespace
        let needle = format!("{key}=");
        let mut search_from = 0;
        while let Some(rel) = out[search_from..].find(&needle) {
            let start = search_from + rel + needle.len();
            let rest = &out[start..];
            let end_rel = rest
                .find(|c: char| c == '&' || c.is_whitespace() || c == '"' || c == '\'')
                .unwrap_or(rest.len());
            let end = start + end_rel;
            out.replace_range(start..end, "<redacted>");
            search_from = start + "<redacted>".len();
        }
    }
    // Bearer tokens
    if let Some(idx) = out.to_ascii_lowercase().find("bearer ") {
        let start = idx + "bearer ".len();
        let rest = &out[start..];
        let end_rel = rest
            .find(|c: char| c.is_whitespace() || c == '"' || c == '\'')
            .unwrap_or(rest.len());
        let end = start + end_rel;
        if end > start {
            out.replace_range(start..end, "<redacted>");
        }
    }
    out
}

/// Sanitize a reqwest transport error so Filen `uploadKey` / API material never
/// reaches logs, tracker text, or user-facing errors.
fn sanitize_filen_transport_error(err: &reqwest::Error) -> String {
    let mut msg = err.to_string();
    if let Some(url) = err.url() {
        let redacted_url = redact_filen_secrets_in_text(url.as_str());
        msg = msg.replace(url.as_str(), &redacted_url);
    }
    redact_filen_secrets_in_text(&msg)
}

/// Encrypt a single plaintext chunk and POST it to `/v3/upload?index={index}`.
///
/// Mirrors the byte-exact wire format used by `filen-sdk-rs::api::v3::upload::
/// upload_file_chunk` (commit on `main` 2026-05): each chunk is encrypted
/// independently as `nonce(12) || ciphertext || tag(16)` and posted as the raw
/// request body, with the URL carrying `&hash=<sha512_hex_lowercase>` over the
/// encrypted bytes. No `Checksum` header is sent: the official Rust SDK omits
/// it and the Filen ingest accepts uploads without it. Lifting the header
/// removed a per-chunk JSON-string allocation and one SHA-512 pass.
#[allow(clippy::too_many_arguments)]
async fn upload_filen_chunk(
    client: &reqwest::Client,
    api_key: &SecretString,
    ingest_base: &str,
    file_uuid: &str,
    parent_uuid: &str,
    upload_key: &str,
    file_key: &str,
    index: u64,
    plaintext: Vec<u8>,
) -> Result<(), ProviderError> {
    let encrypted = FilenProvider::encrypt_file_content(&plaintext, file_key)?;
    drop(plaintext);

    let mut hasher = Sha512::new();
    hasher.update(&encrypted);
    let chunk_hash = hex::encode(hasher.finalize());

    let url = format!(
        "{}/v3/upload?uuid={}&index={}&parent={}&uploadKey={}&hash={}",
        ingest_base.trim_end_matches('/'),
        file_uuid,
        index,
        parent_uuid,
        upload_key,
        chunk_hash,
    );

    let resp = client
        .post(&url)
        .header(
            "Authorization",
            HeaderValue::from_str(&format!("Bearer {}", api_key.expose_secret()))
                .map_err(|e| ProviderError::Other(format!("Invalid auth header: {}", e)))?,
        )
        .body(encrypted)
        .send()
        .await
        .map_err(|e| ProviderError::NetworkError(sanitize_filen_transport_error(&e)))?;

    let status = resp.status();
    let retry_header = resp
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let resp_text = resp
        .text()
        .await
        .map_err(|e| ProviderError::ParseError(e.to_string()))?;

    if !status.is_success() {
        // Body preview is application JSON; still scrub in case a gateway echoes
        // the upload key.
        let preview = redact_filen_secrets_in_text(&resp_text[..resp_text.len().min(200)]);
        return Err(ProviderError::TransferFailed(format_filen_error(
            &format!("Upload chunk {} failed", index),
            status,
            &preview,
            retry_header.as_deref(),
        )));
    }

    let upload_resp: serde_json::Value =
        serde_json::from_str(&resp_text).map_err(|e| ProviderError::ParseError(e.to_string()))?;
    if upload_resp["status"].as_bool() != Some(true) {
        let msg = upload_resp["message"].as_str().unwrap_or("unknown");
        return Err(ProviderError::TransferFailed(format!(
            "Upload chunk {} rejected: {}",
            index,
            redact_filen_secrets_in_text(msg),
        )));
    }

    Ok(())
}

/// Fetch a single encrypted chunk from Filen's egest CDN, retrying transient
/// failures up to `max_retries` times.
///
/// Two failure classes are treated as transient and trigger a retry:
///
/// 1. HTTP status codes 5xx, 408, and 429 (server errors and rate limits).
/// 2. Body-decode errors raised while consuming `bytes_stream()`. These come
///    from reqwest when the underlying TCP connection is closed mid-response
///    or when the chunked-encoding framing is broken. On long sequential
///    download sessions (hundreds of consecutive GETs against
///    `egest.filen.io`) we observed these reliably around the 200th chunk.
///
/// Each retry uses exponential backoff: 250 ms, 500 ms, 1 s, 2 s, capped at
/// 4 s. 4xx errors other than 408/429 are not retried (the caller will get a
/// 404 or 403 immediately rather than waiting four full backoff cycles for an
/// authoritative non-recoverable response).
async fn download_filen_chunk(
    client: &reqwest::Client,
    url: &str,
    chunk_idx: u32,
    max_retries: u32,
) -> Result<Vec<u8>, String> {
    let mut last_err: Option<String> = None;
    for attempt in 0..=max_retries {
        if attempt > 0 {
            // Exponential backoff: 250 ms * 2^(attempt-1), capped at 4 s.
            let delay_ms = (250u64 << (attempt - 1).min(4)).min(4_000);
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            filen_log(&format!(
                "download chunk {} retry {}/{} after {} ms",
                chunk_idx, attempt, max_retries, delay_ms
            ));
        }

        let request = match client.get(url).build() {
            Ok(r) => r,
            Err(e) => {
                last_err = Some(format!("request build: {}", e));
                continue;
            }
        };

        let resp = match client.execute(request).await {
            Ok(r) => r,
            Err(e) => {
                last_err = Some(format!("send: {}", e));
                continue;
            }
        };

        let status = resp.status();
        if !status.is_success() {
            let body_preview = resp
                .text()
                .await
                .unwrap_or_default()
                .chars()
                .take(160)
                .collect::<String>();
            // Retry only on classes that may recover. 4xx (except 408/429)
            // are authoritative, so we surface them immediately.
            let retryable = filen_status_is_retryable(status.as_u16());
            let err = format!("status {}: {}", status, body_preview);
            if !retryable {
                return Err(err);
            }
            last_err = Some(err);
            continue;
        }

        let mut encrypted = Vec::new();
        let mut stream = resp.bytes_stream();
        let mut body_failed = false;
        while let Some(part) = stream.next().await {
            match part {
                Ok(bytes) => encrypted.extend_from_slice(&bytes),
                Err(e) => {
                    last_err = Some(format!(
                        "body decode after {} bytes: {}",
                        encrypted.len(),
                        e
                    ));
                    body_failed = true;
                    break;
                }
            }
        }
        if body_failed {
            continue;
        }
        return Ok(encrypted);
    }
    Err(last_err.unwrap_or_else(|| "exhausted retries with no error".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify the chunk-count math used by `upload()`. Mirrors the boundary
    /// cases that the Filen ingest is sensitive to: a one-byte file must be
    /// one chunk, an exactly-CHUNK_SIZE file must be one chunk, and one byte
    /// past the boundary must spill into a second chunk.
    #[test]
    fn total_chunks_math_matches_filen_sdk() {
        let cs = FILEN_CHUNK_SIZE as u64;
        assert_eq!(1u64.div_ceil(cs), 1);
        assert_eq!(cs.div_ceil(cs), 1);
        assert_eq!((cs + 1).div_ceil(cs), 2);
        assert_eq!((cs * 10).div_ceil(cs), 10);
        assert_eq!((cs * 10 + 1).div_ceil(cs), 11);
    }

    /// Confirm that the per-chunk wire format matches `nonce(12) ||
    /// ciphertext || tag(16)` (28 bytes overhead) and that
    /// `encrypt_file_content` paired with `decrypt_file_content` yields a
    /// byte-exact round-trip for a non-empty plaintext. This is the contract
    /// the chunked upload pipeline relies on.
    #[test]
    fn encrypt_decrypt_roundtrip_per_chunk_format() {
        let file_key: String = (0..32).map(|i| format!("{:02x}", i as u8)).collect();

        let plaintext = (0..4096u32)
            .flat_map(|i| i.to_le_bytes())
            .collect::<Vec<u8>>();

        let encrypted =
            FilenProvider::encrypt_file_content(&plaintext, &file_key).expect("encrypt");
        assert_eq!(
            encrypted.len(),
            plaintext.len() + 12 + 16,
            "encrypted len must be plaintext + nonce(12) + tag(16)"
        );
        let decrypted =
            FilenProvider::decrypt_file_content(&encrypted, &file_key).expect("decrypt");
        assert_eq!(decrypted, plaintext, "round-trip must be byte-exact");
    }

    /// Regression for #128: a Filen v2 file key is a 32-char string used
    /// directly as the 32-byte AES-256 key (its UTF-8 bytes), not hex. The old
    /// hex-only `derive_file_key` rejected these with "Invalid file key hex",
    /// which broke previewing/downloading files uploaded by other Filen clients
    /// (the official app, rclone, RSAF). Both key formats must derive to 32
    /// bytes, and a v2 key must survive a full encrypt -> decrypt round-trip.
    #[test]
    fn derive_file_key_accepts_v2_and_v3_formats() {
        // v3: 64 hex chars -> 32 raw bytes (also AeroFTP's own upload format).
        let v3: String = (0..32).map(|i| format!("{:02x}", i as u8)).collect();
        assert_eq!(v3.len(), 64);
        assert_eq!(
            FilenProvider::derive_file_key(&v3).expect("v3 hex").len(),
            32
        );

        // v2: 32-char alphanumeric key used as raw UTF-8 (the case #128 hit).
        let v2 = "aB3xZ9kLmN0pQ7rS4tU1vW6yC2dE5fG8"; // 32 chars, includes non-hex letters
        assert_eq!(v2.len(), 32);
        let derived = FilenProvider::derive_file_key(v2).expect("v2 raw must not be hex-decoded");
        assert_eq!(derived, v2.as_bytes(), "a v2 key is its own 32 raw bytes");

        // A v2 key must round-trip through the real encrypt/decrypt path.
        let plaintext = b"\x00\x01\x02 the quick brown fox \xff\xfe".to_vec();
        let enc = FilenProvider::encrypt_file_content(&plaintext, v2).expect("encrypt v2");
        let dec = FilenProvider::decrypt_file_content(&enc, v2).expect("decrypt v2");
        assert_eq!(dec, plaintext, "v2 round-trip must be byte-exact");

        // An unsupported length is a clear error, not a panic or a wrong key.
        assert!(FilenProvider::derive_file_key("tooshort").is_err());
    }

    /// Two chunks encrypted with the same file_key must produce different
    /// ciphertexts (proving each call uses a fresh random nonce). This is
    /// what makes parallel chunk uploads safe under AES-GCM.
    #[test]
    fn each_chunk_uses_a_fresh_nonce() {
        let file_key: String = (0..32).map(|i| format!("{:02x}", i as u8)).collect();
        let plaintext = vec![0xAB_u8; 1024];
        let a = FilenProvider::encrypt_file_content(&plaintext, &file_key).expect("encrypt a");
        let b = FilenProvider::encrypt_file_content(&plaintext, &file_key).expect("encrypt b");
        assert_ne!(
            a, b,
            "two encryptions of the same plaintext must differ (random nonce)"
        );
        assert_ne!(&a[..12], &b[..12], "nonces must differ");
    }

    // ---- S3-T02 multipart trait wiring ----

    #[test]
    fn filen_multipart_meta_roundtrip_preserves_fields() {
        let meta = FilenMultipartMeta {
            file_uuid: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            parent_uuid: "11111111-2222-3333-4444-555555555555".to_string(),
            file_key: (0..32).map(|i| format!("{:02x}", i as u8)).collect(),
            upload_key: (0..32).map(|i| format!("{:02x}", 255 - i as u8)).collect(),
            file_name: "weird name (1).bin".to_string(),
            mime: "application/octet-stream".to_string(),
            total: 16 * 1024 * 1024,
            part: 1024 * 1024,
            total_chunks: 16,
            last_modified_ms: None,
        };
        let encoded = meta.encode();
        let decoded = FilenMultipartMeta::decode(&encoded).expect("decode roundtrip");
        assert_eq!(meta, decoded);
    }

    #[test]
    fn filen_multipart_meta_decode_rejects_garbage() {
        let err = FilenMultipartMeta::decode("not-json").unwrap_err();
        assert!(matches!(err, ProviderError::Other(_)));
    }

    #[test]
    fn filen_runner_part_size_clamps_and_never_returns_zero() {
        assert_eq!(filen_runner_part_size(1024), 1024);
        assert_eq!(
            filen_runner_part_size(FILEN_CHUNK_SIZE as u64),
            FILEN_CHUNK_SIZE as u64
        );
        assert_eq!(
            filen_runner_part_size(50 * 1024 * 1024 * 1024),
            FILEN_CHUNK_SIZE as u64
        );
        assert_eq!(filen_runner_part_size(0), 1);
    }

    #[test]
    fn filen_total_chunks_matches_runner_formula() {
        let p = FILEN_CHUNK_SIZE as u64;
        // div_ceil semantics: exact multiple ⇒ same number; +1 byte ⇒ +1 chunk
        assert_eq!(p.div_ceil(p), 1);
        assert_eq!((4 * p).div_ceil(p), 4);
        assert_eq!((p + 1).div_ceil(p), 2);
        assert_eq!((1u64).div_ceil(p), 1);
    }

    // ---------------------------------------------------------------------
    // KE-E3: Filen Retry-After detection (Sprint K1)
    // ---------------------------------------------------------------------

    #[test]
    fn filen_is_rate_limited_recognises_429_and_503() {
        assert!(filen_is_rate_limited(429));
        assert!(filen_is_rate_limited(503));
    }

    #[test]
    fn filen_is_rate_limited_rejects_other_statuses() {
        assert!(!filen_is_rate_limited(200));
        assert!(!filen_is_rate_limited(400));
        assert!(!filen_is_rate_limited(401));
        assert!(!filen_is_rate_limited(404));
        assert!(!filen_is_rate_limited(500));
        assert!(!filen_is_rate_limited(502));
        assert!(!filen_is_rate_limited(504));
    }

    #[test]
    fn filen_retry_marker_tail_emits_marker_on_429_with_header() {
        let tail = filen_retry_marker_tail(429, Some("15")).expect("rate-limited + hint");
        assert!(tail.contains("retry-after-secs=15"));
    }

    #[test]
    fn filen_retry_marker_tail_emits_marker_on_503_with_header() {
        let tail = filen_retry_marker_tail(503, Some("40")).expect("503 + hint");
        assert!(tail.contains("retry-after-secs=40"));
    }

    #[test]
    fn filen_retry_marker_tail_returns_none_without_header() {
        assert_eq!(filen_retry_marker_tail(429, None), None);
        assert_eq!(filen_retry_marker_tail(429, Some("")), None);
        assert_eq!(filen_retry_marker_tail(503, Some("not-numeric")), None);
    }

    #[test]
    fn filen_retry_marker_tail_returns_none_for_non_throttle_status() {
        assert_eq!(filen_retry_marker_tail(500, Some("10")), None);
        assert_eq!(filen_retry_marker_tail(404, Some("10")), None);
        assert_eq!(filen_retry_marker_tail(200, Some("10")), None);
    }

    #[test]
    fn format_filen_error_appends_marker_on_throttle() {
        let msg = format_filen_error(
            "Upload chunk 7 failed",
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            "Too many requests",
            Some("25"),
        );
        assert!(msg.contains("Upload chunk 7 failed"));
        assert!(msg.contains("429"));
        assert!(msg.contains("retry-after-secs=25"));
    }

    #[test]
    fn format_filen_error_omits_marker_on_non_throttle() {
        let msg = format_filen_error(
            "Upload chunk 7 failed",
            reqwest::StatusCode::BAD_REQUEST,
            "invalid uuid",
            Some("25"),
        );
        assert!(msg.contains("400"));
        assert!(!msg.contains("retry-after-secs"));
    }

    // Row 4 (#347): the base composition of format_filen_error, prefix then the
    // StatusCode Display form then the body preview, with no marker when there is
    // no Retry-After header.
    #[test]
    fn format_filen_error_composes_prefix_status_and_body() {
        let msg = format_filen_error(
            "List dir failed",
            reqwest::StatusCode::NOT_FOUND,
            "no such folder",
            None,
        );
        assert_eq!(msg, "List dir failed: 404 Not Found - no such folder");
        assert!(!msg.contains("retry-after-secs"));
    }

    // Row 4 (#347): the transient-class retry boundary for Filen egest/ingest.
    // 5xx and the two recoverable 4xx (408, 429) retry; every other 4xx is
    // authoritative and surfaced immediately.
    #[test]
    fn filen_status_is_retryable_covers_transient_classes_only() {
        // Retryable: the whole 5xx range plus 408 and 429.
        for code in [500u16, 502, 503, 504, 599, 408, 429] {
            assert!(
                filen_status_is_retryable(code),
                "HTTP {code} must be retryable"
            );
        }
        // Authoritative: other 4xx and any 2xx/3xx.
        for code in [400u16, 401, 403, 404, 409, 410, 200, 301] {
            assert!(
                !filen_status_is_retryable(code),
                "HTTP {code} must NOT be retried"
            );
        }
    }

    // ---- DAG-P1-05D: HttpClonePool worker promotion ----

    /// Parse a query parameter from a request URI (avoids axum `query` feature).
    fn uri_query_param(uri: &str, key: &str) -> Option<String> {
        let q = uri.split_once('?')?.1;
        for pair in q.split('&') {
            let (k, v) = pair.split_once('=')?;
            if k == key {
                return Some(v.to_string());
            }
        }
        None
    }

    fn demo_cfg() -> FilenConfig {
        FilenConfig {
            email: "demo@example.com".to_string(),
            password: SecretString::from("demo-password-not-real".to_string()),
            two_factor_code: None,
            totp_secret: None,
            api_key: None,
        }
    }

    #[test]
    fn clone_for_transfer_requires_connection() {
        let p = FilenProvider::new(demo_cfg());
        assert!(!p.is_connected());
        assert!(matches!(
            p.clone_for_transfer(),
            Err(ProviderError::NotConnected)
        ));
        // Disconnected error must not leak credentials.
        let err = match p.clone_for_transfer() {
            Err(e) => e,
            Ok(_) => panic!("expected NotConnected"),
        };
        let s = err.to_string();
        assert!(!s.contains("demo-password"));
        assert!(!s.contains("demo@example.com") || s == "Not connected to server");
    }

    #[test]
    fn connected_clone_succeeds_without_reconnect_and_is_distinct() {
        let p = FilenProvider::connected_for_test(demo_cfg());
        let worker = p.clone_for_transfer().expect("connected clone");
        assert_eq!(worker.provider_type(), ProviderType::Filen);
        assert!(worker.is_connected());
        assert_eq!(
            p.transfer_executor_kind(),
            ProviderTransferExecutorKind::HttpClonePool
        );
        assert_eq!(p.transfer_executor_max_sessions(), 4);
        let primary_ptr = &p as *const _ as usize;
        let worker_ptr = worker.as_ref() as *const dyn StorageProvider as *const () as usize;
        assert_ne!(primary_ptr, worker_ptr);
    }

    #[test]
    fn clone_shares_auth_arc_and_keeps_caches_bounded() {
        let mut p = FilenProvider::connected_for_test(demo_cfg());
        // Pollute primary caches beyond navigation seed.
        for i in 0..20 {
            p.dir_cache_insert(
                format!("/folder-{i}"),
                DirInfo {
                    uuid: format!("uuid-{i}"),
                    name: format!("folder-{i}"),
                },
            );
            p.file_key_cache_insert(format!("file-{i}"), format!("key-{i}"));
        }
        assert!(p.dir_cache.len() > 2);
        assert_eq!(p.file_key_cache.len(), 20);

        let w = p.clone_transfer_worker();
        assert!(
            Arc::ptr_eq(&p.auth, &w.auth),
            "auth snapshot must be shared immutably via Arc"
        );
        assert!(
            Arc::ptr_eq(&p.config, &w.config),
            "config snapshot must be shared immutably via Arc"
        );
        // Seeded navigation only: root (+ optional current). Never full caches.
        assert!(
            w.dir_cache.len() <= 2,
            "clone dir_cache must stay bounded (got {})",
            w.dir_cache.len()
        );
        assert!(w.dir_cache.contains_key("/"));
        assert!(
            w.file_key_cache.is_empty(),
            "clone must not copy file_key_cache"
        );
        // clone_for_transfer also succeeds and reports Filen.
        let worker = p.clone_for_transfer().expect("clone");
        assert_eq!(worker.provider_type(), ProviderType::Filen);
    }

    #[test]
    fn runtime_composition_yields_http_clone_pool_when_connected() {
        use crate::provider_transfer_executor::{
            compose_runtime_transfer_capabilities, resolve_session_model,
        };
        use crate::transfer_dag::Capability;

        let p = FilenProvider::connected_for_test(demo_cfg());
        let advertised = p.transfer_capabilities();
        let can_clone = p.clone_for_transfer().is_ok();
        assert!(can_clone);
        let caps = compose_runtime_transfer_capabilities(
            &advertised,
            p.transfer_executor_kind(),
            can_clone,
        );
        assert_eq!(caps.file_parallel, Capability::Supported);
        assert_eq!(caps.session_pool, Capability::Supported);
        assert_eq!(caps.max_file_slots, Some(4));
        assert_eq!(caps.max_chunk_slots, Some(4));

        let model = resolve_session_model(
            ProviderType::Filen,
            &caps,
            p.transfer_executor_kind(),
            can_clone,
            p.transfer_executor_max_sessions(),
            8,
        );
        assert!(matches!(
            model,
            crate::provider_transfer_executor::ProviderExecutorSessionModel::HttpClonePool { .. }
        ));
        assert_eq!(model.max_leases(), 4);
    }

    #[test]
    fn forced_clone_failure_demotes_runtime_file_parallelism() {
        use crate::provider_transfer_executor::compose_runtime_transfer_capabilities;
        use crate::transfer_dag::Capability;

        let p = FilenProvider::new(demo_cfg());
        assert_eq!(
            p.transfer_executor_kind(),
            ProviderTransferExecutorKind::HttpClonePool
        );
        let can_clone = p.clone_for_transfer().is_ok();
        assert!(!can_clone);
        let caps = compose_runtime_transfer_capabilities(
            &p.transfer_capabilities(),
            p.transfer_executor_kind(),
            can_clone,
        );
        assert_eq!(caps.file_parallel, Capability::Unsupported);
        assert_eq!(caps.session_pool, Capability::Unsupported);
        assert_eq!(caps.max_file_slots, Some(1));
        // Multipart part cap from hints remains 4; primary mutex serialises parts.
        assert_eq!(caps.max_chunk_slots, Some(4));
    }

    #[test]
    fn legacy_fanout_ceiling_matches_shaped_session_cap() {
        // Composition proof: legacy upload() fan-out is FILEN_PARALLEL_CHUNK_UPLOADS
        // and must not exceed the shaped session/file ceiling. Sub-threshold
        // files (total < 1 MiB) produce a single Filen chunk; shaped files at
        // or above 1 MiB use DAG part nodes under the same cap of 4.
        assert_eq!(FILEN_PARALLEL_CHUNK_UPLOADS, 4);
        assert_eq!(FILEN_TRANSFER_MAX_SESSIONS, 4);
        assert_eq!(
            FILEN_PARALLEL_CHUNK_UPLOADS as u16,
            FILEN_TRANSFER_MAX_SESSIONS
        );
        let p = FilenProvider::connected_for_test(demo_cfg());
        let hints = p.transfer_optimization_hints();
        assert_eq!(hints.multipart_threshold, FILEN_CHUNK_SIZE as u64);
        assert_eq!(hints.multipart_max_parallel, 4);
        // One sub-threshold file → one chunk (no 4×4 multiplication on shaped path).
        assert_eq!(1u64.div_ceil(FILEN_CHUNK_SIZE as u64), 1);
        assert_eq!(
            (FILEN_CHUNK_SIZE as u64).div_ceil(FILEN_CHUNK_SIZE as u64),
            1
        );
        assert_eq!(
            (4 * FILEN_CHUNK_SIZE as u64).div_ceil(FILEN_CHUNK_SIZE as u64),
            4
        );
    }

    #[test]
    fn multipart_handle_debug_redacts_upload_id() {
        let meta = FilenMultipartMeta {
            file_uuid: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            parent_uuid: "11111111-2222-3333-4444-555555555555".to_string(),
            file_key: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .to_string(),
            upload_key: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                .to_string(),
            file_name: "secret.bin".to_string(),
            mime: "application/octet-stream".to_string(),
            total: 4096,
            part: 1024,
            total_chunks: 4,
            last_modified_ms: None,
        };
        let handle = MultipartHandle {
            upload_id: meta.encode(),
            remote_path: "/secret.bin".into(),
        };
        let dbg = format!("{handle:?}");
        assert!(dbg.contains("<redacted>"), "dbg={dbg}");
        assert!(!dbg.contains(&meta.file_key), "file_key must not appear");
        assert!(
            !dbg.contains(&meta.upload_key),
            "upload_key must not appear"
        );
        assert!(!dbg.contains("uploadKey"), "raw meta must not appear");
        assert!(dbg.contains("/secret.bin"));
    }

    #[test]
    fn redact_filen_secrets_strips_upload_key_and_bearer() {
        let raw = "error sending request for url (https://ingest.filen.io/v3/upload?uuid=u&index=0&parent=p&uploadKey=deadbeefcafebabe&hash=abc) Bearer sk-live-secret-token";
        let clean = redact_filen_secrets_in_text(raw);
        assert!(!clean.contains("deadbeefcafebabe"), "clean={clean}");
        assert!(!clean.contains("sk-live-secret-token"), "clean={clean}");
        assert!(clean.contains("uploadKey=<redacted>"), "clean={clean}");
        assert!(
            clean.contains("Bearer <redacted>")
                || clean.to_ascii_lowercase().contains("bearer <redacted>")
        );
    }

    #[test]
    fn throttle_429_and_503_map_to_typed_congestion_with_retry_after() {
        let msg_429 = format_filen_error(
            "Upload chunk 0 failed",
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            "Too many requests",
            Some("17"),
        );
        let pe_429 = ProviderError::TransferFailed(msg_429);
        let te_429 = crate::transfer_dag::TransferError::from_provider(&pe_429);
        assert_eq!(
            te_429.kind,
            crate::transfer_dag::TransferErrorKind::RateLimited
        );
        assert_eq!(te_429.retry_after, Some(std::time::Duration::from_secs(17)));

        let msg_503 = format_filen_error(
            "Upload chunk 1 failed",
            reqwest::StatusCode::SERVICE_UNAVAILABLE,
            "Service Unavailable",
            Some("42"),
        );
        let pe_503 = ProviderError::TransferFailed(msg_503);
        let te_503 = crate::transfer_dag::TransferError::from_provider(&pe_503);
        assert_eq!(
            te_503.kind,
            crate::transfer_dag::TransferErrorKind::ServiceUnavailable
        );
        assert_eq!(te_503.retry_after, Some(std::time::Duration::from_secs(42)));
    }

    #[test]
    fn clone_multipart_worker_helper_returns_some_only_when_connected() {
        use crate::transfer_multipart::clone_multipart_worker;

        let disconnected = FilenProvider::new(demo_cfg());
        assert!(clone_multipart_worker(&disconnected).is_none());

        let connected = FilenProvider::connected_for_test(demo_cfg());
        assert!(clone_multipart_worker(&connected).is_some());
    }

    #[tokio::test]
    async fn barrier_backed_ingest_records_exact_peak_4() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        use axum::{body::Bytes, extract::Request, http::StatusCode, routing::post, Router};

        let in_flight = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let request_count = Arc::new(AtomicUsize::new(0));
        let indexes = Arc::new(std::sync::Mutex::new(Vec::<u64>::new()));
        let nonces = Arc::new(std::sync::Mutex::new(Vec::<Vec<u8>>::new()));
        let body_lens = Arc::new(std::sync::Mutex::new(Vec::<usize>::new()));
        let seen_uuid = Arc::new(std::sync::Mutex::new(None::<String>));
        let seen_parent = Arc::new(std::sync::Mutex::new(None::<String>));
        let seen_upload_key = Arc::new(std::sync::Mutex::new(None::<String>));
        let gate = Arc::new(tokio::sync::Barrier::new(4));

        let in_flight_h = Arc::clone(&in_flight);
        let peak_h = Arc::clone(&peak);
        let count_h = Arc::clone(&request_count);
        let indexes_h = Arc::clone(&indexes);
        let nonces_h = Arc::clone(&nonces);
        let body_lens_h = Arc::clone(&body_lens);
        let uuid_h = Arc::clone(&seen_uuid);
        let parent_h = Arc::clone(&seen_parent);
        let uk_h = Arc::clone(&seen_upload_key);
        let gate_h = Arc::clone(&gate);

        let app = Router::new().route(
            "/v3/upload",
            post(move |req: Request| {
                let in_flight = Arc::clone(&in_flight_h);
                let peak = Arc::clone(&peak_h);
                let count = Arc::clone(&count_h);
                let indexes = Arc::clone(&indexes_h);
                let nonces = Arc::clone(&nonces_h);
                let body_lens = Arc::clone(&body_lens_h);
                let uuid = Arc::clone(&uuid_h);
                let parent = Arc::clone(&parent_h);
                let uk = Arc::clone(&uk_h);
                let gate = Arc::clone(&gate_h);
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    let uri = req.uri().to_string();
                    let q_uuid = uri_query_param(&uri, "uuid").expect("uuid");
                    let q_index: u64 = uri_query_param(&uri, "index")
                        .expect("index")
                        .parse()
                        .expect("index u64");
                    let q_parent = uri_query_param(&uri, "parent").expect("parent");
                    let q_upload_key = uri_query_param(&uri, "uploadKey").expect("uploadKey");
                    let q_hash = uri_query_param(&uri, "hash").expect("hash");
                    {
                        let mut g = uuid.lock().unwrap();
                        if g.is_none() {
                            *g = Some(q_uuid.clone());
                        } else {
                            assert_eq!(g.as_ref().unwrap(), &q_uuid);
                        }
                    }
                    {
                        let mut g = parent.lock().unwrap();
                        if g.is_none() {
                            *g = Some(q_parent.clone());
                        } else {
                            assert_eq!(g.as_ref().unwrap(), &q_parent);
                        }
                    }
                    {
                        let mut g = uk.lock().unwrap();
                        if g.is_none() {
                            *g = Some(q_upload_key.clone());
                        } else {
                            assert_eq!(g.as_ref().unwrap(), &q_upload_key);
                        }
                    }
                    indexes.lock().unwrap().push(q_index);
                    assert!(!q_hash.is_empty());

                    let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    gate.wait().await;
                    in_flight.fetch_sub(1, Ordering::SeqCst);

                    let body = axum::body::to_bytes(req.into_body(), 2 * 1024 * 1024)
                        .await
                        .unwrap_or_else(|_| Bytes::new());
                    body_lens.lock().unwrap().push(body.len());
                    if body.len() >= 12 {
                        nonces.lock().unwrap().push(body[..12].to_vec());
                    }
                    (
                        StatusCode::OK,
                        axum::Json(serde_json::json!({"status": true})),
                    )
                }
            }),
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fixture");
        let addr = listener.local_addr().expect("local addr");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });

        let mut primary = FilenProvider::connected_for_test(demo_cfg());
        primary.ingest_base_override = Some(format!("http://{addr}"));

        let file_uuid = "550e8400-e29b-41d4-a716-446655440000".to_string();
        let parent_uuid = "11111111-2222-3333-4444-555555555555".to_string();
        let file_key: String = (0..32).map(|i| format!("{:02x}", i as u8)).collect();
        let upload_key: String = (0..32).map(|i| format!("{:02x}", 255 - i as u8)).collect();
        let part_plain = 64usize;
        let meta = FilenMultipartMeta {
            file_uuid: file_uuid.clone(),
            parent_uuid: parent_uuid.clone(),
            file_key: file_key.clone(),
            upload_key: upload_key.clone(),
            file_name: "f.bin".to_string(),
            mime: "application/octet-stream".to_string(),
            total: (4 * part_plain) as u64,
            part: part_plain as u64,
            total_chunks: 4,
            last_modified_ms: None,
        };
        let handle = MultipartHandle {
            upload_id: meta.encode(),
            remote_path: "/f.bin".to_string(),
        };

        let mut workers: Vec<Box<dyn StorageProvider>> = (0..4)
            .map(|_| primary.clone_for_transfer().expect("worker clone"))
            .collect();
        let mut set = tokio::task::JoinSet::new();
        for (i, mut worker) in workers.drain(..).enumerate() {
            let handle = handle.clone();
            let part = (i as u32) + 1;
            set.spawn(async move {
                worker
                    .upload_part(&handle, part, vec![b'y'; part_plain])
                    .await
                    .map_err(|e| e.to_string())
            });
        }
        let mut receipts = Vec::new();
        while let Some(res) = set.join_next().await {
            receipts.push(res.expect("join").expect("upload_part"));
        }
        receipts.sort_by_key(|r| r.part_number);
        assert_eq!(receipts.len(), 4);
        for (i, r) in receipts.iter().enumerate() {
            assert_eq!(r.part_number, (i as u32) + 1);
        }

        let mut seen_idx = indexes.lock().unwrap().clone();
        seen_idx.sort();
        assert_eq!(
            seen_idx,
            vec![0, 1, 2, 3],
            "Filen index is 0-based part_number-1"
        );

        assert_eq!(
            seen_uuid.lock().unwrap().as_deref(),
            Some(file_uuid.as_str())
        );
        assert_eq!(
            seen_parent.lock().unwrap().as_deref(),
            Some(parent_uuid.as_str())
        );
        assert_eq!(
            seen_upload_key.lock().unwrap().as_deref(),
            Some(upload_key.as_str())
        );

        let lens = body_lens.lock().unwrap().clone();
        assert_eq!(lens.len(), 4);
        for len in &lens {
            assert_eq!(
                *len,
                part_plain + 12 + 16,
                "encrypted body = plaintext + nonce(12) + tag(16)"
            );
        }
        let ns = nonces.lock().unwrap().clone();
        assert_eq!(ns.len(), 4);
        let mut unique = ns.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(
            unique.len(),
            4,
            "each part must use a distinct random nonce"
        );

        let observed_peak = peak.load(Ordering::SeqCst);
        assert_eq!(request_count.load(Ordering::SeqCst), 4);
        assert_eq!(
            observed_peak, 4,
            "barrier-backed fixture must record exact peak=4 (got {observed_peak})"
        );

        server.abort();
    }

    #[tokio::test]
    async fn out_of_order_parts_map_to_requested_numbers_and_complete_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        use axum::{extract::Request, http::StatusCode, routing::post, Router};

        let done_count = Arc::new(AtomicUsize::new(0));
        let done_h = Arc::clone(&done_count);

        let app = Router::new()
            .route(
                "/v3/upload",
                post(|req: Request| async move {
                    let uri = req.uri().to_string();
                    let index: u64 = uri_query_param(&uri, "index")
                        .expect("index")
                        .parse()
                        .unwrap_or(0);
                    // Higher indexes complete first so completion order differs.
                    let delay_ms = 20u64 * (4u64.saturating_sub(index));
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    let _ = axum::body::to_bytes(req.into_body(), 64 * 1024).await;
                    (
                        StatusCode::OK,
                        axum::Json(serde_json::json!({"status": true})),
                    )
                }),
            )
            .route(
                "/v3/upload/done",
                post(move |_req: Request| {
                    let done = Arc::clone(&done_h);
                    async move {
                        done.fetch_add(1, Ordering::SeqCst);
                        (
                            StatusCode::OK,
                            axum::Json(serde_json::json!({"status": true})),
                        )
                    }
                }),
            );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });

        let mut primary = FilenProvider::connected_for_test(demo_cfg());
        primary.ingest_base_override = Some(format!("http://{addr}"));
        primary.gateway_base_override = Some(format!("http://{addr}"));

        // Use a known master key that can encrypt metadata (AES path).
        // encrypt_metadata uses master_keys from auth snapshot set in connected_for_test.
        let meta = FilenMultipartMeta {
            file_uuid: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            parent_uuid: "11111111-2222-3333-4444-555555555555".to_string(),
            file_key: (0..32).map(|i| format!("{:02x}", i as u8)).collect(),
            upload_key: (0..32).map(|i| format!("{:02x}", 200 - i as u8)).collect(),
            file_name: "ooo.bin".to_string(),
            mime: "application/octet-stream".to_string(),
            total: 4096,
            part: 1024,
            total_chunks: 4,
            last_modified_ms: None,
        };
        let handle = MultipartHandle {
            upload_id: meta.encode(),
            remote_path: "/ooo.bin".into(),
        };

        let mut set = tokio::task::JoinSet::new();
        for part in [4u32, 1, 3, 2] {
            let mut worker = primary.clone_for_transfer().expect("clone");
            let handle = handle.clone();
            set.spawn(async move {
                worker
                    .upload_part(&handle, part, vec![0u8; 32])
                    .await
                    .map(|r| (part, r))
                    .map_err(|e| e.to_string())
            });
        }
        let mut got = Vec::new();
        let mut receipts = Vec::new();
        while let Some(res) = set.join_next().await {
            let (requested, receipt) = res.unwrap().unwrap();
            assert_eq!(receipt.part_number, requested);
            got.push(receipt.part_number);
            receipts.push(receipt);
        }
        got.sort();
        assert_eq!(got, vec![1, 2, 3, 4]);

        primary
            .complete_multipart_upload(handle.clone(), receipts)
            .await
            .expect("complete");
        assert_eq!(done_count.load(Ordering::SeqCst), 1);

        // Abort is a documented no-op and may be called again without extra wire.
        primary
            .abort_multipart_upload(handle)
            .await
            .expect("abort no-op");
        assert_eq!(done_count.load(Ordering::SeqCst), 1);

        server.abort();
    }

    #[tokio::test]
    async fn one_part_failure_leaves_sibling_and_primary_usable() {
        use axum::{extract::Request, http::StatusCode, routing::post, Router};

        let app = Router::new().route(
            "/v3/upload",
            post(|req: Request| async move {
                let uri = req.uri().to_string();
                let index: u64 = uri_query_param(&uri, "index")
                    .expect("index")
                    .parse()
                    .unwrap_or(0);
                let _ = axum::body::to_bytes(req.into_body(), 64 * 1024).await;
                if index == 1 {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        axum::Json(serde_json::json!({"status": false, "message": "boom"})),
                    )
                } else {
                    (
                        StatusCode::OK,
                        axum::Json(serde_json::json!({"status": true})),
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });

        let mut primary = FilenProvider::connected_for_test(demo_cfg());
        primary.ingest_base_override = Some(format!("http://{addr}"));
        let meta = FilenMultipartMeta {
            file_uuid: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            parent_uuid: "11111111-2222-3333-4444-555555555555".to_string(),
            file_key: (0..32).map(|i| format!("{:02x}", i as u8)).collect(),
            upload_key: (0..32).map(|i| format!("{:02x}", 100 - i as u8)).collect(),
            file_name: "fail.bin".to_string(),
            mime: "application/octet-stream".to_string(),
            total: 2048,
            part: 1024,
            total_chunks: 2,
            last_modified_ms: None,
        };
        let handle = MultipartHandle {
            upload_id: meta.encode(),
            remote_path: "/fail.bin".into(),
        };
        let mut w1 = primary.clone_for_transfer().unwrap();
        let mut w2 = primary.clone_for_transfer().unwrap();
        let ok = w1.upload_part(&handle, 1, vec![1u8; 8]).await;
        let err = w2.upload_part(&handle, 2, vec![2u8; 8]).await;
        assert!(ok.is_ok());
        assert!(err.is_err());
        // Failure must not complete; incomplete parts list fails closed.
        let complete_err = primary
            .complete_multipart_upload(
                handle.clone(),
                vec![UploadedPart {
                    part_number: 1,
                    etag: String::new(),
                }],
            )
            .await;
        assert!(complete_err.is_err());
        // Abort remains a no-op and is safe to call once (executor at-most-once).
        primary.abort_multipart_upload(handle).await.expect("abort");
        assert!(primary.is_connected());
        server.abort();
    }

    #[tokio::test]
    async fn transport_error_does_not_leak_upload_key_or_api_key() {
        // Point ingest at a closed local port so reqwest fails with a URL that
        // would otherwise include uploadKey in the error string.
        let mut primary = FilenProvider::connected_for_test(demo_cfg());
        primary.ingest_base_override = Some("http://127.0.0.1:1".to_string());
        let api_key = primary.auth.api_key.expose_secret().to_string();
        let upload_key = "cafebabedeadbeefcafebabedeadbeefcafebabedeadbeefcafebabedeadbeef";
        let file_key = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let meta = FilenMultipartMeta {
            file_uuid: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            parent_uuid: "11111111-2222-3333-4444-555555555555".to_string(),
            file_key: file_key.to_string(),
            upload_key: upload_key.to_string(),
            file_name: "x.bin".to_string(),
            mime: "application/octet-stream".to_string(),
            total: 64,
            part: 64,
            total_chunks: 1,
            last_modified_ms: None,
        };
        let handle = MultipartHandle {
            upload_id: meta.encode(),
            remote_path: "/x.bin".into(),
        };
        let err = primary
            .upload_part(&handle, 1, vec![9u8; 64])
            .await
            .expect_err("must fail against closed port");
        let s = err.to_string();
        assert!(
            !s.contains(upload_key),
            "upload_key must not appear in error: {s}"
        );
        assert!(
            !s.contains(&api_key),
            "api_key must not appear in error: {s}"
        );
        assert!(
            !s.contains(file_key),
            "file_key must not appear in error: {s}"
        );
        assert!(
            !s.contains("uploadKey=cafe") && !s.contains(&format!("uploadKey={upload_key}")),
            "raw uploadKey query must be redacted: {s}"
        );
    }

    // ── lastModified is the SOURCE mtime, never the moment of the write ──────
    //
    // Filen's `lastModified` is the only mtime the account holds. Stamping the
    // write instant re-dated every file to when it was transferred, so a folder
    // uploaded through AeroFTP compared as entirely out of sync against the
    // local folder it came from, on every subsequent compare, and the Filen
    // desktop bridge served the same wrong value over WebDAV (#347).

    #[test]
    fn upload_metadata_carries_the_source_mtime_not_the_upload_time() {
        let dir = std::env::temp_dir().join(format!("filen_mtime_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("report.txt");
        std::fs::write(&path, b"contents").unwrap();

        // Backdate the source well outside any comparison tolerance.
        let long_ago =
            std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
        filetime::set_file_mtime(&path, filetime::FileTime::from_system_time(long_ago)).unwrap();

        let meta = std::fs::metadata(&path).unwrap();
        let stamped = FilenProvider::source_last_modified_ms(Some(&meta));
        assert_eq!(
            stamped, 1_000_000_000_000,
            "the upload must record the file's own mtime"
        );

        let now = chrono::Utc::now().timestamp_millis();
        assert!(
            (now - stamped) > 60_000,
            "a source mtime that lands within a minute of now means the write is              stamping its own time again"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// No readable source mtime is the one case where the write has nothing
    /// better than its own clock.
    #[test]
    fn upload_metadata_falls_back_to_now_without_a_source() {
        let before = chrono::Utc::now().timestamp_millis();
        let stamped = FilenProvider::source_last_modified_ms(None);
        assert!(stamped >= before, "fallback must be the current time");
    }

    /// A rename changes the name, not the contents, so it carries the mtime the
    /// file already had rather than re-dating it.
    #[test]
    fn rename_preserves_the_existing_mtime() {
        assert_eq!(
            FilenProvider::entry_last_modified_ms(Some("2020-05-17T10:30:00Z")),
            Some(1_589_711_400_000),
        );
        // An entry that never had a timestamp, and a malformed one, both decline
        // rather than inventing a value; the caller then falls back to now.
        assert_eq!(FilenProvider::entry_last_modified_ms(None), None);
        assert_eq!(
            FilenProvider::entry_last_modified_ms(Some("not a date")),
            None
        );
    }

    /// The commit runs long after the local file was read, so the mtime travels
    /// inside the handle. A handle written before the field existed decodes with
    /// `None` instead of failing, and the commit then stamps its own time.
    #[test]
    fn multipart_handle_carries_the_mtime_and_tolerates_older_handles() {
        let meta = FilenMultipartMeta {
            file_uuid: "u".into(),
            parent_uuid: "p".into(),
            file_key: "k".into(),
            upload_key: "uk".into(),
            file_name: "big.bin".into(),
            mime: "application/octet-stream".into(),
            total: 1024,
            part: 512,
            total_chunks: 2,
            last_modified_ms: Some(1_589_711_400_000),
        };
        let decoded = FilenMultipartMeta::decode(&meta.encode()).unwrap();
        assert_eq!(decoded.last_modified_ms, Some(1_589_711_400_000));

        let legacy = r#"{"file_uuid":"u","parent_uuid":"p","file_key":"k","upload_key":"uk","file_name":"big.bin","mime":"application/octet-stream","total":1024,"part":512,"total_chunks":2}"#;
        let decoded_legacy = FilenMultipartMeta::decode(legacy)
            .expect("a handle from before this field must still decode");
        assert_eq!(decoded_legacy.last_modified_ms, None);
    }
}
