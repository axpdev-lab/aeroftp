//! Internxt Drive Storage Provider
//!
//! Implements StorageProvider for Internxt Drive using their REST API.
//! Uses client-side AES-256-CTR encryption (zero-knowledge).
//! File content is encrypted locally; filenames are stored as plainName (unencrypted).
//!
//! Auth flow:
//! 1. POST /drive/auth/login {email} → sKey (encrypted salt) + TFA flag
//! 2. Decrypt sKey with AppCryptoSecret → plaintext salt
//! 3. PBKDF2-SHA1(password, salt, 10000, 32) → password hash
//! 4. Encrypt hash with AppCryptoSecret → encrypted password
//! 5. POST /drive/auth/cli/login/access {email, password, tfa} → JWT + encrypted mnemonic
//! 6. Decrypt mnemonic with user password (AES-256-CBC, OpenSSL Salted__ format)
//! 7. BIP39 mnemonic → seed → per-file encryption keys
//!
//! Reference: github.com/internxt/rclone-adapter (Go, open-source)

use async_trait::async_trait;
use aes::cipher::{KeyIvInit, StreamCipher};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use reqwest::header::CONTENT_TYPE;
use secrecy::ExposeSecret;
use serde::Deserialize;
use sha2::{Sha256, Sha512, Digest};
use ripemd::Ripemd160;
use std::collections::HashMap;
use tokio::io::AsyncWriteExt;

use super::{
    StorageProvider, ProviderType, ProviderError, RemoteEntry, StorageInfo,
};
use super::types::InternxtConfig;

// AES-256-CBC type alias (for mnemonic/salt decryption)
type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;
type Aes256CbcEnc = cbc::Encryptor<aes::Aes256>;

// AES-256-CTR type alias (for file content encryption)
type Aes256Ctr = ctr::Ctr128BE<aes::Aes256>;

/// Logging through tracing infrastructure (info level for visibility)
fn internxt_log(msg: &str) {
    tracing::info!(target: "internxt", "{}", msg);
}

/// Internxt API gateway
const GATEWAY: &str = "https://gateway.internxt.com";
const API_URL: &str = "https://api.internxt.com";

/// App-level crypto secret used for salt/password encryption (from Internxt SDK)
const APP_CRYPTO_SECRET: &str = "6KYQBP847D4ATSFA";

/// OpenSSL "Salted__" prefix
const SALTED_PREFIX: &[u8] = b"Salted__";

// ─── Serde Helpers ──────────────────────────────────────────────────────────

/// Deserialize a Vec that might be null in JSON (treat null as empty vec)
fn deserialize_null_vec<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    let opt: Option<Vec<T>> = Option::deserialize(deserializer)?;
    Ok(opt.unwrap_or_default())
}

// ─── API Response Types ────────────────────────────────────────────────────

/// Step 1: POST /drive/auth/login response
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct LoginResponse {
    #[serde(rename = "hasKeys")]
    has_keys: bool,
    #[serde(rename = "sKey")]
    s_key: String,
    tfa: bool,
}

