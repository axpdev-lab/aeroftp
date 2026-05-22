//! Filen.io Storage Provider
//!
//! Implements StorageProvider for Filen using their REST API.
//! Uses client-side AES-256-GCM encryption (zero-knowledge).
//! All file names, metadata, and content are encrypted locally.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

pub mod notes;

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
use tracing::debug;

/// Debug logging through tracing infrastructure (no file I/O)
fn filen_log(msg: &str) {
    debug!(target: "filen", "{}", msg);
}

use super::http_retry::{send_with_retry, HttpRetryConfig};
use super::types::FilenConfig;
use super::{
    ProviderError, ProviderType, RemoteEntry, ShareLinkCapabilities, ShareLinkOptions,
    ShareLinkResult, StorageInfo, StorageProvider,
};

/// Filen API gateway
const GATEWAY: &str = "https://gateway.filen.io";

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

/// Filen Storage Provider
pub struct FilenProvider {
    config: FilenConfig,
    client: reqwest::Client,
    connected: bool,
    /// F-SEC-01: API key wrapped in SecretString for memory zeroization on drop
    api_key: SecretString,
    /// F-SEC-02: Master encryption keys wrapped in SecretString for memory zeroization on drop
    master_keys: Vec<SecretString>,
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
            config,
            client,
            connected: false,
            api_key: SecretString::from(String::new()),
            master_keys: Vec::new(),
            current_path: "/".to_string(),
            current_folder_uuid: String::new(),
            root_uuid: String::new(),
            dir_cache: HashMap::new(),
            file_key_cache: HashMap::new(),
            retry_config: HttpRetryConfig::default(),
            user_uuid: String::new(),
            auth_version: None,
        }
    }

    pub fn auth_version(&self) -> Option<u32> {
        self.auth_version
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
        for key in &self.master_keys {
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
            .post(format!("{}/v3/user/masterKeys", GATEWAY))
            .header(
                "Authorization",
                HeaderValue::from_str(&format!("Bearer {}", self.api_key.expose_secret()))
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

    /// Derive AES key from file key: hex-decode 64-char hex string to 32 raw bytes
    /// (Filen SDK v3 format for file data encryption)
    fn derive_file_key(file_key: &str) -> Result<Vec<u8>, ProviderError> {
        hex::decode(file_key)
            .map_err(|e| ProviderError::Other(format!("Invalid file key hex: {}", e)))
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
                .post(format!("{}/v3/dir/content", GATEWAY))
                .header(
                    "Authorization",
                    HeaderValue::from_str(&format!("Bearer {}", self.api_key.expose_secret()))
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
            .post(format!("{}/v3/auth/info", GATEWAY))
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
        self.master_keys = vec![SecretString::from(derived_master_key.clone())];

        let configured_api_key = self
            .config
            .api_key
            .as_ref()
            .map(|k| k.expose_secret().trim().to_string())
            .filter(|k| !k.is_empty());

        let encrypted_master_keys: Option<String> = if let Some(api_key) = configured_api_key {
            self.api_key = SecretString::from(api_key);
            filen_log("connect: API-key path, skipping /v3/login (no 2FA window)");
            // Best-effort: recover the canonical master-keys history. If the
            // call fails, derived_master_key alone still decrypts everything
            // encrypted under the current password.
            match self.fetch_master_keys_blob(&derived_master_key).await {
                Ok(blob) => blob,
                Err(e) => {
                    filen_log(&format!("connect: /v3/user/masterKeys failed: {}", e));
                    None
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
                .post(format!("{}/v3/login", GATEWAY))
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

            self.api_key = SecretString::from(login_data.api_key);
            Some(login_data.master_keys)
        };

        // Step 4: decrypt the additional master keys, when a blob was obtained.
        filen_log(&format!(
            "derived_master_key len={}",
            derived_master_key.len()
        ));
        if let Some(blob) = encrypted_master_keys {
            filen_log(&format!("master_keys blob len={}", blob.len()));
            if let Some(decrypted) = Self::try_decrypt_aes_gcm(&blob, &derived_master_key) {
                let decrypted_keys: Vec<SecretString> = decrypted
                    .split('|')
                    .map(|s| SecretString::from(s.to_string()))
                    .collect();
                // Check if derived_master_key is already present
                let already_present = decrypted_keys
                    .iter()
                    .any(|k| k.expose_secret() == derived_master_key);
                self.master_keys = decrypted_keys;
                if !already_present {
                    self.master_keys
                        .push(SecretString::from(derived_master_key));
                }
            }
        }

        // Step 5: Get root folder UUID from user info
        let user_resp: serde_json::Value = self
            .client
            .get(format!("{}/v3/user/baseFolder", GATEWAY))
            .header(
                "Authorization",
                HeaderValue::from_str(&format!("Bearer {}", self.api_key.expose_secret()))
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
            .get(format!("{}/v3/user/account", GATEWAY))
            .header(
                "Authorization",
                HeaderValue::from_str(&format!("Bearer {}", self.api_key.expose_secret()))
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
            self.master_keys.len()
        ));
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.connected = false;
        // F-SEC-01: Replace api_key with empty SecretString (zeroizes old value on drop)
        self.api_key = SecretString::from(String::new());
        // F-SEC-02: Clear master keys (each SecretString zeroizes on drop)
        self.master_keys.clear();
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
            .post(format!("{}/v3/dir/content", GATEWAY))
            .header(
                "Authorization",
                HeaderValue::from_str(&format!("Bearer {}", self.api_key.expose_secret()))
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
                let api_key = self.api_key.clone();
                let file_uuid = file_uuid.clone();
                let parent_uuid = parent_uuid.clone();
                let upload_key = upload_key.clone();
                let file_key = file_key.clone();
                in_flight.push(async move {
                    upload_filen_chunk(
                        &client,
                        &api_key,
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
        let now = chrono::Utc::now().timestamp_millis();
        let metadata = serde_json::json!({
            "name": file_name,
            "size": file_size,
            "mime": mime_type,
            "key": file_key,
            "lastModified": now,
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
            .post(format!("{}/v3/upload/done", GATEWAY))
            .header(
                "Authorization",
                HeaderValue::from_str(&format!("Bearer {}", self.api_key.expose_secret()))
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
            .post(format!("{}/v3/dir/create", GATEWAY))
            .header(
                "Authorization",
                HeaderValue::from_str(&format!("Bearer {}", self.api_key.expose_secret()))
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
            .master_keys
            .iter()
            .map(|k| k.expose_secret().to_string())
            .collect();
        for key in &master_keys_exposed {
            let encrypted_for_key = Self::encrypt_metadata_with_key(&name_json, key)?;
            let meta_request = self
                .client
                .post(format!("{}/v3/dir/metadata", GATEWAY))
                .header(
                    "Authorization",
                    HeaderValue::from_str(&format!("Bearer {}", self.api_key.expose_secret()))
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
            .post(format!("{}/{}", GATEWAY, endpoint))
            .header(
                "Authorization",
                HeaderValue::from_str(&format!("Bearer {}", self.api_key.expose_secret()))
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
            .post(format!("{}/v3/dir/trash", GATEWAY))
            .header(
                "Authorization",
                HeaderValue::from_str(&format!("Bearer {}", self.api_key.expose_secret()))
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
                .post(format!("{}/v3/dir/rename", GATEWAY))
                .header(
                    "Authorization",
                    HeaderValue::from_str(&format!("Bearer {}", self.api_key.expose_secret()))
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
                .master_keys
                .iter()
                .map(|k| k.expose_secret().to_string())
                .collect();
            for key in &master_keys_exposed {
                let enc = Self::encrypt_metadata_with_key(&name_json, key)?;
                let meta_request = self
                    .client
                    .post(format!("{}/v3/dir/metadata", GATEWAY))
                    .header(
                        "Authorization",
                        HeaderValue::from_str(&format!("Bearer {}", self.api_key.expose_secret()))
                            .map_err(|e| {
                                ProviderError::Other(format!("Invalid auth header: {}", e))
                            })?,
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
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;

            let meta_json = serde_json::json!({
                "name": new_name,
                "size": entry.size,
                "mime": mime,
                "key": file_key,
                "lastModified": now,
            });
            let encrypted_metadata = self.encrypt_metadata(&meta_json.to_string())?;

            let request = self
                .client
                .post(format!("{}/v3/file/rename", GATEWAY))
                .header(
                    "Authorization",
                    HeaderValue::from_str(&format!("Bearer {}", self.api_key.expose_secret()))
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
        let request = self
            .client
            .get(format!("{}/v3/user/info", GATEWAY))
            .header(
                "Authorization",
                HeaderValue::from_str(&format!("Bearer {}", self.api_key.expose_secret()))
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
            .post(format!("{}/{}", GATEWAY, endpoint))
            .header(
                "Authorization",
                HeaderValue::from_str(&format!("Bearer {}", self.api_key.expose_secret()))
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
            .post(format!("{}/{}", GATEWAY, endpoint))
            .header(
                "Authorization",
                HeaderValue::from_str(&format!("Bearer {}", self.api_key.expose_secret()))
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

    // TODO (F-FEAT-02): Filen supports file versioning. Use GET v3/file/versions to list
    // previous versions, POST v3/file/version/restore to restore a specific version.
    // Each version has a uuid, size, and timestamp.

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
        "https://ingest.filen.io/v3/upload?uuid={}&index={}&parent={}&uploadKey={}&hash={}",
        file_uuid, index, parent_uuid, upload_key, chunk_hash,
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
        .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

    let status = resp.status();
    let resp_text = resp
        .text()
        .await
        .map_err(|e| ProviderError::ParseError(e.to_string()))?;

    if !status.is_success() {
        return Err(ProviderError::TransferFailed(format!(
            "Upload chunk {} failed: {} - {}",
            index,
            status,
            &resp_text[..resp_text.len().min(200)],
        )));
    }

    let upload_resp: serde_json::Value =
        serde_json::from_str(&resp_text).map_err(|e| ProviderError::ParseError(e.to_string()))?;
    if upload_resp["status"].as_bool() != Some(true) {
        return Err(ProviderError::TransferFailed(format!(
            "Upload chunk {} rejected: {}",
            index,
            upload_resp["message"].as_str().unwrap_or("unknown"),
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
            let retryable =
                status.is_server_error() || status.as_u16() == 408 || status.as_u16() == 429;
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
}