/// Step 2: POST /drive/auth/cli/login/access response
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct AccessResponse {
    user: AccessUser,
    #[serde(default)]
    token: String,
    #[serde(rename = "newToken", default)]
    new_token: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct AccessUser {
    #[serde(default)]
    email: String,
    #[serde(rename = "userId", default)]
    user_id: String,
    #[serde(default)]
    mnemonic: String,
    #[serde(rename = "rootFolderId", default)]
    root_folder_id: String,
    #[serde(default)]
    bucket: String,
    #[serde(rename = "bridgeUser", default)]
    bridge_user: String,
    #[serde(default)]
    uuid: String,
    #[serde(rename = "rootFolderUuid", default)]
    root_folder_uuid: Option<String>,
}

/// Folder listing response wrapper
#[derive(Debug, Deserialize)]
struct FoldersWrapper {
    #[serde(default)]
    folders: Vec<InternxtFolder>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct InternxtFolder {
    uuid: String,
    #[serde(default)]
    id: Option<serde_json::Value>,
    #[serde(rename = "plainName", default)]
    plain_name: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(rename = "parentId", default)]
    parent_id: Option<serde_json::Value>,
    #[serde(rename = "parentUuid", default)]
    parent_uuid: Option<String>,
    #[serde(default)]
    bucket: Option<String>,
    #[serde(rename = "encryptVersion", default)]
    encrypt_version: Option<String>,
    #[serde(default)]
    deleted: Option<bool>,
    #[serde(default)]
    removed: Option<bool>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    size: Option<serde_json::Value>,
    #[serde(rename = "userId", default)]
    user_id: Option<serde_json::Value>,
    #[serde(default)]
    user: Option<serde_json::Value>,
    #[serde(default)]
    parent: Option<serde_json::Value>,
    #[serde(rename = "createdAt", default)]
    created_at: Option<String>,
    #[serde(rename = "updatedAt", default)]
    updated_at: Option<String>,
    #[serde(rename = "creationTime", default)]
    creation_time: Option<String>,
    #[serde(rename = "modificationTime", default)]
    modification_time: Option<String>,
    #[serde(rename = "type", default)]
    folder_type: Option<String>,
}

/// File listing response wrapper
#[derive(Debug, Deserialize)]
struct FilesWrapper {
    #[serde(default)]
    files: Vec<InternxtFile>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct InternxtFile {
    uuid: String,
    #[serde(default)]
    id: Option<serde_json::Value>,
    #[serde(rename = "fileId", default)]
    file_id: Option<String>,
    #[serde(rename = "plainName", default)]
    plain_name: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(rename = "type", default)]
    file_type: Option<String>,
    #[serde(default)]
    bucket: Option<String>,
    #[serde(rename = "userId", default)]
    user_id: Option<serde_json::Value>,
    #[serde(default)]
    user: Option<serde_json::Value>,
    #[serde(rename = "encryptVersion", default)]
    encrypt_version: Option<String>,
    /// Size can be number or string in API response
    #[serde(default)]
    size: Option<serde_json::Value>,
    #[serde(default)]
    deleted: Option<bool>,
    #[serde(default)]
    removed: Option<bool>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    shares: Option<serde_json::Value>,
    #[serde(default)]
    sharings: Option<serde_json::Value>,
    #[serde(default)]
    thumbnails: Option<serde_json::Value>,
    #[serde(rename = "createdAt", default)]
    created_at: Option<String>,
    #[serde(rename = "updatedAt", default)]
    updated_at: Option<String>,
    #[serde(rename = "creationTime", default)]
    creation_time: Option<String>,
    #[serde(rename = "modificationTime", default)]
    modification_time: Option<String>,
    #[serde(rename = "folderId", default)]
    folder_id: Option<serde_json::Value>,
    #[serde(rename = "folderUuid", default)]
    folder_uuid: Option<String>,
    #[serde(default)]
    folder: Option<serde_json::Value>,
}

/// Network file info (for download)
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct BucketFileInfo {
    bucket: Option<String>,
    index: String,
    size: i64,
    #[serde(default)]
    shards: Vec<ShardInfo>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ShardInfo {
    index: i32,
    hash: String,
    url: String,
}

/// Upload start response
#[derive(Debug, Deserialize)]
struct StartUploadResp {
    uploads: Vec<UploadPart>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct UploadPart {
    index: i32,
    uuid: String,
    url: Option<String>,
    #[serde(default, deserialize_with = "deserialize_null_vec")]
    urls: Vec<String>,
    #[serde(rename = "UploadId", default)]
    upload_id: Option<String>,
}

/// Upload finish response
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct FinishUploadResp {
    bucket: Option<String>,
    index: Option<String>,
    id: String,
}

/// Create file metadata response
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct CreateMetaResponse {
    uuid: String,
    #[serde(rename = "plainName")]
    plain_name: Option<String>,
}

/// Create folder response
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct CreateFolderResponse {
    uuid: String,
    #[serde(rename = "plainName")]
    plain_name: Option<String>,
}

/// User usage response
#[derive(Debug, Deserialize)]
struct UsageResponse {
    #[serde(rename = "drive")]
    drive: Option<i64>,
    #[serde(default)]
    total: Option<i64>,
}

/// User limit response
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct LimitResponse {
    #[serde(rename = "maxSpaceBytes")]
    max_space_bytes: i64,
}

// ─── Directory Cache ───────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct DirInfo {
    uuid: String,
}

// ─── Provider Struct ───────────────────────────────────────────────────────

/// Internxt Drive Storage Provider
pub struct InternxtProvider {
    config: InternxtConfig,
    client: reqwest::Client,
    connected: bool,
    /// JWT Bearer token for /drive/* endpoints
    token: String,
    /// Decrypted BIP39 mnemonic (never sent to frontend)
    mnemonic: String,
    /// User's storage bucket ID
    bucket: String,
    /// BasicAuth header for /network/* endpoints: Basic base64(bridgeUser:sha256hex(userId))
    basic_auth: String,
    /// Root folder UUID
    root_folder_id: String,
    /// Current working directory path
    current_path: String,
    /// Current folder UUID
    current_folder_id: String,
    /// Base URL for API requests (gateway or api.internxt.com)
    api_base: String,
    /// Cache: path → DirInfo (uuid, name)
    dir_cache: HashMap<String, DirInfo>,
}

impl InternxtProvider {
    pub fn new(config: InternxtConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .connect_timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            config,
            client,
            connected: false,
            token: String::new(),
            mnemonic: String::new(),
            bucket: String::new(),
            basic_auth: String::new(),
            api_base: GATEWAY.to_string(),
            root_folder_id: String::new(),
            current_path: "/".to_string(),
            current_folder_id: String::new(),
            dir_cache: HashMap::new(),
        }
    }

    // ─── OpenSSL AES-256-CBC Crypto ────────────────────────────────────

    /// Derive AES-256 key and IV from secret + salt using 3 rounds of MD5
    /// (OpenSSL EVP_BytesToKey compatible)
    fn openssl_key_iv(secret: &[u8], salt: &[u8]) -> ([u8; 32], [u8; 16]) {
        use md5::{Md5, Digest as Md5Digest};
        let mut md5_hashes: Vec<Vec<u8>> = Vec::with_capacity(3);
        let mut digest_input = Vec::new();
        digest_input.extend_from_slice(secret);
        digest_input.extend_from_slice(salt);

        for i in 0..3 {
            let mut hasher = Md5::new();
            if i == 0 {
                hasher.update(&digest_input);
            } else {
                hasher.update(&md5_hashes[i - 1]);
                hasher.update(&digest_input);
            }
            md5_hashes.push(hasher.finalize().to_vec());
        }

        let mut key = [0u8; 32];
        key[..16].copy_from_slice(&md5_hashes[0]);
        key[16..].copy_from_slice(&md5_hashes[1]);
        let mut iv = [0u8; 16];
        iv.copy_from_slice(&md5_hashes[2]);
        (key, iv)
    }

    /// Decrypt OpenSSL "Salted__" AES-256-CBC hex-encoded ciphertext
    fn decrypt_text_with_key(encrypted_hex: &str, secret: &str) -> Result<String, ProviderError> {
        let ciphertext = hex::decode(encrypted_hex).map_err(|e| {
            ProviderError::AuthenticationFailed(format!("Failed to decode hex: {}", e))
        })?;

        if ciphertext.len() < 16 {
            return Err(ProviderError::AuthenticationFailed("Ciphertext too short".to_string()));
        }

        if &ciphertext[..8] != SALTED_PREFIX {
            return Err(ProviderError::AuthenticationFailed("Missing OpenSSL Salted__ prefix".to_string()));
        }

        let salt = &ciphertext[8..16];
        let encrypted_content = &ciphertext[16..];

        if encrypted_content.len() % 16 != 0 {
            return Err(ProviderError::AuthenticationFailed("Ciphertext not aligned to block size".to_string()));
        }

        let (key, iv) = Self::openssl_key_iv(secret.as_bytes(), salt);

        let mut buf = encrypted_content.to_vec();
        let decryptor = Aes256CbcDec::new_from_slices(&key, &iv).map_err(|e| {
            ProviderError::AuthenticationFailed(format!("Failed to create AES-CBC decryptor: {}", e))
        })?;

        use aes::cipher::BlockDecryptMut;
        let decrypted = decryptor.decrypt_padded_mut::<aes::cipher::block_padding::Pkcs7>(&mut buf)
            .map_err(|e| {
                ProviderError::AuthenticationFailed(format!("AES-CBC decryption failed: {}", e))
            })?;

        String::from_utf8(decrypted.to_vec()).map_err(|e| {
            ProviderError::AuthenticationFailed(format!("Decrypted text is not valid UTF-8: {}", e))
        })
    }

    /// Encrypt plaintext with AES-256-CBC in OpenSSL Salted__ format → hex
    fn encrypt_text_with_key(plaintext: &str, secret: &str) -> Result<String, ProviderError> {
        use rand::RngCore;
        let mut salt = [0u8; 8];
        rand::thread_rng().fill_bytes(&mut salt);

        let (key, iv) = Self::openssl_key_iv(secret.as_bytes(), &salt);

        let encryptor = Aes256CbcEnc::new_from_slices(&key, &iv).map_err(|e| {
            ProviderError::AuthenticationFailed(format!("Failed to create AES-CBC encryptor: {}", e))
        })?;

        use aes::cipher::BlockEncryptMut;
        // Allocate buffer with padding space
        let block_size = 16;
        let padding_len = block_size - (plaintext.len() % block_size);
        let mut buf = vec![0u8; plaintext.len() + padding_len];
        buf[..plaintext.len()].copy_from_slice(plaintext.as_bytes());
        let padded = encryptor.encrypt_padded_mut::<aes::cipher::block_padding::Pkcs7>(&mut buf, plaintext.len())
            .map_err(|e| ProviderError::AuthenticationFailed(format!("AES-CBC encryption failed: {}", e)))?;

        let mut result = Vec::with_capacity(8 + 8 + padded.len());
        result.extend_from_slice(SALTED_PREFIX);
        result.extend_from_slice(&salt);
        result.extend_from_slice(padded);

        Ok(hex::encode(result))
    }

    /// Decrypt text using the default AppCryptoSecret
    fn decrypt_text(encrypted_hex: &str) -> Result<String, ProviderError> {
        Self::decrypt_text_with_key(encrypted_hex, APP_CRYPTO_SECRET)
    }

    /// Encrypt text using the default AppCryptoSecret
    fn encrypt_text(plaintext: &str) -> Result<String, ProviderError> {
        Self::encrypt_text_with_key(plaintext, APP_CRYPTO_SECRET)
    }

    /// Hash password: PBKDF2-SHA1(password, salt_hex, 10000, 32) → hex
    fn pass_to_hash(password: &str, salt_hex: &str) -> Result<String, ProviderError> {
        let salt = hex::decode(salt_hex).map_err(|e| {
            ProviderError::AuthenticationFailed(format!("Failed to decode salt hex: {}", e))
        })?;

        let mut hash = [0u8; 32];
        pbkdf2::pbkdf2_hmac::<sha1::Sha1>(password.as_bytes(), &salt, 10_000, &mut hash);
        Ok(hex::encode(hash))
    }

    /// Encrypt password hash for the login flow:
    /// 1. Decrypt encrypted salt with AppCryptoSecret
    /// 2. PBKDF2-SHA1(password, salt, 10000) → hash
    /// 3. Encrypt hash with AppCryptoSecret
    fn encrypt_password_hash(password: &str, encrypted_salt: &str) -> Result<String, ProviderError> {
        let salt = Self::decrypt_text(encrypted_salt)?;
        let hash = Self::pass_to_hash(password, &salt)?;
        Self::encrypt_text(&hash)
    }

    // ─── File Encryption (AES-256-CTR) ─────────────────────────────────

    /// Generate BIP39 seed from mnemonic
    fn mnemonic_to_seed(mnemonic: &str) -> Vec<u8> {
        // BIP39 seed: PBKDF2-SHA512(mnemonic, "mnemonic", 2048, 64)
        let mut seed = [0u8; 64];
        pbkdf2::pbkdf2_hmac::<Sha512>(
            mnemonic.as_bytes(),
            b"mnemonic",
            2048,
            &mut seed,
        );
        seed.to_vec()
    }

    /// SHA-512(key || data)
    fn get_file_deterministic_key(key: &[u8], data: &[u8]) -> Vec<u8> {
        let mut hasher = Sha512::new();
        hasher.update(key);
        hasher.update(data);
        hasher.finalize().to_vec()
    }

    /// Derive per-file bucket key from mnemonic + bucket ID
    fn generate_file_bucket_key(mnemonic: &str, bucket_id: &str) -> Result<Vec<u8>, ProviderError> {
        let seed = Self::mnemonic_to_seed(mnemonic);
        let bucket_bytes = hex::decode(bucket_id).map_err(|e| {
            ProviderError::Other(format!("Failed to decode bucket ID: {}", e))
        })?;
        Ok(Self::get_file_deterministic_key(&seed, &bucket_bytes))
    }

    /// Derive per-file AES-256-CTR key and IV from mnemonic + bucket + index
    fn generate_file_key(mnemonic: &str, bucket_id: &str, index_hex: &str) -> Result<([u8; 32], [u8; 16]), ProviderError> {
        let bucket_key = Self::generate_file_bucket_key(mnemonic, bucket_id)?;
        let index_bytes = hex::decode(index_hex).map_err(|e| {
            ProviderError::Other(format!("Failed to decode file index: {}", e))
        })?;

        let det_key = Self::get_file_deterministic_key(&bucket_key[..32], &index_bytes);
        let mut key = [0u8; 32];
        key.copy_from_slice(&det_key[..32]);

        let mut iv = [0u8; 16];
        let iv_len = index_bytes.len().min(16);
        iv[..iv_len].copy_from_slice(&index_bytes[..iv_len]);

        Ok((key, iv))
    }

    /// Decrypt file content using AES-256-CTR
    fn decrypt_file_content(data: &[u8], key: &[u8; 32], iv: &[u8; 16]) -> Result<Vec<u8>, ProviderError> {
        let mut buf = data.to_vec();
        let mut cipher = Aes256Ctr::new(key.into(), iv.into());
        cipher.apply_keystream(&mut buf);
        Ok(buf)
    }

    /// Encrypt file content using AES-256-CTR
    fn encrypt_file_content(data: &[u8], key: &[u8; 32], iv: &[u8; 16]) -> Result<Vec<u8>, ProviderError> {
        // CTR mode: encryption = decryption
        Self::decrypt_file_content(data, key, iv)
    }

    // ─── API Helpers ───────────────────────────────────────────────────

    /// Make authenticated request to /drive/* endpoints (Bearer token)
    fn drive_request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let url = format!("{}/drive{}", self.api_base, path);
        self.client
            .request(method, &url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("internxt-client", "aeroftp")
            .header("internxt-version", "v1.0.436")
    }

    /// Make authenticated request to /network/* endpoints (Basic auth)
    /// Network endpoints always use GATEWAY (gateway.internxt.com), not api.internxt.com
    fn network_request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let url = format!("{}/network{}", GATEWAY, path);
        self.client
            .request(method, &url)
            .header("Authorization", &self.basic_auth)
            .header("internxt-client", "aeroftp")
            .header("internxt-version", "1.0")
    }

    /// Compute BasicAuth header: Basic base64(bridgeUser:sha256hex(userId))
    fn compute_basic_auth(bridge_user: &str, user_id: &str) -> String {
        let hash = hex::encode(Sha256::digest(user_id.as_bytes()));
        let creds = format!("{}:{}", bridge_user, hash);
        format!("Basic {}", BASE64.encode(creds.as_bytes()))
    }

    // ─── Path Resolution ───────────────────────────────────────────────

    /// Resolve a virtual path to a folder UUID, navigating from root
    async fn resolve_folder_uuid(&mut self, path: &str) -> Result<String, ProviderError> {
        let normalized = Self::normalize_path(path);

        // Check cache
        if let Some(info) = self.dir_cache.get(&normalized) {
            return Ok(info.uuid.clone());
        }

        // Root
        if normalized == "/" {
            return Ok(self.root_folder_id.clone());
        }

        // Walk path segments from root
        let mut current_uuid = self.root_folder_id.clone();
        let mut current_path = String::from("/");

        for segment in normalized.trim_matches('/').split('/') {
            if segment.is_empty() {
                continue;
            }

            // Check cache for this intermediate path
            let check_path = if current_path == "/" {
                format!("/{}", segment)
            } else {
                format!("{}/{}", current_path, segment)
            };

            if let Some(info) = self.dir_cache.get(&check_path) {
                current_uuid = info.uuid.clone();
                current_path = check_path;
                continue;
            }

            // List folders in current_uuid to find segment
            let found = self.find_subfolder(&current_uuid, segment).await?;
            match found {
                Some(uuid) => {
                    self.dir_cache.insert(check_path.clone(), DirInfo {
                        uuid: uuid.clone(),
                    });
                    current_uuid = uuid;
                    current_path = check_path;
                }
                None => {
                    return Err(ProviderError::NotFound(format!("Folder not found: {}", path)));
                }
            }
        }

        Ok(current_uuid)
    }

    /// Find a subfolder by name within a parent folder
    async fn find_subfolder(&self, parent_uuid: &str, name: &str) -> Result<Option<String>, ProviderError> {
        let mut offset = 0;
        loop {
            let url = format!("/folders/content/{}/folders?offset={}&limit=50&sort=plainName&order=ASC",
                parent_uuid, offset);

            let resp = self.drive_request(reqwest::Method::GET, &url)
                .send()
                .await
                .map_err(|e| ProviderError::ConnectionFailed(format!("Failed to list folders: {}", e)))?;

            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                return Err(ProviderError::ServerError(format!("List folders failed ({}): {}",
                    status, super::sanitize_api_error(&body))));
            }

            let wrapper: FoldersWrapper = resp.json().await.map_err(|e| {
                ProviderError::ServerError(format!("Failed to parse folders response: {}", e))
            })?;

            for folder in &wrapper.folders {
                let folder_name = folder.plain_name.as_deref()
                    .or(folder.name.as_deref())
                    .unwrap_or("");
                if folder_name.eq_ignore_ascii_case(name) {
                    return Ok(Some(folder.uuid.clone()));
                }
            }

            if wrapper.folders.len() < 50 {
                break;
            }
            offset += 50;
        }
        Ok(None)
    }

    /// Normalize path: ensure leading /, remove trailing /, collapse //, resolve . and ..
    fn normalize_path(path: &str) -> String {
        let trimmed = path.trim();
        if trimmed.is_empty() || trimmed == "." || trimmed == "/" {
            return "/".to_string();
        }

        let mut segments: Vec<&str> = Vec::new();
        for seg in trimmed.split('/') {
            match seg {
                "" | "." => continue,
                ".." => { segments.pop(); }
                s => segments.push(s),
            }
        }

        if segments.is_empty() {
            "/".to_string()
        } else {
            format!("/{}", segments.join("/"))
        }
    }

    /// Resolve path relative to current directory
    fn resolve_path(&self, path: &str) -> String {
        if path.starts_with('/') {
            Self::normalize_path(path)
        } else if path == ".." {
            let parts: Vec<&str> = self.current_path.trim_matches('/').split('/').collect();
            if parts.len() <= 1 {
                "/".to_string()
            } else {
                format!("/{}", parts[..parts.len() - 1].join("/"))
            }
        } else {
            let base = if self.current_path == "/" {
                String::new()
            } else {
                self.current_path.clone()
            };
            Self::normalize_path(&format!("{}/{}", base, path))
        }
    }

    /// Extract parent path and filename from a path
    fn split_path(path: &str) -> (&str, &str) {
        let normalized = path.trim_end_matches('/');
        match normalized.rfind('/') {
            Some(0) => ("/", &normalized[1..]),
            Some(pos) => (&normalized[..pos], &normalized[pos + 1..]),
            None => ("/", normalized),
        }
    }

    /// Find a file by name in a folder, returns (uuid, file_id, bucket)
    async fn find_file_in_folder(&self, folder_uuid: &str, filename: &str) -> Result<Option<(String, String, String)>, ProviderError> {
        let mut offset = 0;
        loop {
            let url = format!("/folders/content/{}/files?offset={}&limit=50&sort=plainName&order=ASC",
                folder_uuid, offset);

            let resp = self.drive_request(reqwest::Method::GET, &url)
                .send()
                .await
                .map_err(|e| ProviderError::ConnectionFailed(format!("Failed to list files: {}", e)))?;

            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                return Err(ProviderError::ServerError(format!("List files failed ({}): {}",
                    status, super::sanitize_api_error(&body))));
            }

            let wrapper: FilesWrapper = resp.json().await.map_err(|e| {
                ProviderError::ServerError(format!("Failed to parse files response: {}", e))
            })?;

            for file in &wrapper.files {
                let fname = Self::get_filename(file);
                if fname.eq_ignore_ascii_case(filename) {
                    return Ok(Some((
                        file.uuid.clone(),
                        file.file_id.clone().unwrap_or_default(),
                        file.bucket.clone().unwrap_or_else(|| self.bucket.clone()),
                    )));
                }
            }

            if wrapper.files.len() < 50 {
                break;
            }
            offset += 50;
        }
        Ok(None)
    }

    /// Get display filename from file entry
    /// Extract size from serde_json::Value (handles both number and string)
    fn extract_size(val: &Option<serde_json::Value>) -> u64 {
        match val {
            Some(serde_json::Value::Number(n)) => n.as_u64().unwrap_or(0),
            Some(serde_json::Value::String(s)) => s.parse::<u64>().unwrap_or(0),
            _ => 0,
        }
    }

    fn get_filename(file: &InternxtFile) -> String {
        let base = file.plain_name.as_deref()
            .or(file.name.as_deref())
            .unwrap_or("unnamed");
        let ext = file.file_type.as_deref().unwrap_or("");
        if ext.is_empty() {
            base.to_string()
        } else {
            format!("{}.{}", base, ext)
        }
    }

    /// Fallback auth using api.internxt.com /drive/auth/login/access (no CLI tier restriction)
    async fn connect_web_auth(&mut self, email: &str, password: &str, tfa: &str, s_key: &str) -> Result<(), ProviderError> {
        internxt_log(&format!("[WEB AUTH] Trying {} /drive/auth/login/access...", API_URL));

        // Re-use sKey from step 1 to encrypt password
        let encrypted_password = Self::encrypt_password_hash(password, s_key)?;

        let web_access_url = format!("{}/drive/auth/login/access", API_URL);
        internxt_log(&format!("[WEB AUTH] POST {}", web_access_url));

        let mut access_body = serde_json::json!({
            "email": email,
            "password": encrypted_password,
        });
        if !tfa.is_empty() {
            access_body["tfa"] = serde_json::Value::String(tfa.to_string());
        }

        let access_resp = self.client
            .post(&web_access_url)
            .header(CONTENT_TYPE, "application/json")
            .header("internxt-client", "aeroftp")
            .json(&access_body)
            .send()
            .await
            .map_err(|e| {
                internxt_log(&format!("[WEB AUTH FAIL] Request error: {}", e));
                ProviderError::ConnectionFailed(format!("Web auth access failed: {}", e))
            })?;

        let status = access_resp.status();
        internxt_log(&format!("[WEB AUTH] Response status: {}", status));

        if !status.is_success() {
            let body = access_resp.text().await.unwrap_or_default();
            internxt_log(&format!("[WEB AUTH FAIL] Body: {}", &body[..body.len().min(200)]));
            return Err(ProviderError::AuthenticationFailed(format!(
                "Authentication failed ({}): {}. Both CLI and web auth endpoints failed.",
                status, super::sanitize_api_error(&body)
            )));
        }

        let access_data: AccessResponse = access_resp.json().await.map_err(|e| {
            ProviderError::AuthenticationFailed(format!("Failed to parse web access response: {}", e))
        })?;

        internxt_log(&format!("[WEB AUTH OK] token_len={}", access_data.token.len()));

        // Decrypt mnemonic
        let decrypted_mnemonic = Self::decrypt_text_with_key(&access_data.user.mnemonic, password)?;
        let word_count = decrypted_mnemonic.split_whitespace().count();
        if word_count != 12 && word_count != 24 {
            return Err(ProviderError::AuthenticationFailed(format!(
                "Invalid mnemonic format (expected 12 or 24 words, got {})", word_count
            )));
        }

        internxt_log(&format!("[WEB AUTH] Mnemonic decrypted OK ({} words)", word_count));

        let token = if access_data.new_token.is_empty() {
            access_data.token.clone()
        } else {
            access_data.new_token.clone()
        };

        self.token = token;
        self.mnemonic = decrypted_mnemonic;
        self.bucket = access_data.user.bucket.clone();
        self.root_folder_id = access_data.user.root_folder_id.clone();
        self.current_folder_id = self.root_folder_id.clone();
        self.basic_auth = Self::compute_basic_auth(
            &access_data.user.bridge_user,
            &access_data.user.user_id,
        );

        // Use api.internxt.com for all subsequent API calls since gateway blocked CLI
        self.api_base = API_URL.to_string();

        self.dir_cache.insert("/".to_string(), DirInfo {
            uuid: self.root_folder_id.clone(),
        });

        self.connected = true;
        internxt_log(&format!("[WEB AUTH] Connected! API: {}", self.api_base));

        // Navigate to initial path if specified
        if let Some(ref initial) = self.config.initial_path {
            let initial = initial.trim().to_string();
            if !initial.is_empty() && initial != "/" {
                let normalized = Self::normalize_path(&initial);
                internxt_log(&format!("[WEB AUTH] Navigating to initial path: {}", normalized));
                match self.resolve_folder_uuid(&normalized).await {
                    Ok(uuid) => {
                        self.current_path = normalized;
                        self.current_folder_id = uuid;
                    }
                    Err(e) => {
                        internxt_log(&format!("[WEB AUTH] Initial path '{}' not found, staying at root: {}", initial, e));
                    }
                }
            }
        }

        Ok(())
    }
}

// ─── StorageProvider Implementation ────────────────────────────────────────

#[async_trait]
impl StorageProvider for InternxtProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn provider_type(&self) -> ProviderType {
        ProviderType::Internxt
    }

    fn display_name(&self) -> String {
        "Internxt Drive".to_string()
    }

    fn account_email(&self) -> Option<String> {
        Some(self.config.email.clone())
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        let email = self.config.email.clone();
        let password = self.config.password.expose_secret().to_string();
        let tfa = self.config.two_factor_code.clone().unwrap_or_default();

        internxt_log(&format!("[CONNECT] email={}, gateway={}", email, GATEWAY));

        // Step 1: POST /drive/auth/login with email → get sKey + TFA flag
        let login_url = format!("{}/drive/auth/login", GATEWAY);
        internxt_log(&format!("[STEP 1] POST {}", login_url));
        let login_body = serde_json::json!({ "email": email });
        let login_resp = self.client
            .post(&login_url)
            .header(CONTENT_TYPE, "application/json")
            .header("internxt-client", "aeroftp")
            .json(&login_body)
            .send()
            .await
            .map_err(|e| {
                internxt_log(&format!("[STEP 1 FAIL] Request error: {}", e));
                ProviderError::ConnectionFailed(format!("Login request failed: {}", e))
            })?;

        let login_status = login_resp.status();
        internxt_log(&format!("[STEP 1] Response status: {}", login_status));

        if !login_status.is_success() {
            let body = login_resp.text().await.unwrap_or_default();
            internxt_log(&format!("[STEP 1 FAIL] Body: {}", &body[..body.len().min(200)]));
            return Err(ProviderError::AuthenticationFailed(format!("Login failed ({}): {}",
                login_status, super::sanitize_api_error(&body))));
        }

        let login_data: LoginResponse = login_resp.json().await.map_err(|e| {
            internxt_log(&format!("[STEP 1 FAIL] JSON parse: {}", e));
            ProviderError::AuthenticationFailed(format!("Failed to parse login response: {}", e))
        })?;

        internxt_log(&format!("[STEP 1 OK] sKey length={}, tfa_required={}", login_data.s_key.len(), login_data.tfa));

        if login_data.tfa && tfa.is_empty() {
            return Err(ProviderError::AuthenticationFailed("2FA code required for this account".to_string()));
        }

        // Step 2: Encrypt password hash
        internxt_log("[STEP 2] Encrypting password with sKey...");
        let encrypted_password = Self::encrypt_password_hash(&password, &login_data.s_key)?;
        internxt_log(&format!("[STEP 2 OK] Encrypted password length={}", encrypted_password.len()));

        // Step 3: POST /drive/auth/cli/login/access
        let access_url = format!("{}/drive/auth/cli/login/access", GATEWAY);
        internxt_log(&format!("[STEP 3] POST {}", access_url));
        let mut access_body = serde_json::json!({
            "email": email,
            "password": encrypted_password,
        });
        if !tfa.is_empty() {
            access_body["tfa"] = serde_json::Value::String(tfa.clone());
        }

        let access_resp = self.client
            .post(&access_url)
            .header(CONTENT_TYPE, "application/json")
            .header("internxt-client", "aeroftp")
            .json(&access_body)
            .send()
            .await
            .map_err(|e| {
                internxt_log(&format!("[STEP 3 FAIL] Request error: {}", e));
                ProviderError::ConnectionFailed(format!("Access request failed: {}", e))
            })?;

        let access_status = access_resp.status();
        internxt_log(&format!("[STEP 3] Response status: {}", access_status));

        if !access_status.is_success() {
            let body = access_resp.text().await.unwrap_or_default();
            internxt_log(&format!("[STEP 3 FAIL] Body: {}", &body[..body.len().min(200)]));

            // Check if this is the 402 free-tier block
            if access_status.as_u16() == 402 {
                internxt_log("[STEP 3] 402 = Free account blocked from CLI access. Trying web auth fallback...");

                // Try alternative: api.internxt.com /drive/auth/login/access
                return self.connect_web_auth(&email, &password, &tfa, &login_data.s_key).await;
            }

            return Err(ProviderError::AuthenticationFailed(format!(
                "Authentication failed ({}): {}. Note: CLI access may require a paid Internxt plan.",
                access_status, super::sanitize_api_error(&body)
            )));
        }

        let access_data: AccessResponse = access_resp.json().await.map_err(|e| {
            internxt_log(&format!("[STEP 3 FAIL] JSON parse: {}", e));
            ProviderError::AuthenticationFailed(format!("Failed to parse access response: {}", e))
        })?;

        internxt_log(&format!("[STEP 3 OK] token_len={}", access_data.token.len()));

        // Step 4: Decrypt mnemonic with user's password
        let decrypted_mnemonic = Self::decrypt_text_with_key(&access_data.user.mnemonic, &password)?;

        // Validate BIP39 mnemonic (basic word count check)
        let word_count = decrypted_mnemonic.split_whitespace().count();
        if word_count != 12 && word_count != 24 {
            return Err(ProviderError::AuthenticationFailed(format!(
                "Invalid mnemonic format (expected 12 or 24 words, got {})", word_count
            )));
        }

        // Store auth state
        let token = if access_data.new_token.is_empty() {
            access_data.token.clone()
        } else {
            access_data.new_token.clone()
        };

        self.token = token;
        self.mnemonic = decrypted_mnemonic;
        self.bucket = access_data.user.bucket.clone();
        self.root_folder_id = access_data.user.root_folder_id.clone();
        self.current_folder_id = self.root_folder_id.clone();
        self.basic_auth = Self::compute_basic_auth(
            &access_data.user.bridge_user,
            &access_data.user.user_id,
        );

        // Cache root
        self.dir_cache.insert("/".to_string(), DirInfo {
            uuid: self.root_folder_id.clone(),
        });

        self.connected = true;
        internxt_log(&format!("Connected! Root folder: {}, Bucket: {}",
            self.root_folder_id, self.bucket));

        // Navigate to initial path if specified
        if let Some(ref initial) = self.config.initial_path {
            let initial = initial.trim().to_string();
            if !initial.is_empty() && initial != "/" {
                let normalized = Self::normalize_path(&initial);
                internxt_log(&format!("[CONNECT] Navigating to initial path: {}", normalized));
                match self.resolve_folder_uuid(&normalized).await {
                    Ok(uuid) => {
                        self.current_path = normalized;
                        self.current_folder_id = uuid;
                    }
                    Err(e) => {
                        internxt_log(&format!("[CONNECT] Initial path '{}' not found, staying at root: {}", initial, e));
                    }
                }
            }
        }

        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.connected = false;
        self.token.clear();
        self.mnemonic.clear();
        self.bucket.clear();
        self.basic_auth.clear();
        self.root_folder_id.clear();
        self.current_folder_id.clear();
        self.current_path = "/".to_string();
        self.api_base = GATEWAY.to_string();
        self.dir_cache.clear();
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let resolved = self.resolve_path(path);
        let folder_uuid = self.resolve_folder_uuid(&resolved).await?;

        internxt_log(&format!("[LIST] path={} uuid={} api_base={}", resolved, folder_uuid, self.api_base));

        let mut entries = Vec::new();

        // List all folders (paginated)
        let mut offset = 0;
        loop {
            let url = format!("/folders/content/{}/folders?offset={}&limit=50&sort=plainName&order=ASC",
                folder_uuid, offset);
            let full_url = format!("{}/drive{}", self.api_base, url);
            internxt_log(&format!("[LIST FOLDERS] GET {}", full_url));

            let resp = self.drive_request(reqwest::Method::GET, &url)
                .send()
                .await
                .map_err(|e| ProviderError::ConnectionFailed(format!("List folders failed: {}", e)))?;

            let status = resp.status();
            internxt_log(&format!("[LIST FOLDERS] Status: {}", status));

            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                internxt_log(&format!("[LIST FOLDERS] Error body: {}", &body[..body.len().min(200)]));
                return Err(ProviderError::ServerError(format!("List folders failed ({}): {}",
                    status, super::sanitize_api_error(&body))));
            }

            let raw_text = resp.text().await.map_err(|e| {
                ProviderError::ServerError(format!("Failed to read folders response: {}", e))
            })?;
            internxt_log(&format!("[LIST FOLDERS] Response ({} bytes, first 500): {}", raw_text.len(), &raw_text[..raw_text.len().min(500)]));
            let wrapper: FoldersWrapper = serde_json::from_str(&raw_text).map_err(|e| {
                internxt_log(&format!("[LIST FOLDERS] Parse error: {} | Full response: {}", e, &raw_text[..raw_text.len().min(1000)]));
                ProviderError::ServerError(format!("Failed to parse folders: {}", e))
            })?;

            let count = wrapper.folders.len();
            for folder in wrapper.folders {
                let name = folder.plain_name
                    .or(folder.name)
                    .unwrap_or_else(|| "unnamed".to_string());

                // Skip deleted/trashed
                if folder.status.as_deref() == Some("TRASHED") || folder.status.as_deref() == Some("DELETED") {
                    continue;
                }

                // Cache this folder
                let folder_path = if resolved == "/" {
                    format!("/{}", name)
                } else {
                    format!("{}/{}", resolved, name)
                };
                let folder_path_clone = folder_path.clone();
                self.dir_cache.insert(folder_path, DirInfo {
                    uuid: folder.uuid.clone(),
                });

                entries.push(RemoteEntry {
                    name: name.clone(),
                    path: folder_path_clone,
                    is_dir: true,
                    size: 0,
                    modified: folder.updated_at.clone(),
                    permissions: None,
                    owner: None,
                    group: None,
                    is_symlink: false,
                    link_target: None,
                    mime_type: None,
                    metadata: Default::default(),
                });
            }

            if count < 50 { break; }
            offset += 50;
        }

        // List all files (paginated)
        offset = 0;
        loop {
            let url = format!("/folders/content/{}/files?offset={}&limit=50&sort=plainName&order=ASC",
                folder_uuid, offset);
            let full_url = format!("{}/drive{}", self.api_base, url);
            internxt_log(&format!("[LIST FILES] GET {}", full_url));

            let resp = self.drive_request(reqwest::Method::GET, &url)
                .send()
                .await
                .map_err(|e| ProviderError::ConnectionFailed(format!("List files failed: {}", e)))?;

            let status = resp.status();
            internxt_log(&format!("[LIST FILES] Status: {}", status));

            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                internxt_log(&format!("[LIST FILES] Error body: {}", &body[..body.len().min(200)]));
                return Err(ProviderError::ServerError(format!("List files failed ({}): {}",
                    status, super::sanitize_api_error(&body))));
            }

            let raw_text = resp.text().await.map_err(|e| {
                ProviderError::ServerError(format!("Failed to read files response: {}", e))
            })?;
            internxt_log(&format!("[LIST FILES] Response ({} bytes, first 500): {}", raw_text.len(), &raw_text[..raw_text.len().min(500)]));
            let wrapper: FilesWrapper = serde_json::from_str(&raw_text).map_err(|e| {
                internxt_log(&format!("[LIST FILES] Parse error: {} | Full response: {}", e, &raw_text[..raw_text.len().min(1000)]));
                ProviderError::ServerError(format!("Failed to parse files: {}", e))
            })?;

            let count = wrapper.files.len();
            for file in wrapper.files {
                // Skip deleted/trashed
                if file.status.as_deref() == Some("TRASHED") || file.status.as_deref() == Some("DELETED") {
                    continue;
                }

                let name = Self::get_filename(&file);
                let size = Self::extract_size(&file.size);
                let mod_time = file.modification_time.clone()
                    .or_else(|| file.updated_at.clone());
                let file_path = if resolved == "/" {
                    format!("/{}", name)
                } else {
                    format!("{}/{}", resolved, name)
                };

                entries.push(RemoteEntry {
                    name,
                    path: file_path,
                    is_dir: false,
                    size,
                    modified: mod_time,
                    permissions: None,
                    owner: None,
                    group: None,
                    is_symlink: false,
                    link_target: None,
                    mime_type: None,
                    metadata: Default::default(),
                });
            }

            if count < 50 { break; }
            offset += 50;
        }

        internxt_log(&format!("[LIST] Total entries: {} (path: {})", entries.len(), resolved));
        Ok(entries)
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> {
        Ok(self.current_path.clone())
    }

    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        let resolved = self.resolve_path(path);
        let uuid = self.resolve_folder_uuid(&resolved).await?;
        self.current_path = resolved;
        self.current_folder_id = uuid;
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
        let resolved = self.resolve_path(remote_path);
        let (parent_path, filename) = Self::split_path(&resolved);
        let parent_path = parent_path.to_string();
        let filename = filename.to_string();
        let parent_uuid = self.resolve_folder_uuid(&parent_path).await?;

        let file_info = self.find_file_in_folder(&parent_uuid, &filename).await?
            .ok_or_else(|| ProviderError::NotFound(format!("File not found: {}", resolved)))?;
        let (file_uuid, file_id, file_bucket) = file_info;

        internxt_log(&format!("Downloading {} (uuid: {}, fileId: {})", filename, file_uuid, file_id));

        // Get bucket file info (shards + encryption index)
        let info_url = format!("/buckets/{}/files/{}/info", file_bucket, file_id);
        let info_resp = self.network_request(reqwest::Method::GET, &info_url)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("Failed to get file info: {}", e)))?;

        if !info_resp.status().is_success() {
            let status = info_resp.status();
            let body = info_resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!("Get file info failed ({}): {}",
                status, super::sanitize_api_error(&body))));
        }

        let bucket_info: BucketFileInfo = info_resp.json().await.map_err(|e| {
            ProviderError::ServerError(format!("Failed to parse file info: {}", e))
        })?;

        // Handle empty files
        if bucket_info.size == 0 {
            let mut file = tokio::fs::File::create(local_path).await.map_err(|e| {
                ProviderError::Other(format!("Failed to create file: {}", e))
            })?;
            file.flush().await.ok();
            return Ok(());
        }

        if bucket_info.shards.is_empty() {
            return Err(ProviderError::ServerError("No shards found for file".to_string()));
        }

        // Derive encryption key from mnemonic + bucket + index
        let (key, iv) = Self::generate_file_key(&self.mnemonic, &file_bucket, &bucket_info.index)?;

        // Download encrypted shard (single-shard only — files <2GB on free/pro plans)
        // TODO: Multi-shard support for very large files if needed
        let shard = &bucket_info.shards[0];
        let dl_resp = self.client
            .get(&shard.url)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("Download failed: {}", e)))?;

        if !dl_resp.status().is_success() {
            return Err(ProviderError::ServerError(format!("Shard download failed: {}", dl_resp.status())));
        }

        let total_size = dl_resp.content_length().unwrap_or(bucket_info.size as u64);
        let encrypted_data = dl_resp.bytes().await.map_err(|e| {
            ProviderError::Other(format!("Failed to read download stream: {}", e))
        })?;

        if let Some(ref progress) = on_progress {
            progress(encrypted_data.len() as u64, total_size);
        }

        // Decrypt
        let decrypted = Self::decrypt_file_content(&encrypted_data, &key, &iv)?;

        // Write to file
        let mut file = tokio::fs::File::create(local_path).await.map_err(|e| {
            ProviderError::Other(format!("Failed to create output file: {}", e))
        })?;
        file.write_all(&decrypted).await.map_err(|e| {
            ProviderError::Other(format!("Failed to write file: {}", e))
        })?;
        file.flush().await.ok();

        internxt_log(&format!("Downloaded {} ({} bytes)", filename, decrypted.len()));
        Ok(())
    }

    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        let tmp = std::env::temp_dir().join(format!("aeroftp_internxt_{}", uuid::Uuid::new_v4()));
        let tmp_str = tmp.to_string_lossy().to_string();
        self.download(remote_path, &tmp_str, None).await?;
        let data = tokio::fs::read(&tmp).await.map_err(|e| {
            ProviderError::Other(format!("Failed to read temp file: {}", e))
        })?;
        let _ = tokio::fs::remove_file(&tmp).await;
        Ok(data)
    }

    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let resolved = self.resolve_path(remote_path);
        let (parent_path, filename) = Self::split_path(&resolved);
        let parent_path = parent_path.to_string();
        let filename = filename.to_string();
        let parent_uuid = self.resolve_folder_uuid(&parent_path).await?;

        // Read file
        let data = tokio::fs::read(local_path).await.map_err(|e| {
            ProviderError::Other(format!("Failed to read file: {}", e))
        })?;
        let plain_size = data.len() as i64;

        internxt_log(&format!("Uploading {} ({} bytes) to {}", filename, plain_size, resolved));

        // Preemptive delete: if file already exists, remove it before uploading.
        // This avoids 409 Conflict and gives the server time to process the delete
        // during the upload/encryption phase.
        if let Some((existing_uuid, _, _)) = self.find_file_in_folder(&parent_uuid, &filename).await? {
            internxt_log(&format!("[UPLOAD] File {} exists, deleting before overwrite...", filename));
            let _ = self.drive_request(reqwest::Method::DELETE, &format!("/files/{}", existing_uuid))
                .send().await;
        }

        if plain_size == 0 {
            let (name, ext) = Self::split_name_ext(&filename);
            self.create_file_meta(None, &parent_uuid, &name, &ext, 0).await?;
            return Ok(());
        }

        // Generate random encryption index
        let mut index_bytes = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut index_bytes);
        let enc_index = hex::encode(index_bytes);

        // Derive per-file key
        let (key, iv) = Self::generate_file_key(&self.mnemonic, &self.bucket, &enc_index)?;

        // Encrypt
        let encrypted = Self::encrypt_file_content(&data, &key, &iv)?;

        if let Some(ref progress) = on_progress {
            progress(0, encrypted.len() as u64);
        }

        // Start upload — network API uses gateway for file storage
        // Retry with exponential backoff (gateway can return 500 timeout)
        let start_url = format!("/v2/buckets/{}/files/start?multiparts=1", self.bucket);
        let start_body = serde_json::json!({
            "uploads": [{ "index": 0, "size": plain_size }]
        });

        let full_start_url = format!("{}/network{}", GATEWAY, start_url);
        internxt_log(&format!("[UPLOAD] POST {} (size={})", full_start_url, plain_size));

        let mut start_data: Option<StartUploadResp> = None;
        let mut last_error = String::new();
        for attempt in 0..3 {
            if attempt > 0 {
                let delay = std::time::Duration::from_millis(1000 * (1 << attempt));
                internxt_log(&format!("[UPLOAD] Retry attempt {} after {:?}...", attempt + 1, delay));
                tokio::time::sleep(delay).await;
            }

            let resp = self.network_request(reqwest::Method::POST, &start_url)
                .header(CONTENT_TYPE, "application/json; charset=utf-8")
                .json(&start_body)
                .send()
                .await;

            let resp = match resp {
                Ok(r) => r,
                Err(e) => {
                    last_error = format!("Request failed: {}", e);
                    internxt_log(&format!("[UPLOAD] Attempt {} failed: {}", attempt + 1, last_error));
                    continue;
                }
            };

            let status = resp.status();
            internxt_log(&format!("[UPLOAD] Attempt {} status: {}", attempt + 1, status));

            if status.as_u16() == 500 || status.as_u16() == 502 || status.as_u16() == 503 {
                let body = resp.text().await.unwrap_or_default();
                last_error = format!("Server error ({}): {}", status, &body[..body.len().min(200)]);
                internxt_log(&format!("[UPLOAD] Attempt {} server error, retrying: {}", attempt + 1, last_error));
                continue;
            }

            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                return Err(ProviderError::ServerError(format!("Start upload failed ({}): {}",
                    status, super::sanitize_api_error(&body))));
            }

            let raw = resp.text().await.map_err(|e| {
                ProviderError::ServerError(format!("Failed to read start upload response: {}", e))
            })?;
            internxt_log(&format!("[UPLOAD] Start response ({} bytes): {}", raw.len(), &raw[..raw.len().min(500)]));

            match serde_json::from_str::<StartUploadResp>(&raw) {
                Ok(data) => {
                    start_data = Some(data);
                    break;
                }
                Err(e) => {
                    last_error = format!("Parse error: {} | Response: {}", e, &raw[..raw.len().min(300)]);
                    internxt_log(&format!("[UPLOAD] {}", last_error));
                    continue;
                }
            }
        }

        let start_data = start_data.ok_or_else(|| {
            ProviderError::ServerError(format!("Start upload failed after 3 attempts: {}", last_error))
        })?;

        if start_data.uploads.is_empty() {
            return Err(ProviderError::ServerError("No upload parts returned".to_string()));
        }

        let part = &start_data.uploads[0];
        let upload_url = if !part.urls.is_empty() {
            part.urls[0].clone()
        } else {
            part.url.clone().unwrap_or_default()
        };

        if upload_url.is_empty() {
            return Err(ProviderError::ServerError("No upload URL provided".to_string()));
        }

        // Transfer encrypted data
        let transfer_resp = self.client
            .put(&upload_url)
            .header(CONTENT_TYPE, "application/octet-stream")
            .body(encrypted.clone())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("Upload transfer failed: {}", e)))?;

        if !transfer_resp.status().is_success() {
            let status = transfer_resp.status();
            let body = transfer_resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!("Upload transfer failed ({}): {}",
                status, super::sanitize_api_error(&body))));
        }

        if let Some(ref progress) = on_progress {
            progress(encrypted.len() as u64, encrypted.len() as u64);
        }

        // Compute hash: RIPEMD-160(SHA-256(encrypted_data)) — matches Internxt web client
        let sha256_result = Sha256::digest(&encrypted);
        let ripemd_hash = hex::encode(Ripemd160::digest(sha256_result));

        // Finish upload
        let finish_url = format!("/v2/buckets/{}/files/finish", self.bucket);
        let finish_body = serde_json::json!({
            "index": enc_index,
            "shards": [{ "hash": ripemd_hash, "uuid": part.uuid }]
        });

        let finish_resp = self.network_request(reqwest::Method::POST, &finish_url)
            .header(CONTENT_TYPE, "application/json; charset=utf-8")
            .json(&finish_body)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("Finish upload failed: {}", e)))?;

        if !finish_resp.status().is_success() {
            let status = finish_resp.status();
            let body = finish_resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!("Finish upload failed ({}): {}",
                status, super::sanitize_api_error(&body))));
        }

        let finish_data: FinishUploadResp = finish_resp.json().await.map_err(|e| {
            ProviderError::ServerError(format!("Failed to parse finish upload: {}", e))
        })?;

        // Create file metadata in Drive
        let (name, ext) = Self::split_name_ext(&filename);
        self.create_file_meta(Some(&finish_data.id), &parent_uuid, &name, &ext, plain_size).await.map_err(|e| {
            // If still 409 despite preemptive delete, provide clear error
            if format!("{}", e).contains("409") {
                ProviderError::ServerError(format!("File {} already exists — try again in a few seconds", filename))
            } else {
                e
            }
        })?;

        internxt_log(&format!("Uploaded {} OK", filename));
        Ok(())
    }

    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let resolved = self.resolve_path(path);
        let (parent_path, folder_name) = Self::split_path(&resolved);
        let parent_path = parent_path.to_string();
        let folder_name = folder_name.to_string();
        let parent_uuid = self.resolve_folder_uuid(&parent_path).await?;

        internxt_log(&format!("Creating folder: {} in {}", folder_name, parent_path));

        let now = chrono::Utc::now().to_rfc3339();
        let body = serde_json::json!({
            "plainName": folder_name,
            "parentFolderUuid": parent_uuid,
            "creationTime": now,
            "modificationTime": now,
        });

        let resp = self.drive_request(reqwest::Method::POST, "/folders")
            .header(CONTENT_TYPE, "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("Create folder failed: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!("Create folder failed ({}): {}",
                status, super::sanitize_api_error(&body))));
        }

        let folder_data: CreateFolderResponse = resp.json().await.map_err(|e| {
            ProviderError::ServerError(format!("Failed to parse create folder response: {}", e))
        })?;

        // Cache new folder
        self.dir_cache.insert(resolved, DirInfo {
            uuid: folder_data.uuid,
        });

        Ok(())
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        let resolved = self.resolve_path(path);
        let (parent_path, filename) = Self::split_path(&resolved);
        let parent_path = parent_path.to_string();
        let filename = filename.to_string();
        let parent_uuid = self.resolve_folder_uuid(&parent_path).await?;

        let file_info = self.find_file_in_folder(&parent_uuid, &filename).await?
            .ok_or_else(|| ProviderError::NotFound(format!("File not found: {}", resolved)))?;

        let resp = self.drive_request(reqwest::Method::DELETE, &format!("/files/{}", file_info.0))
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("Delete failed: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!("Delete failed ({}): {}",
                status, super::sanitize_api_error(&body))));
        }

        Ok(())
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let resolved = self.resolve_path(path);
        let uuid = self.resolve_folder_uuid(&resolved).await?;

        let resp = self.drive_request(reqwest::Method::DELETE, &format!("/folders/{}", uuid))
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("Delete folder failed: {}", e)))?;

        if !resp.status().is_success() && resp.status().as_u16() != 204 {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!("Delete folder failed ({}): {}",
                status, super::sanitize_api_error(&body))));
        }

        // Remove from cache
        self.dir_cache.remove(&resolved);

        Ok(())
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        let resolved = self.resolve_path(path);
        let _uuid = self.resolve_folder_uuid(&resolved).await?;

        // List and delete all contents first
        let entries = self.list(&resolved).await?;
        for entry in entries {
            let child_path = if resolved == "/" {
                format!("/{}", entry.name)
            } else {
                format!("{}/{}", resolved, entry.name)
            };

            if entry.is_dir {
                // Box the recursive future to avoid infinite type
                Box::pin(self.rmdir_recursive(&child_path)).await?;
            } else {
                self.delete(&child_path).await?;
            }
        }

        // Now delete the empty folder
        self.rmdir(&resolved).await
    }

    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let from_resolved = self.resolve_path(from);
        let to_resolved = self.resolve_path(to);
        let (from_parent, from_name) = Self::split_path(&from_resolved);
        let from_parent = from_parent.to_string();
        let from_name = from_name.to_string();
        let (to_parent, to_name) = Self::split_path(&to_resolved);
        let to_parent = to_parent.to_string();
        let to_name = to_name.to_string();
        let from_parent_uuid = self.resolve_folder_uuid(&from_parent).await?;

        // Try as file first
        if let Some((file_uuid, _, _)) = self.find_file_in_folder(&from_parent_uuid, &from_name).await? {
            let (new_plain_name, new_type) = Self::split_name_ext(&to_name);
            let mut payload = serde_json::json!({ "plainName": new_plain_name });
            if !new_type.is_empty() {
                payload["type"] = serde_json::Value::String(new_type);
            }

            let resp = self.drive_request(reqwest::Method::PUT, &format!("/files/{}/meta", file_uuid))
                .header(CONTENT_TYPE, "application/json")
                .json(&payload)
                .send()
                .await
                .map_err(|e| ProviderError::ConnectionFailed(format!("Rename file failed: {}", e)))?;

            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                return Err(ProviderError::ServerError(format!("Rename failed ({}): {}",
                    status, super::sanitize_api_error(&body))));
            }
            return Ok(());
        }

        // Try as folder
        if let Some(folder_info) = self.dir_cache.get(&from_resolved) {
            let _folder_uuid = folder_info.uuid.clone();
            // Folder rename: PUT /folders/{uuid}/meta not supported — use move approach
            // For now, just rename via the API pattern if it's in the same parent
            if from_parent == to_parent {
                // Simple rename — not directly supported by Internxt API for folders
                // The desktop app uses a different approach
                return Err(ProviderError::NotSupported("Folder rename is not supported by the Internxt API".to_string()));
            }
        }

        Err(ProviderError::NotFound(format!("Not found: {}", from_resolved)))
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        let resolved = self.resolve_path(path);
        let (parent_path, name) = Self::split_path(&resolved);
        let parent_path = parent_path.to_string();
        let name = name.to_string();
        let parent_uuid = self.resolve_folder_uuid(&parent_path).await?;

        // Try as file
        if let Some((_, _, _)) = self.find_file_in_folder(&parent_uuid, &name).await? {
            // Re-list to get full entry info
            let entries = self.list(&parent_path).await?;
            for entry in entries {
                if entry.name == name {
                    return Ok(entry);
                }
            }
        }

        // Try as folder
        if self.resolve_folder_uuid(&resolved).await.is_ok() {
            return Ok(RemoteEntry {
                name: name.to_string(),
                path: resolved.clone(),
                is_dir: true,
                size: 0,
                modified: None,
                permissions: None,
                owner: None,
                group: None,
                is_symlink: false,
                link_target: None,
                mime_type: None,
                metadata: Default::default(),
            });
        }

        Err(ProviderError::NotFound(format!("Not found: {}", resolved)))
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
        // JWT tokens last 7 days; no keepalive needed
        Ok(())
    }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        Ok(format!("Internxt Drive ({})", self.config.email))
    }

    async fn storage_info(&mut self) -> Result<StorageInfo, ProviderError> {
        // GET /drive/users/usage
        let usage_resp = self.drive_request(reqwest::Method::GET, "/users/usage")
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("Usage request failed: {}", e)))?;

        let used = if usage_resp.status().is_success() {
            let data: UsageResponse = usage_resp.json().await.unwrap_or(UsageResponse { drive: None, total: None });
            data.total.or(data.drive).unwrap_or(0) as u64
        } else {
            0
        };

        // GET /drive/users/limit
        let limit_resp = self.drive_request(reqwest::Method::GET, "/users/limit")
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("Limit request failed: {}", e)))?;

        let total = if limit_resp.status().is_success() {
            let data: LimitResponse = limit_resp.json().await.unwrap_or(LimitResponse { max_space_bytes: 0 });
            data.max_space_bytes as u64
        } else {
            0
        };

        Ok(StorageInfo {
            used,
            total,
            free: total.saturating_sub(used),
        })
    }
}

// ─── Helper methods (not part of trait) ────────────────────────────────────

impl InternxtProvider {
    /// Split "document.pdf" → ("document", "pdf"), "README" → ("README", "")
    fn split_name_ext(filename: &str) -> (String, String) {
        match filename.rfind('.') {
            Some(pos) if pos > 0 => {
                (filename[..pos].to_string(), filename[pos + 1..].to_string())
            }
            _ => (filename.to_string(), String::new()),
        }
    }

    /// Create file metadata in Drive after network upload
    async fn create_file_meta(
        &self,
        file_id: Option<&str>,
        folder_uuid: &str,
        plain_name: &str,
        file_type: &str,
        size: i64,
    ) -> Result<CreateMetaResponse, ProviderError> {
        let now = chrono::Utc::now().to_rfc3339();

        let mut body = serde_json::json!({
            "name": plain_name,
            "bucket": self.bucket,
            "encryptVersion": "03-aes",
            "folderUuid": folder_uuid,
            "size": size,
            "plainName": plain_name,
            "type": file_type,
            "creationTime": now,
            "date": now,
            "modificationTime": now,
        });

        if let Some(id) = file_id {
            body["fileId"] = serde_json::Value::String(id.to_string());
        }

        let resp = self.drive_request(reqwest::Method::POST, "/files")
            .header(CONTENT_TYPE, "application/json; charset=utf-8")
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("Create file meta failed: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let resp_body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!("Create file meta failed ({}): {}",
                status, super::sanitize_api_error(&resp_body))));
        }

        resp.json::<CreateMetaResponse>().await.map_err(|e| {
            ProviderError::ServerError(format!("Failed to parse create file meta response: {}", e))
        })
    }
}
