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

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use aes::cipher::{KeyIvInit, StreamCipher};
use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use futures_util::future::BoxFuture;
use reqwest::header::CONTENT_TYPE;
use ripemd::Ripemd160;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use sha2::{Digest, Sha256, Sha512};
use std::collections::{HashMap, HashSet};

use super::types::InternxtConfig;
use super::{
    http_retry::{send_with_retry, HttpRetryConfig},
    ProviderError, ProviderType, RemoteEntry, StorageInfo, StorageProvider,
};

// AES-256-CBC type alias (for mnemonic/salt decryption)
type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;
type Aes256CbcEnc = cbc::Encryptor<aes::Aes256>;

// AES-256-CTR type alias (for file content encryption)
type Aes256Ctr = ctr::Ctr128BE<aes::Aes256>;

/// Logging through tracing infrastructure (debug level to reduce noise)
fn internxt_log(msg: &str) {
    tracing::debug!(target: "internxt", "{}", msg);
}

/// Internxt API gateway: acts as a reverse proxy that routes requests:
/// - `gateway.internxt.com/drive/*` → `drive.internxt.com/*` (Drive REST API)
/// - `gateway.internxt.com/network/*` → `api.internxt.com/*` (Bridge/Network storage API)
///
/// Using the gateway simplifies the client to a single base URL for both APIs.
/// All `/drive/*` and `/network/*` prefixed paths in this file rely on this routing.
/// Reference: rclone-adapter uses the same gateway approach.
const GATEWAY: &str = "https://gateway.internxt.com";

/// Well-known application-level crypto secret, identical across all Internxt clients
/// (web, desktop, CLI, rclone adapter). Used for encrypting/decrypting the sKey (salt)
/// and password hash during the login flow. Not a vulnerability: this is public knowledge
/// and is hardcoded in Internxt's open-source SDK: https://github.com/niclas19/sdk
/// Changing this value would break compatibility with all Internxt clients.
const APP_CRYPTO_SECRET: &str = "6KYQBP847D4ATSFA";

/// OpenSSL "Salted__" prefix
const SALTED_PREFIX: &[u8] = b"Salted__";

/// What the CLI access endpoint's 402 means. The server answers it only after
/// accepting the credentials, so whatever fails next is not a credentials problem.
const PLAN_WITHOUT_CLI_ACCESS: &str =
    "this Internxt plan does not include CLI/WebDAV/Rclone access";

/// The error of a refused Internxt login step, `context` naming the step. Every
/// step reads the status the same way: only the server's own 401 blames the
/// credentials; a 402 or 403 is the plan or the account being refused; a 429 is
/// Internxt limiting logins, with the wait it asked for when it sent
/// `Retry-After`; anything else is a server failure.
fn internxt_login_refused(
    context: &str,
    status: reqwest::StatusCode,
    retry_after: Option<&str>,
    body: &str,
) -> ProviderError {
    let detail = format!(
        "{} ({}): {}",
        context,
        status,
        super::sanitize_api_error(body)
    );
    match status.as_u16() {
        401 => ProviderError::AuthenticationFailed(detail),
        402 | 403 => ProviderError::PermissionDenied(detail),
        429 => {
            let wait = match retry_after.map(str::trim).filter(|raw| !raw.is_empty()) {
                Some(raw) => match super::retry_after::parse_retry_after_seconds(raw) {
                    Some(wait) => format!("retry in {} seconds", wait.as_secs()),
                    None => format!("retry after {}", &raw[..raw.floor_char_boundary(64)]),
                },
                None => "retry later".to_string(),
            };
            ProviderError::ServerError(format!(
                "{}: Internxt is limiting logins, {} ({}): {}",
                context,
                wait,
                status,
                super::sanitize_api_error(body)
            ))
        }
        _ => ProviderError::ServerError(detail),
    }
}

/// The `Retry-After` header of a login response, read before its body.
fn login_retry_after(resp: &reqwest::Response) -> Option<String> {
    resp.headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

fn internxt_auth_failure(context: &str, detail: &str) -> ProviderError {
    let clean = detail.trim();
    if clean.is_empty() {
        ProviderError::AuthenticationFailed(context.to_string())
    } else {
        ProviderError::AuthenticationFailed(format!("{}: {}", context, clean))
    }
}

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
    /// rootFolderUuid is available but rootFolderId (numeric) is used instead
    /// for backward compatibility with older API responses and the Bridge/Network API.
    #[serde(rename = "rootFolderUuid", default)]
    root_folder_uuid: Option<String>,
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
    /// JWT Bearer token for /drive/* endpoints (SecretString for memory zeroization)
    token: SecretString,
    /// Decrypted BIP39 mnemonic: never sent to frontend (SecretString for memory zeroization)
    mnemonic: SecretString,
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
    /// Base URL for /drive/* requests, the login included: the gateway (a local fixture in tests)
    api_base: String,
    /// Cache: path → DirInfo (uuid, name)
    /// M3: Capped at DIR_CACHE_MAX_ENTRIES to prevent unbounded memory growth
    dir_cache: HashMap<String, DirInfo>,
    /// HTTP retry configuration for 429/5xx handling
    #[allow(dead_code)]
    retry_config: HttpRetryConfig,
}

/// M3: Maximum number of cached directory entries to prevent unbounded memory growth.
const DIR_CACHE_MAX_ENTRIES: usize = 10_000;

/// Page size asked of the cursor-paginated folder content routes; the server
/// accepts 50 to 1000, and rclone-adapter asks the maximum too.
const CONTENT_PAGE_LIMIT: u32 = 1000;

/// The cursors a folder content listing has followed. A server that hands one
/// back again would loop the listing forever, so that is an error, never an end.
#[derive(Default)]
struct CursorTrail {
    seen: HashSet<String>,
}

impl CursorTrail {
    /// The cursor of the next page to read, or `None` when `next` says the
    /// listing of `kind` (`folders` or `files`) is complete.
    fn follow(
        &mut self,
        kind: &str,
        next: Option<String>,
    ) -> Result<Option<String>, ProviderError> {
        match next {
            None => Ok(None),
            Some(cursor) if self.seen.insert(cursor.clone()) => Ok(Some(cursor)),
            Some(_) => Err(ProviderError::ServerError(format!(
                "List {} failed: the server repeated a page cursor, so the listing cannot be completed",
                kind
            ))),
        }
    }
}

impl InternxtProvider {
    pub fn new(config: InternxtConfig) -> Self {
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
            token: SecretString::from(String::new()),
            mnemonic: SecretString::from(String::new()),
            bucket: String::new(),
            basic_auth: String::new(),
            api_base: GATEWAY.to_string(),
            root_folder_id: String::new(),
            current_path: "/".to_string(),
            current_folder_id: String::new(),
            dir_cache: HashMap::new(),
            retry_config: HttpRetryConfig::default(),
        }
    }

    /// M3: Insert into dir_cache with eviction when cap is reached.
    fn dir_cache_insert(&mut self, key: String, value: DirInfo) {
        if self.dir_cache.len() >= DIR_CACHE_MAX_ENTRIES {
            internxt_log("dir_cache reached cap, evicting all entries");
            self.dir_cache.clear();
        }
        self.dir_cache.insert(key, value);
    }

    // ─── OpenSSL AES-256-CBC Crypto ────────────────────────────────────

    /// Derive AES-256 key and IV from secret + salt using 3 rounds of MD5
    /// (OpenSSL EVP_BytesToKey compatible)
    fn openssl_key_iv(secret: &[u8], salt: &[u8]) -> ([u8; 32], [u8; 16]) {
        use md5::{Digest as Md5Digest, Md5};
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
            return Err(ProviderError::AuthenticationFailed(
                "Ciphertext too short".to_string(),
            ));
        }

        if &ciphertext[..8] != SALTED_PREFIX {
            return Err(ProviderError::AuthenticationFailed(
                "Missing OpenSSL Salted__ prefix".to_string(),
            ));
        }

        let salt = &ciphertext[8..16];
        let encrypted_content = &ciphertext[16..];

        if encrypted_content.len() % 16 != 0 {
            return Err(ProviderError::AuthenticationFailed(
                "Ciphertext not aligned to block size".to_string(),
            ));
        }

        let (key, iv) = Self::openssl_key_iv(secret.as_bytes(), salt);

        let mut buf = encrypted_content.to_vec();
        let decryptor = Aes256CbcDec::new_from_slices(&key, &iv).map_err(|e| {
            ProviderError::AuthenticationFailed(format!(
                "Failed to create AES-CBC decryptor: {}",
                e
            ))
        })?;

        use aes::cipher::BlockDecryptMut;
        let decrypted = decryptor
            .decrypt_padded_mut::<aes::cipher::block_padding::Pkcs7>(&mut buf)
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
            ProviderError::AuthenticationFailed(format!(
                "Failed to create AES-CBC encryptor: {}",
                e
            ))
        })?;

        use aes::cipher::BlockEncryptMut;
        // Allocate buffer with padding space
        let block_size = 16;
        let padding_len = block_size - (plaintext.len() % block_size);
        let mut buf = vec![0u8; plaintext.len() + padding_len];
        buf[..plaintext.len()].copy_from_slice(plaintext.as_bytes());
        let padded = encryptor
            .encrypt_padded_mut::<aes::cipher::block_padding::Pkcs7>(&mut buf, plaintext.len())
            .map_err(|e| {
                ProviderError::AuthenticationFailed(format!("AES-CBC encryption failed: {}", e))
            })?;

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
    fn encrypt_password_hash(
        password: &str,
        encrypted_salt: &str,
    ) -> Result<String, ProviderError> {
        let salt = Self::decrypt_text(encrypted_salt)?;
        let hash = Self::pass_to_hash(password, &salt)?;
        Self::encrypt_text(&hash)
    }

    // ─── File Encryption (AES-256-CTR) ─────────────────────────────────

    /// Generate BIP39 seed from mnemonic
    fn mnemonic_to_seed(mnemonic: &str) -> Vec<u8> {
        // BIP39 seed: PBKDF2-SHA512(mnemonic, "mnemonic", 2048, 64)
        let mut seed = [0u8; 64];
        pbkdf2::pbkdf2_hmac::<Sha512>(mnemonic.as_bytes(), b"mnemonic", 2048, &mut seed);
        seed.to_vec()
    }

    /// SHA-512(key || data)
    fn get_file_deterministic_key(key: &[u8], data: &[u8]) -> Vec<u8> {
        let mut hasher = Sha512::new();
        hasher.update(key);
        hasher.update(data);
        hasher.finalize().to_vec()
    }

    /// Derive per-file bucket key from mnemonic + bucket ID.
    /// Reverse-engineered from rclone adapter: bucket_id is typically hex-encoded,
    /// but some accounts may have non-hex IDs. Fallback to raw bytes if hex decode fails.
    /// TODO: validate with official test vectors once Internxt publishes them
    fn generate_file_bucket_key(mnemonic: &str, bucket_id: &str) -> Result<Vec<u8>, ProviderError> {
        let seed = Self::mnemonic_to_seed(mnemonic);
        // Try hex decode first; if bucket_id contains non-hex chars, use raw bytes
        let bucket_bytes = match hex::decode(bucket_id) {
            Ok(bytes) => bytes,
            Err(_) => {
                tracing::debug!(target: "internxt", "Bucket ID '{}' is not valid hex, using raw bytes", bucket_id);
                bucket_id.as_bytes().to_vec()
            }
        };
        Ok(Self::get_file_deterministic_key(&seed, &bucket_bytes))
    }

    /// Derive per-file AES-256-CTR key and IV from mnemonic + bucket + index.
    /// Key derivation chain: BIP39(mnemonic) → seed → SHA-512(seed, bucket) → bucket_key
    /// → SHA-512(bucket_key[..32], index) → (key[..32], iv = index[..16])
    /// TODO: validate with official test vectors once Internxt publishes them
    fn generate_file_key(
        mnemonic: &str,
        bucket_id: &str,
        index_hex: &str,
    ) -> Result<([u8; 32], [u8; 16]), ProviderError> {
        let bucket_key = Self::generate_file_bucket_key(mnemonic, bucket_id)?;
        let index_bytes = hex::decode(index_hex)
            .map_err(|e| ProviderError::Other(format!("Failed to decode file index: {}", e)))?;

        let det_key = Self::get_file_deterministic_key(&bucket_key[..32], &index_bytes);
        let mut key = [0u8; 32];
        key.copy_from_slice(&det_key[..32]);

        let mut iv = [0u8; 16];
        let iv_len = index_bytes.len().min(16);
        iv[..iv_len].copy_from_slice(&index_bytes[..iv_len]);

        Ok((key, iv))
    }

    /// Decrypt file content using AES-256-CTR
    fn decrypt_file_content(
        data: &[u8],
        key: &[u8; 32],
        iv: &[u8; 16],
    ) -> Result<Vec<u8>, ProviderError> {
        let mut buf = data.to_vec();
        let mut cipher = Aes256Ctr::new(key.into(), iv.into());
        cipher.apply_keystream(&mut buf);
        Ok(buf)
    }

    /// Encrypt file content using AES-256-CTR
    fn encrypt_file_content(
        data: &[u8],
        key: &[u8; 32],
        iv: &[u8; 16],
    ) -> Result<Vec<u8>, ProviderError> {
        // CTR mode: encryption = decryption
        Self::decrypt_file_content(data, key, iv)
    }

    // ─── API Helpers ───────────────────────────────────────────────────

    /// Make authenticated request to /drive/* endpoints (Bearer token).
    /// The `/drive/` prefix is part of the gateway routing convention (see GATEWAY doc).
    fn drive_request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let url = format!("{}/drive{}", self.api_base, path);
        self.client
            .request(method, &url)
            .header(
                "Authorization",
                format!("Bearer {}", self.token.expose_secret()),
            )
            .header("internxt-client", "aeroftp")
            .header("internxt-version", "v1.0.436")
    }

    /// Move the file or folder `uuid` (`kind` is `files` or `folders`) into
    /// the folder `folder_uuid`. `lands_at` is the path it takes there, the
    /// one a 409 names.
    async fn move_item(
        &mut self,
        kind: &str,
        uuid: &str,
        folder_uuid: &str,
        lands_at: &str,
    ) -> Result<(), ProviderError> {
        let payload = serde_json::json!({ "destinationFolder": folder_uuid });
        let path = format!("/{kind}/{uuid}");
        let resp = self
            .send_with_reauth(|this| {
                this.drive_request(reqwest::Method::PATCH, &path)
                    .header(CONTENT_TYPE, "application/json")
                    .json(&payload)
            })
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(rename_refused("Move", status, &body, lands_at));
        }
        Ok(())
    }

    /// Rename the file or folder `uuid` (`kind` is `files` or `folders`) to
    /// `name` in the folder it is in; `lands_at` is the path a 409 names.
    async fn rename_item(
        &mut self,
        kind: &str,
        uuid: &str,
        name: &str,
        lands_at: &str,
    ) -> Result<(), ProviderError> {
        let payload = if kind == "files" {
            let (plain_name, file_type) = Self::split_name_ext(name);
            let mut payload = serde_json::json!({ "plainName": plain_name });
            if !file_type.is_empty() {
                payload["type"] = serde_json::Value::String(file_type);
            }
            payload
        } else {
            serde_json::json!({ "plainName": name })
        };
        let path = format!("/{kind}/{uuid}/meta");
        let resp = self
            .send_with_reauth(|this| {
                this.drive_request(reqwest::Method::PUT, &path)
                    .header(CONTENT_TYPE, "application/json")
                    .json(&payload)
            })
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(rename_refused("Rename", status, &body, lands_at));
        }
        Ok(())
    }

    /// Make authenticated request to /network/* endpoints (Basic auth).
    /// Network endpoints always use GATEWAY (gateway.internxt.com/network/* → api.internxt.com/*),
    /// whatever `api_base` the /drive/* requests use.
    /// This is because the Bridge/Network API only accepts Basic auth via the gateway.
    fn network_request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let url = format!("{}/network{}", GATEWAY, path);
        self.client
            .request(method, &url)
            .header("Authorization", &self.basic_auth)
            .header("internxt-client", "aeroftp")
            .header("internxt-version", "1.0")
    }

    /// Compute BasicAuth header: Basic base64(bridgeUser:sha256hex(userId)).
    /// This format is derived from the rclone adapter source code. The Bridge API
    /// expects SHA-256 of the userId as the password component, not the raw userId.
    /// TODO: verify this matches official Bridge API docs if they become available
    fn compute_basic_auth(bridge_user: &str, user_id: &str) -> String {
        let hash = hex::encode(Sha256::digest(user_id.as_bytes()));
        let creds = format!("{}:{}", bridge_user, hash);
        format!("Basic {}", BASE64.encode(creds.as_bytes()))
    }

    // ─── Path Resolution ───────────────────────────────────────────────

    /// The uuid of the folder at `path`, for a read: each name matched
    /// exactly first, and in another letter case as the fallback.
    async fn resolve_folder_uuid(&mut self, path: &str) -> Result<String, ProviderError> {
        self.resolve_folder(path, true).await
    }

    /// The uuid of the folder at `path` with every name matched exactly, for
    /// a step that changes or destroys what it finds: `rmdir /docs` beside
    /// only `Docs` removed `Docs`, and `rm /docs/x` deleted `Docs/x`. The
    /// cache is trusted, because only exact matches are ever written to it.
    async fn resolve_folder_uuid_exact(&mut self, path: &str) -> Result<String, ProviderError> {
        self.resolve_folder(path, false).await
    }

    /// Whether the folder at `resolved` is cached, that is, was resolved
    /// with every name matched exactly: a folder under it may be cached
    /// under its path. A listing, mkdir or rename under a path resolved
    /// through the fallback caches nothing, or the path of another folder
    /// would hold its children's ids.
    fn is_cached_exactly(&self, resolved: &str) -> bool {
        resolved == "/" || self.dir_cache.contains_key(resolved)
    }

    /// Resolve a virtual path to a folder UUID, navigating from root. Only
    /// exact matches are cached, and nothing below a name matched in another
    /// case: a spelling the fallback resolved (`cd /docs` beside only `Docs`)
    /// kept the uuid of `Docs` after another client made a real `docs`, and
    /// `rm -r /docs` emptied `Docs`.
    async fn resolve_folder(
        &mut self,
        path: &str,
        other_case: bool,
    ) -> Result<String, ProviderError> {
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
        let mut exact_so_far = true;

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
            let found = self
                .find_subfolder(&current_uuid, segment, other_case)
                .await?;
            match found {
                Some((uuid, exact)) => {
                    exact_so_far &= exact;
                    if exact_so_far {
                        self.dir_cache_insert(check_path.clone(), DirInfo { uuid: uuid.clone() });
                    }
                    current_uuid = uuid;
                    current_path = check_path;
                }
                None => {
                    return Err(ProviderError::NotFound(path.to_string()));
                }
            }
        }

        Ok(current_uuid)
    }

    /// Find a subfolder by name within a parent folder: the name as spelled
    /// first, on every page, and when `other_case`, another letter case as
    /// the fallback. Internxt is taken to keep `Docs` and `docs` as two
    /// folders (as rclone lists it), and the first match ignoring the case
    /// walked `/docs` into `Docs` when it was listed first. Returns the uuid
    /// and whether the name matched exactly.
    async fn find_subfolder(
        &mut self,
        parent_uuid: &str,
        name: &str,
        other_case: bool,
    ) -> Result<Option<(String, bool)>, ProviderError> {
        let mut trail = CursorTrail::default();
        let mut cursor: Option<String> = None;
        let mut fallback = None;
        loop {
            let (folders, next) = self
                .content_page::<InternxtFolder>(parent_uuid, "folders", cursor.as_deref())
                .await?;

            for folder in &folders {
                let folder_name = folder
                    .plain_name
                    .as_deref()
                    .or(folder.name.as_deref())
                    .unwrap_or("");
                if folder_name == name {
                    return Ok(Some((folder.uuid.clone(), true)));
                }
                if other_case && fallback.is_none() && folder_name.eq_ignore_ascii_case(name) {
                    fallback = Some((folder.uuid.clone(), false));
                }
            }

            match trail.follow("folders", next)? {
                Some(next) => cursor = Some(next),
                None => return Ok(fallback),
            }
        }
    }

    /// Delete the file `uuid`.
    async fn delete_file_by_uuid(&mut self, uuid: &str) -> Result<(), ProviderError> {
        let resp = self
            .send_with_reauth(|this| {
                this.drive_request(reqwest::Method::DELETE, &format!("/files/{}", uuid))
            })
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Delete failed ({}): {}",
                status,
                super::sanitize_api_error(&body)
            )));
        }
        Ok(())
    }

    /// Delete the folder `uuid`, found at `resolved`.
    async fn delete_folder_by_uuid(
        &mut self,
        uuid: &str,
        resolved: &str,
    ) -> Result<(), ProviderError> {
        let sent = self
            .send_with_reauth(|this| {
                this.drive_request(reqwest::Method::DELETE, &format!("/folders/{}", uuid))
            })
            .await;
        // Whatever the answer, not only the folder's own id: every folder
        // cached under it is gone with it, under every capitalization.
        super::forget_cached_subtree_ignoring_case(&mut self.dir_cache, resolved);
        let resp = sent?;

        if !resp.status().is_success() && resp.status().as_u16() != 204 {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Delete folder failed ({}): {}",
                status,
                super::sanitize_api_error(&body)
            )));
        }
        Ok(())
    }

    /// Delete the folder `uuid`, found at `resolved`, after everything in it,
    /// each item by the uuid its folder's listing gives. By path, the
    /// listing and the deletes resolved `resolved` again through the cache,
    /// and a spelling the fallback had cached there emptied another folder.
    async fn remove_folder_tree(
        &mut self,
        uuid: &str,
        resolved: &str,
    ) -> Result<(), ProviderError> {
        let mut files = Vec::new();
        let mut trail = CursorTrail::default();
        let mut cursor: Option<String> = None;
        loop {
            let (page, next) = self
                .content_page::<InternxtFile>(uuid, "files", cursor.as_deref())
                .await?;
            files.extend(
                page.into_iter()
                    .filter(|f| !matches!(f.status.as_deref(), Some("TRASHED" | "DELETED")))
                    .map(|f| f.uuid),
            );
            match trail.follow("files", next)? {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        let mut folders = Vec::new();
        let mut trail = CursorTrail::default();
        let mut cursor: Option<String> = None;
        loop {
            let (page, next) = self
                .content_page::<InternxtFolder>(uuid, "folders", cursor.as_deref())
                .await?;
            folders.extend(
                page.into_iter()
                    .filter(|f| !matches!(f.status.as_deref(), Some("TRASHED" | "DELETED")))
                    .map(|f| {
                        let name = f.plain_name.or(f.name).unwrap_or_default();
                        (f.uuid, name)
                    }),
            );
            match trail.follow("folders", next)? {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }

        for file in files {
            self.delete_file_by_uuid(&file).await?;
        }
        for (folder, name) in folders {
            let path = format!("{}/{name}", resolved.trim_end_matches('/'));
            Box::pin(self.remove_folder_tree(&folder, &path)).await?;
        }
        self.delete_folder_by_uuid(uuid, resolved).await
    }

    /// Read one page of `GET /folders/v2/content/{folder_uuid}/{kind}`, `kind`
    /// being `folders` or `files`: its items and the cursor of the next page,
    /// `None` on the last one. These cursor routes replace the deprecated
    /// offset ones (`/folders/content/...`, drive-server-wip #1163).
    async fn content_page<T: serde::de::DeserializeOwned>(
        &mut self,
        folder_uuid: &str,
        kind: &str,
        cursor: Option<&str>,
    ) -> Result<(Vec<T>, Option<String>), ProviderError> {
        let url = Self::content_page_url(folder_uuid, kind, cursor);
        tracing::debug!(target: "internxt", "[LIST {}] GET {}/drive{}", kind, self.api_base, url);

        let resp = self
            .send_with_reauth(|this| this.drive_request(reqwest::Method::GET, &url))
            .await?;

        let status = resp.status();
        tracing::debug!(target: "internxt", "[LIST {}] Status: {}", kind, status);
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            tracing::debug!(target: "internxt", "[LIST {}] Error body: {}", kind, &body[..body.floor_char_boundary(200)]);
            return Err(ProviderError::ServerError(format!(
                "List {} failed ({}): {}",
                kind,
                status,
                super::sanitize_api_error(&body)
            )));
        }

        let raw = resp.text().await.map_err(|e| {
            ProviderError::ServerError(format!("Failed to read {} response: {}", kind, e))
        })?;
        tracing::debug!(target: "internxt", "[LIST {}] Response ({} bytes): {}", kind, raw.len(), &raw[..raw.floor_char_boundary(200)]);
        Self::parse_content_page(&raw, kind)
    }

    /// The drive path of a folder content page. The cursor is base64 (`+`, `/`,
    /// `=`), so it goes out form-encoded.
    fn content_page_url(folder_uuid: &str, kind: &str, cursor: Option<&str>) -> String {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        query
            .append_pair("limit", &CONTENT_PAGE_LIMIT.to_string())
            .append_pair("order", "ASC");
        if let Some(cursor) = cursor {
            query.append_pair("cursor", cursor);
        }
        format!(
            "/folders/v2/content/{}/{}?{}",
            folder_uuid,
            kind,
            query.finish()
        )
    }

    /// Parse a folder content page. The `kind` array is required, since a page
    /// without it is not an empty folder; `nextCursor` is a string, or null
    /// (or absent) on the last page.
    fn parse_content_page<T: serde::de::DeserializeOwned>(
        raw: &str,
        kind: &str,
    ) -> Result<(Vec<T>, Option<String>), ProviderError> {
        let invalid = |detail: String| {
            ProviderError::ServerError(format!("Failed to parse {}: {}", kind, detail))
        };
        let mut page: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(raw).map_err(|e| invalid(e.to_string()))?;
        let items = page
            .remove(kind)
            .ok_or_else(|| invalid(format!("the page has no {} array", kind)))?;
        let items: Vec<T> = serde_json::from_value(items).map_err(|e| invalid(e.to_string()))?;
        let next = match page.remove("nextCursor") {
            None | Some(serde_json::Value::Null) => None,
            Some(serde_json::Value::String(cursor)) if cursor.is_empty() => None,
            Some(serde_json::Value::String(cursor)) => Some(cursor),
            Some(other) => return Err(invalid(format!("nextCursor is not a string: {}", other))),
        };
        Ok((items, next))
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
                ".." => {
                    segments.pop();
                }
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

    /// Find a file by name in a folder, returns (uuid, file_id, bucket): the
    /// name as spelled first, on every page, and another letter case as the
    /// fallback. The first match ignoring the case resolved `report.pdf` to
    /// a `Report.pdf` listed before it.
    async fn find_file_in_folder(
        &mut self,
        folder_uuid: &str,
        filename: &str,
    ) -> Result<Option<(String, String, String)>, ProviderError> {
        self.find_file_matching(folder_uuid, filename, true).await
    }

    /// [`Self::find_file_in_folder`] without the fallback to another letter
    /// case, for a step that destroys what it finds: an upload of `a.txt`
    /// beside only `A.txt` deleted `A.txt` first, and `rm /a.txt` deleted it.
    async fn find_exact_file_in_folder(
        &mut self,
        folder_uuid: &str,
        filename: &str,
    ) -> Result<Option<(String, String, String)>, ProviderError> {
        self.find_file_matching(folder_uuid, filename, false).await
    }

    /// The file named `filename` in `folder_uuid`: the exact name first, and
    /// when `other_case`, another letter case as the fallback.
    async fn find_file_matching(
        &mut self,
        folder_uuid: &str,
        filename: &str,
        other_case: bool,
    ) -> Result<Option<(String, String, String)>, ProviderError> {
        let mut trail = CursorTrail::default();
        let mut cursor: Option<String> = None;
        let mut fallback = None;
        loop {
            let (files, next) = self
                .content_page::<InternxtFile>(folder_uuid, "files", cursor.as_deref())
                .await?;

            for file in &files {
                let fname = Self::get_filename(file);
                let exact = fname == filename;
                if exact
                    || (other_case && fallback.is_none() && fname.eq_ignore_ascii_case(filename))
                {
                    let found = (
                        file.uuid.clone(),
                        file.file_id.clone().unwrap_or_default(),
                        file.bucket.clone().unwrap_or_else(|| self.bucket.clone()),
                    );
                    if exact {
                        return Ok(Some(found));
                    }
                    fallback = Some(found);
                }
            }

            match trail.follow("files", next)? {
                Some(next) => cursor = Some(next),
                None => return Ok(fallback),
            }
        }
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
        let base = file
            .plain_name
            .as_deref()
            .or(file.name.as_deref())
            .unwrap_or("unnamed");
        let ext = file.file_type.as_deref().unwrap_or("");
        if ext.is_empty() {
            base.to_string()
        } else {
            format!("{}.{}", base, ext)
        }
    }

    /// Fallback auth on the gateway's web login, /drive/auth/login/access, which has
    /// no CLI tier restriction (the CLI access endpoint answers 402 on free plans).
    /// It used to go to api.internxt.com, a legacy host whose /drive/* paths now hang
    /// until nginx answers 504; every official client uses the gateway.
    /// `cli_refusal` is what the CLI access endpoint answered with its 402.
    async fn connect_web_auth(
        &mut self,
        email: &str,
        password: &str,
        tfa: &str,
        s_key: &str,
        cli_refusal: &str,
    ) -> Result<(), ProviderError> {
        let plan_refused = format!(
            "{} (the CLI login answered 402: {})",
            PLAN_WITHOUT_CLI_ACCESS, cli_refusal
        );
        internxt_log(&format!(
            "[WEB AUTH] Trying {} /drive/auth/login/access...",
            self.api_base
        ));

        // Re-use sKey from step 1 to encrypt password
        let encrypted_password = Self::encrypt_password_hash(password, s_key)?;

        let web_access_url = format!("{}/drive/auth/login/access", self.api_base);
        internxt_log(&format!("[WEB AUTH] POST {}", web_access_url));

        let mut access_body = serde_json::json!({
            "email": email,
            "password": encrypted_password,
        });
        if !tfa.is_empty() {
            access_body["tfa"] = serde_json::Value::String(tfa.to_string());
        }

        let access_resp = self
            .client
            .post(&web_access_url)
            .header(CONTENT_TYPE, "application/json")
            .header("internxt-client", "aeroftp")
            .json(&access_body)
            .send()
            .await
            .map_err(|e| {
                internxt_log(&format!("[WEB AUTH FAIL] Request error: {}", e));
                ProviderError::ConnectionFailed(format!(
                    "{}, and the web login fallback could not be reached: {}",
                    plan_refused, e
                ))
            })?;

        let status = access_resp.status();
        internxt_log(&format!("[WEB AUTH] Response status: {}", status));

        if !status.is_success() {
            let retry_after = login_retry_after(&access_resp);
            let body = access_resp.text().await.unwrap_or_default();
            internxt_log(&format!(
                "[WEB AUTH FAIL] Body: {}",
                &body[..body.floor_char_boundary(200)]
            ));
            return Err(internxt_login_refused(
                &format!("{}, and the web login fallback failed", plan_refused),
                status,
                retry_after.as_deref(),
                &body,
            ));
        }

        let access_data: AccessResponse = access_resp.json().await.map_err(|e| {
            ProviderError::ServerError(format!(
                "{}, and the web login fallback answered an unreadable response: {}",
                plan_refused, e
            ))
        })?;

        internxt_log(&format!(
            "[WEB AUTH OK] token_len={}",
            access_data.token.len()
        ));

        // Decrypt mnemonic
        let decrypted_mnemonic = Self::decrypt_text_with_key(&access_data.user.mnemonic, password)?;
        let word_count = decrypted_mnemonic.split_whitespace().count();
        if word_count != 12 && word_count != 24 {
            return Err(ProviderError::AuthenticationFailed(format!(
                "Invalid mnemonic format (expected 12 or 24 words, got {})",
                word_count
            )));
        }

        internxt_log(&format!(
            "[WEB AUTH] Mnemonic decrypted OK ({} words)",
            word_count
        ));

        let token = if access_data.new_token.is_empty() {
            access_data.token.clone()
        } else {
            access_data.new_token.clone()
        };

        self.token = SecretString::from(token);
        self.mnemonic = SecretString::from(decrypted_mnemonic);
        self.bucket = access_data.user.bucket.clone();
        self.root_folder_id = access_data.user.root_folder_id.clone();
        self.current_folder_id = self.root_folder_id.clone();
        self.basic_auth =
            Self::compute_basic_auth(&access_data.user.bridge_user, &access_data.user.user_id);

        self.dir_cache_insert(
            "/".to_string(),
            DirInfo {
                uuid: self.root_folder_id.clone(),
            },
        );

        self.connected = true;
        internxt_log(&format!("[WEB AUTH] Connected! API: {}", self.api_base));

        // Navigate to initial path if specified
        if let Some(ref initial) = self.config.initial_path {
            let initial = initial.trim().to_string();
            if !initial.is_empty() && initial != "/" {
                let normalized = Self::normalize_path(&initial);
                internxt_log(&format!(
                    "[WEB AUTH] Navigating to initial path: {}",
                    normalized
                ));
                match self.resolve_folder_uuid(&normalized).await {
                    Ok(uuid) => {
                        self.current_path = normalized;
                        self.current_folder_id = uuid;
                    }
                    Err(e) => {
                        internxt_log(&format!(
                            "[WEB AUTH] Initial path '{}' not found, staying at root: {}",
                            initial, e
                        ));
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
        // INT-023: use expose_secret() inline, avoid storing password in a plain String
        // TODO: integrate `zeroize` crate for any unavoidable intermediate Strings
        let tfa = self.config.two_factor_code.clone().unwrap_or_default();

        internxt_log(&format!("[CONNECT] email={}, api={}", email, self.api_base));

        // Step 1: POST /drive/auth/login with email → get sKey + TFA flag
        // (login endpoint uses gateway /drive/ prefix: see GATEWAY doc)
        let login_url = format!("{}/drive/auth/login", self.api_base);
        tracing::debug!(target: "internxt", "[STEP 1] POST {}", login_url);
        let login_body = serde_json::json!({ "email": email });
        let login_resp = self
            .client
            .post(&login_url)
            .header(CONTENT_TYPE, "application/json")
            .header("internxt-client", "aeroftp")
            .json(&login_body)
            .send()
            .await
            .map_err(|e| {
                internxt_log(&format!("[STEP 1 FAIL] Request error: {}", e));
                ProviderError::ConnectionFailed(format!(
                    "Internxt login could not be reached: {}",
                    e
                ))
            })?;

        let login_status = login_resp.status();
        tracing::debug!(target: "internxt", "[STEP 1] Response status: {}", login_status);

        if !login_status.is_success() {
            let retry_after = login_retry_after(&login_resp);
            let body = login_resp.text().await.unwrap_or_default();
            tracing::debug!(target: "internxt", "[STEP 1 FAIL] Body: {}", &body[..body.floor_char_boundary(200)]);
            return Err(internxt_login_refused(
                "Internxt login failed",
                login_status,
                retry_after.as_deref(),
                &body,
            ));
        }

        let login_data: LoginResponse = login_resp.json().await.map_err(|e| {
            internxt_log(&format!("[STEP 1 FAIL] JSON parse: {}", e));
            ProviderError::ServerError(format!(
                "Internxt login answered an unreadable response: {}",
                e
            ))
        })?;

        tracing::debug!(target: "internxt", "[STEP 1 OK] sKey length={}, tfa_required={}", login_data.s_key.len(), login_data.tfa);

        if login_data.tfa && tfa.is_empty() {
            return Err(ProviderError::AuthenticationFailed(
                "2FA code required for this account".to_string(),
            ));
        }

        // Step 2: Encrypt password hash (use expose_secret() inline)
        tracing::debug!(target: "internxt", "[STEP 2] Encrypting password with sKey...");
        let encrypted_password =
            Self::encrypt_password_hash(self.config.password.expose_secret(), &login_data.s_key)?;
        tracing::debug!(target: "internxt", "[STEP 2 OK] Encrypted password length={}", encrypted_password.len());

        // Step 3: POST /drive/auth/cli/login/access
        // (CLI access endpoint uses gateway /drive/ prefix: see GATEWAY doc)
        let access_url = format!("{}/drive/auth/cli/login/access", self.api_base);
        tracing::debug!(target: "internxt", "[STEP 3] POST {}", access_url);
        let mut access_body = serde_json::json!({
            "email": email,
            "password": encrypted_password,
        });
        if !tfa.is_empty() {
            access_body["tfa"] = serde_json::Value::String(tfa.clone());
        }

        let access_resp = self
            .client
            .post(&access_url)
            .header(CONTENT_TYPE, "application/json")
            .header("internxt-client", "aeroftp")
            .json(&access_body)
            .send()
            .await
            .map_err(|e| {
                internxt_log(&format!("[STEP 3 FAIL] Request error: {}", e));
                ProviderError::ConnectionFailed(format!(
                    "Internxt CLI login could not be reached: {}",
                    e
                ))
            })?;

        let access_status = access_resp.status();
        tracing::debug!(target: "internxt", "[STEP 3] Response status: {}", access_status);

        if !access_status.is_success() {
            let retry_after = login_retry_after(&access_resp);
            let body = access_resp.text().await.unwrap_or_default();
            tracing::debug!(target: "internxt", "[STEP 3 FAIL] Body: {}", &body[..body.floor_char_boundary(200)]);

            // Check if this is the 402 free-tier block
            if access_status.as_u16() == 402 {
                internxt_log("[STEP 3] 402 = Free account blocked from CLI access. Trying web auth fallback...");

                // Try the web login on the gateway: /drive/auth/login/access
                let password_clone = self.config.password.expose_secret().to_string();
                let cli_refusal = super::sanitize_api_error(&body);
                let result = self
                    .connect_web_auth(
                        &email,
                        &password_clone,
                        &tfa,
                        &login_data.s_key,
                        &cli_refusal,
                    )
                    .await;
                // password_clone is a plain String on the stack; it will be dropped here.
                // SecretString's zeroize-on-drop still protects the original.
                return result;
            }

            return Err(internxt_login_refused(
                "Internxt CLI login failed",
                access_status,
                retry_after.as_deref(),
                &body,
            ));
        }

        let access_data: AccessResponse = access_resp.json().await.map_err(|e| {
            internxt_log(&format!("[STEP 3 FAIL] JSON parse: {}", e));
            ProviderError::ServerError(format!(
                "Internxt CLI login answered an unreadable response: {}",
                e
            ))
        })?;

        tracing::debug!(target: "internxt", "[STEP 3 OK] token_len={}", access_data.token.len());

        // Step 4: Decrypt mnemonic with user's password (expose_secret() inline)
        let decrypted_mnemonic = Self::decrypt_text_with_key(
            &access_data.user.mnemonic,
            self.config.password.expose_secret(),
        )?;

        // Validate BIP39 mnemonic (basic word count check)
        let word_count = decrypted_mnemonic.split_whitespace().count();
        if word_count != 12 && word_count != 24 {
            return Err(ProviderError::AuthenticationFailed(format!(
                "Invalid mnemonic format (expected 12 or 24 words, got {})",
                word_count
            )));
        }

        // Store auth state
        let token = if access_data.new_token.is_empty() {
            access_data.token.clone()
        } else {
            access_data.new_token.clone()
        };

        self.token = SecretString::from(token);
        self.mnemonic = SecretString::from(decrypted_mnemonic);
        self.bucket = access_data.user.bucket.clone();
        self.root_folder_id = access_data.user.root_folder_id.clone();
        self.current_folder_id = self.root_folder_id.clone();
        self.basic_auth =
            Self::compute_basic_auth(&access_data.user.bridge_user, &access_data.user.user_id);

        // Cache root
        self.dir_cache_insert(
            "/".to_string(),
            DirInfo {
                uuid: self.root_folder_id.clone(),
            },
        );

        self.connected = true;
        internxt_log(&format!(
            "Connected! Root folder: {}, Bucket: {}",
            self.root_folder_id, self.bucket
        ));

        // Navigate to initial path if specified
        if let Some(ref initial) = self.config.initial_path {
            let initial = initial.trim().to_string();
            if !initial.is_empty() && initial != "/" {
                let normalized = Self::normalize_path(&initial);
                internxt_log(&format!(
                    "[CONNECT] Navigating to initial path: {}",
                    normalized
                ));
                match self.resolve_folder_uuid(&normalized).await {
                    Ok(uuid) => {
                        self.current_path = normalized;
                        self.current_folder_id = uuid;
                    }
                    Err(e) => {
                        internxt_log(&format!(
                            "[CONNECT] Initial path '{}' not found, staying at root: {}",
                            initial, e
                        ));
                    }
                }
            }
        }

        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.connected = false;
        // SecretString: replace with empty, old value is zeroized on drop
        self.token = SecretString::from(String::new());
        self.mnemonic = SecretString::from(String::new());
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
        let cache_children = self.is_cached_exactly(&resolved);

        internxt_log(&format!(
            "[LIST] path={} uuid={} api_base={}",
            resolved, folder_uuid, self.api_base
        ));

        let mut entries = Vec::new();

        // List all folders, every page
        let mut trail = CursorTrail::default();
        let mut cursor: Option<String> = None;
        loop {
            let (folders, next) = self
                .content_page::<InternxtFolder>(&folder_uuid, "folders", cursor.as_deref())
                .await?;

            for folder in folders {
                let name = folder
                    .plain_name
                    .or(folder.name)
                    .unwrap_or_else(|| "unnamed".to_string());

                // Skip deleted/trashed
                if folder.status.as_deref() == Some("TRASHED")
                    || folder.status.as_deref() == Some("DELETED")
                {
                    continue;
                }

                // Cache this folder
                let folder_path = if resolved == "/" {
                    format!("/{}", name)
                } else {
                    format!("{}/{}", resolved, name)
                };
                let folder_path_clone = folder_path.clone();
                if cache_children {
                    self.dir_cache_insert(
                        folder_path,
                        DirInfo {
                            uuid: folder.uuid.clone(),
                        },
                    );
                }

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

            match trail.follow("folders", next)? {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }

        // List all files, every page
        let mut trail = CursorTrail::default();
        let mut cursor: Option<String> = None;
        loop {
            let (files, next) = self
                .content_page::<InternxtFile>(&folder_uuid, "files", cursor.as_deref())
                .await?;

            for file in files {
                // Skip deleted/trashed
                if file.status.as_deref() == Some("TRASHED")
                    || file.status.as_deref() == Some("DELETED")
                {
                    continue;
                }

                let name = Self::get_filename(&file);
                let size = Self::extract_size(&file.size);
                let mod_time = file
                    .modification_time
                    .clone()
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

            match trail.follow("files", next)? {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }

        internxt_log(&format!(
            "[LIST] Total entries: {} (path: {})",
            entries.len(),
            resolved
        ));
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

        let file_info = self
            .find_file_in_folder(&parent_uuid, &filename)
            .await?
            .ok_or_else(|| ProviderError::NotFound(resolved.to_string()))?;
        let (file_uuid, file_id, file_bucket) = file_info;

        internxt_log(&format!(
            "Downloading {} (uuid: {}, fileId: {})",
            filename, file_uuid, file_id
        ));

        // Get bucket file info (shards + encryption index)
        let info_url = format!("/buckets/{}/files/{}/info", file_bucket, file_id);
        let info_resp = self
            .send_with_reauth(|this| {
                this.network_request(reqwest::Method::GET, &info_url)
                    .header("x-api-version", "2")
            })
            .await?;

        if !info_resp.status().is_success() {
            let status = info_resp.status();
            let body = info_resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Get file info failed ({}): {}",
                status,
                super::sanitize_api_error(&body)
            )));
        }

        let bucket_info: BucketFileInfo = info_resp
            .json()
            .await
            .map_err(|e| ProviderError::ServerError(format!("Failed to parse file info: {}", e)))?;

        // Handle empty files: atomic write for consistency
        if bucket_info.size == 0 {
            let atomic = super::atomic_write::AtomicFile::new(local_path)
                .await
                .map_err(|e| ProviderError::Other(format!("Failed to create file: {}", e)))?;
            atomic.commit().await.ok();
            return Ok(());
        }

        if bucket_info.shards.is_empty() {
            return Err(ProviderError::ServerError(
                "No shards found for file".to_string(),
            ));
        }

        // Derive encryption key from mnemonic + bucket + index
        let (key, iv) = Self::generate_file_key(
            self.mnemonic.expose_secret(),
            &file_bucket,
            &bucket_info.index,
        )?;

        // Single-shard download. Multi-shard files (very large, typically >5GB) would need
        // sequential shard download and concatenation. Not implemented yet.
        // TODO: Multi-shard download support for very large files
        let shard = &bucket_info.shards[0];
        let dl_resp = self.send_retryable(self.client.get(&shard.url)).await?;

        if !dl_resp.status().is_success() {
            return Err(ProviderError::ServerError(format!(
                "Shard download failed: {}",
                dl_resp.status()
            )));
        }

        let total_size = dl_resp.content_length().unwrap_or(bucket_info.size as u64);
        let encrypted_data = dl_resp
            .bytes()
            .await
            .map_err(|e| ProviderError::Other(format!("Failed to read download stream: {}", e)))?;

        if let Some(ref progress) = on_progress {
            progress(encrypted_data.len() as u64, total_size);
        }

        // Decrypt
        let decrypted = Self::decrypt_file_content(&encrypted_data, &key, &iv)?;

        // Write to file atomically
        let mut atomic = super::atomic_write::AtomicFile::new(local_path)
            .await
            .map_err(|e| ProviderError::Other(format!("Failed to create output file: {}", e)))?;
        atomic
            .write_all(&decrypted)
            .await
            .map_err(|e| ProviderError::Other(format!("Failed to write file: {}", e)))?;
        atomic
            .commit()
            .await
            .map_err(|e| ProviderError::Other(format!("Failed to finalize download: {}", e)))?;

        internxt_log(&format!(
            "Downloaded {} ({} bytes)",
            filename,
            decrypted.len()
        ));
        Ok(())
    }

    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        let tmp = std::env::temp_dir().join(format!("aeroftp_internxt_{}", uuid::Uuid::new_v4()));
        let tmp_str = tmp.to_string_lossy().to_string();
        self.download(remote_path, &tmp_str, None).await?;

        // H2: Check file size before reading to prevent OOM
        let limit = super::MAX_DOWNLOAD_TO_BYTES;
        let metadata = tokio::fs::metadata(&tmp)
            .await
            .map_err(|e| ProviderError::Other(format!("Failed to stat temp file: {}", e)))?;
        if metadata.len() > limit {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(ProviderError::TransferFailed(format!(
                "File too large for in-memory download ({:.1} MB). Use streaming download for files over {:.0} MB.",
                metadata.len() as f64 / 1_048_576.0,
                limit as f64 / 1_048_576.0,
            )));
        }

        let data = tokio::fs::read(&tmp)
            .await
            .map_err(|e| ProviderError::Other(format!("Failed to read temp file: {}", e)))?;
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
        let parent_uuid = self.resolve_folder_uuid_exact(&parent_path).await?;

        // M9: Full file read into memory for client-side AES-256-CTR encryption before upload.
        // Internxt requires encrypted content + HMAC, which needs buffering the entire plaintext.
        // Practical limit: available RAM. For files >1GB, memory pressure may be significant.
        let data = tokio::fs::read(local_path)
            .await
            .map_err(|e| ProviderError::Other(format!("Failed to read file: {}", e)))?;
        let plain_size = data.len() as i64;

        internxt_log(&format!(
            "Uploading {} ({} bytes) to {}",
            filename, plain_size, resolved
        ));

        // Preemptive delete: if file already exists, remove it before uploading.
        // This avoids 409 Conflict and gives the server time to process the delete
        // during the upload/encryption phase. The file of this exact name,
        // never one of another letter case.
        if let Some((existing_uuid, _, _)) = self
            .find_exact_file_in_folder(&parent_uuid, &filename)
            .await?
        {
            internxt_log(&format!(
                "[UPLOAD] File {} exists, deleting before overwrite...",
                filename
            ));
            let _ = self
                .send_with_reauth(|this| {
                    this.drive_request(
                        reqwest::Method::DELETE,
                        &format!("/files/{}", existing_uuid),
                    )
                })
                .await;
        }

        if plain_size == 0 {
            let (name, ext) = Self::split_name_ext(&filename);
            self.create_file_meta(None, &parent_uuid, &name, &ext, 0)
                .await?;
            return Ok(());
        }

        // Generate random encryption index
        let mut index_bytes = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut index_bytes);
        let enc_index = hex::encode(index_bytes);

        // Derive per-file key
        let (key, iv) =
            Self::generate_file_key(self.mnemonic.expose_secret(), &self.bucket, &enc_index)?;

        // Encrypt
        let encrypted = Self::encrypt_file_content(&data, &key, &iv)?;

        if let Some(ref progress) = on_progress {
            progress(0, encrypted.len() as u64);
        }

        // Single-shard upload supports files up to ~5GB on current Internxt plans.
        // For larger files, multi-shard upload with multiparts=N would be needed.
        // TODO: Multi-shard upload for files exceeding single-shard limit
        // Retry with exponential backoff (gateway can return 500 timeout)
        let start_url = format!("/v2/buckets/{}/files/start?multiparts=1", self.bucket);
        let start_body = serde_json::json!({
            "uploads": [{ "index": 0, "size": plain_size }]
        });

        let full_start_url = format!("{}/network{}", GATEWAY, start_url);
        internxt_log(&format!(
            "[UPLOAD] POST {} (size={})",
            full_start_url, plain_size
        ));

        let mut start_data: Option<StartUploadResp> = None;
        let mut last_error = String::new();
        for attempt in 0..3 {
            if attempt > 0 {
                let delay = std::time::Duration::from_millis(1000 * (1 << attempt));
                internxt_log(&format!(
                    "[UPLOAD] Retry attempt {} after {:?}...",
                    attempt + 1,
                    delay
                ));
                tokio::time::sleep(delay).await;
            }

            let resp = self
                .send_with_reauth(|this| {
                    this.network_request(reqwest::Method::POST, &start_url)
                        .header(CONTENT_TYPE, "application/json; charset=utf-8")
                        .json(&start_body)
                })
                .await;

            let resp = match resp {
                Ok(r) => r,
                Err(e) => {
                    last_error = format!("Request failed: {}", e);
                    internxt_log(&format!(
                        "[UPLOAD] Attempt {} failed: {}",
                        attempt + 1,
                        last_error
                    ));
                    continue;
                }
            };

            let status = resp.status();
            internxt_log(&format!(
                "[UPLOAD] Attempt {} status: {}",
                attempt + 1,
                status
            ));

            if status.as_u16() == 500 || status.as_u16() == 502 || status.as_u16() == 503 {
                let body = resp.text().await.unwrap_or_default();
                last_error = format!(
                    "Server error ({}): {}",
                    status,
                    &body[..body.floor_char_boundary(200)]
                );
                internxt_log(&format!(
                    "[UPLOAD] Attempt {} server error, retrying: {}",
                    attempt + 1,
                    last_error
                ));
                continue;
            }

            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                return Err(ProviderError::ServerError(format!(
                    "Start upload failed ({}): {}",
                    status,
                    super::sanitize_api_error(&body)
                )));
            }

            let raw = resp.text().await.map_err(|e| {
                ProviderError::ServerError(format!("Failed to read start upload response: {}", e))
            })?;
            tracing::debug!(target: "internxt", "[UPLOAD] Start response ({} bytes): {}", raw.len(), &raw[..raw.floor_char_boundary(200)]);

            match serde_json::from_str::<StartUploadResp>(&raw) {
                Ok(data) => {
                    start_data = Some(data);
                    break;
                }
                Err(e) => {
                    last_error = format!(
                        "Parse error: {} | Response: {}",
                        e,
                        &raw[..raw.floor_char_boundary(200)]
                    );
                    internxt_log(&format!("[UPLOAD] {}", last_error));
                    continue;
                }
            }
        }

        let start_data = start_data.ok_or_else(|| {
            ProviderError::ServerError(format!(
                "Start upload failed after 3 attempts: {}",
                last_error
            ))
        })?;

        if start_data.uploads.is_empty() {
            return Err(ProviderError::ServerError(
                "No upload parts returned".to_string(),
            ));
        }

        let part = &start_data.uploads[0];
        let upload_url = if !part.urls.is_empty() {
            part.urls[0].clone()
        } else {
            part.url.clone().unwrap_or_default()
        };

        if upload_url.is_empty() {
            return Err(ProviderError::ServerError(
                "No upload URL provided".to_string(),
            ));
        }

        // Transfer encrypted data
        let transfer_resp = self
            .send_retryable(
                self.client
                    .put(&upload_url)
                    .header(CONTENT_TYPE, "application/octet-stream")
                    .body(encrypted.clone()),
            )
            .await?;

        if !transfer_resp.status().is_success() {
            let status = transfer_resp.status();
            let body = transfer_resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Upload transfer failed ({}): {}",
                status,
                super::sanitize_api_error(&body)
            )));
        }

        if let Some(ref progress) = on_progress {
            progress(encrypted.len() as u64, encrypted.len() as u64);
        }

        // RIPEMD-160(SHA-256(encrypted_data)): matches the Internxt web client and rclone adapter.
        // The API field is labeled "sha256_of_encrypted_data" but the actual hash algorithm
        // is RIPEMD-160 wrapping SHA-256, a Bitcoin-style hash (Hash160) used by Internxt's
        // Bridge service for shard integrity verification.
        let sha256_result = Sha256::digest(&encrypted);
        let ripemd_hash = hex::encode(Ripemd160::digest(sha256_result));

        // Finish upload
        let finish_url = format!("/v2/buckets/{}/files/finish", self.bucket);
        let finish_body = serde_json::json!({
            "index": enc_index,
            "shards": [{ "hash": ripemd_hash, "uuid": part.uuid }]
        });

        let finish_resp = self
            .send_with_reauth(|this| {
                this.network_request(reqwest::Method::POST, &finish_url)
                    .header(CONTENT_TYPE, "application/json; charset=utf-8")
                    .json(&finish_body)
            })
            .await?;

        if !finish_resp.status().is_success() {
            let status = finish_resp.status();
            let body = finish_resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Finish upload failed ({}): {}",
                status,
                super::sanitize_api_error(&body)
            )));
        }

        let finish_data: FinishUploadResp = finish_resp.json().await.map_err(|e| {
            ProviderError::ServerError(format!("Failed to parse finish upload: {}", e))
        })?;

        // Create file metadata in Drive
        let (name, ext) = Self::split_name_ext(&filename);
        self.create_file_meta(Some(&finish_data.id), &parent_uuid, &name, &ext, plain_size)
            .await
            .map_err(|e| {
                // If still 409 despite preemptive delete, provide clear error
                if format!("{}", e).contains("409") {
                    ProviderError::ServerError(format!(
                        "File {} already exists: try again in a few seconds",
                        filename
                    ))
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
        let cache_new = self.is_cached_exactly(&parent_path);

        internxt_log(&format!(
            "Creating folder: {} in {}",
            folder_name, parent_path
        ));

        let now = chrono::Utc::now().to_rfc3339();
        let body = serde_json::json!({
            "plainName": folder_name,
            "parentFolderUuid": parent_uuid,
            "creationTime": now,
            "modificationTime": now,
        });

        let resp = self
            .send_with_reauth(|this| {
                this.drive_request(reqwest::Method::POST, "/folders")
                    .header(CONTENT_TYPE, "application/json")
                    .json(&body)
            })
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Create folder failed ({}): {}",
                status,
                super::sanitize_api_error(&body)
            )));
        }

        let folder_data: CreateFolderResponse = resp.json().await.map_err(|e| {
            ProviderError::ServerError(format!("Failed to parse create folder response: {}", e))
        })?;

        // Cache new folder, when its parent was resolved exactly.
        if cache_new {
            self.dir_cache_insert(
                resolved,
                DirInfo {
                    uuid: folder_data.uuid,
                },
            );
        }

        Ok(())
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        let resolved = self.resolve_path(path);
        let (parent_path, filename) = Self::split_path(&resolved);
        let parent_path = parent_path.to_string();
        let filename = filename.to_string();
        let parent_uuid = self.resolve_folder_uuid_exact(&parent_path).await?;

        let file_info = self
            .find_exact_file_in_folder(&parent_uuid, &filename)
            .await?
            .ok_or_else(|| ProviderError::NotFound(resolved.to_string()))?;

        self.delete_file_by_uuid(&file_info.0).await
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let resolved = self.resolve_path(path);
        let uuid = self.resolve_folder_uuid_exact(&resolved).await?;
        self.delete_folder_by_uuid(&uuid, &resolved).await
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        let resolved = self.resolve_path(path);
        // The folder by its exact path, and then everything in it by the
        // uuids of its listings, never by path again.
        let uuid = self.resolve_folder_uuid_exact(&resolved).await?;
        self.remove_folder_tree(&uuid, &resolved).await
    }

    async fn delete_permanent(&mut self, path: &str) -> Result<bool, ProviderError> {
        // Internxt: DELETE /storage/trash with body { items: [{ uuid, type }] }.
        // Use the existing list_trash to recover uuid + type by basename.
        // The uuid is encoded in the synthetic path "[Trash]/{uuid}" set by
        // list_trash; is_dir distinguishes file vs folder for the API "type".
        //
        // Empirical note: on the gateway tested (gateway.internxt.com,
        // 2026-05-09) `DELETE /drive/files/{uuid}` already removes the file
        // hard (the trash listing remains empty after the call) so this
        // override mostly returns Ok(false). It is wired anyway because
        // other Internxt deployments and workspace plans do route through
        // a recoverable trash, in which case the name match + DELETE
        // /storage/trash flow works correctly.
        //
        // The match is by exact name among the items trashed from the
        // path's folder (resolved by exact names): by name alone, a purge of
        // `/new/a.txt` could take a trashed `/old/a.txt`. Ok(false) when
        // the folder is no longer there to tell which item is this path.
        let resolved = self.resolve_path(path);
        let (parent_path, basename) = Self::split_path(&resolved);
        let (parent_path, basename) = (parent_path.to_string(), basename.to_string());
        if basename.is_empty() {
            return Ok(false);
        }
        let parent_uuid = match self.resolve_folder_uuid_exact(&parent_path).await {
            Ok(uuid) => uuid,
            Err(ProviderError::NotFound(_)) => return Ok(false),
            Err(e) => return Err(e),
        };
        let trashed = self.list_trash().await?;
        let from_parent = |e: &&RemoteEntry| e.metadata.get("parent_uuid") == Some(&parent_uuid);
        let target = trashed
            .iter()
            .filter(from_parent)
            .find(|e| e.name == basename)
            .and_then(|e| {
                let uuid = e.path.strip_prefix("[Trash]/")?.to_string();
                if uuid.is_empty() {
                    None
                } else {
                    Some((uuid, e.is_dir))
                }
            });
        let (uuid, is_dir) = match target {
            Some(t) => t,
            None => return Ok(false),
        };
        let kind = if is_dir { "folder" } else { "file" };
        let payload = serde_json::json!({
            "items": [{ "uuid": uuid, "type": kind }]
        });
        let resp = self
            .send_with_reauth(|this| {
                this.drive_request(reqwest::Method::DELETE, "/storage/trash")
                    .json(&payload)
            })
            .await?;
        let status = resp.status();
        if !status.is_success() && status.as_u16() != 204 {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Permanent trash delete failed ({}): {}",
                status,
                super::sanitize_api_error(&body)
            )));
        }
        Ok(true)
    }

    /// A move to the new folder, which keeps the name, and a rename in
    /// place, as needed. Internxt refuses a taken name (409), but at the
    /// second step the first had already happened, so the destination is
    /// looked up first and a taken one refused before any change. When the
    /// destination folder already holds the old name the rename goes first,
    /// in the source folder; if the second step fails the first is undone.
    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let from_resolved = self.resolve_path(from);
        let to_resolved = self.resolve_path(to);
        if from_resolved == to_resolved {
            return Ok(());
        }
        super::refuse_occupied_destination(self, &from_resolved, &to_resolved).await?;
        let (from_parent, from_name) = Self::split_path(&from_resolved);
        let (from_parent, from_name) = (from_parent.to_string(), from_name.to_string());
        let (to_parent, to_name) = Self::split_path(&to_resolved);
        let (to_parent, to_name) = (to_parent.to_string(), to_name.to_string());
        let join = |parent: &str, name: &str| format!("{}/{name}", parent.trim_end_matches('/'));
        let from_parent_uuid = self.resolve_folder_uuid(&from_parent).await?;

        let (kind, uuid) = match self
            .find_file_in_folder(&from_parent_uuid, &from_name)
            .await?
        {
            Some((file_uuid, _, _)) => ("files", file_uuid),
            None => match self.resolve_folder_uuid(&from_resolved).await {
                Ok(folder_uuid) => ("folders", folder_uuid),
                Err(ProviderError::NotFound(_)) => {
                    return Err(ProviderError::NotFound(from_resolved.to_string()))
                }
                Err(e) => return Err(e),
            },
        };

        let renames = from_name != to_name;
        let outcome: Result<(), ProviderError> = async {
            if from_parent == to_parent {
                if renames {
                    self.rename_item(kind, &uuid, &to_name, &to_resolved)
                        .await?;
                }
            } else {
                let to_parent_uuid = self.resolve_folder_uuid(&to_parent).await?;
                let moved_to = join(&to_parent, &from_name);
                let rename_first = renames && self.exists(&moved_to).await?;
                if rename_first {
                    let renamed_at = join(&from_parent, &to_name);
                    if self.exists(&renamed_at).await? {
                        return Err(ProviderError::Other(format!(
                            "Cannot move {from_resolved} to {to_resolved} in two steps without two \
                             items sharing a name: {moved_to} and {renamed_at} both exist"
                        )));
                    }
                    self.rename_item(kind, &uuid, &to_name, &renamed_at).await?;
                    if let Err(e) = self
                        .move_item(kind, &uuid, &to_parent_uuid, &to_resolved)
                        .await
                    {
                        let undone = self
                            .rename_item(kind, &uuid, &from_name, &from_resolved)
                            .await;
                        return Err(super::second_step_failed(
                            &from_resolved,
                            &to_resolved,
                            &renamed_at,
                            e,
                            undone,
                        ));
                    }
                } else {
                    self.move_item(kind, &uuid, &to_parent_uuid, &moved_to)
                        .await?;
                    if renames {
                        if let Err(e) = self.rename_item(kind, &uuid, &to_name, &to_resolved).await
                        {
                            let undone = self
                                .move_item(kind, &uuid, &from_parent_uuid, &from_resolved)
                                .await;
                            return Err(super::second_step_failed(
                                &from_resolved,
                                &to_resolved,
                                &moved_to,
                                e,
                                undone,
                            ));
                        }
                    }
                }
            }
            Ok(())
        }
        .await;

        // Whatever happened, the ids cached for either path, and for
        // everything under them, may now point at a moved folder. Under every
        // capitalization, as in `rmdir`.
        super::forget_cached_subtree_ignoring_case(&mut self.dir_cache, &from_resolved);
        super::forget_cached_subtree_ignoring_case(&mut self.dir_cache, &to_resolved);
        if outcome.is_ok() && kind == "folders" && self.is_cached_exactly(&to_parent) {
            self.dir_cache_insert(to_resolved, DirInfo { uuid });
        }
        outcome
    }

    /// No: Internxt refuses a taken name (409) and has no overwrite on
    /// rename or move, so there is no one-step replace, and the callers that
    /// need one refuse before they write anything.
    async fn supports_atomic_replace(&mut self) -> Result<bool, ProviderError> {
        Ok(false)
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

        // Try as folder. Only an absence is NotFound: a refused lookup says
        // nothing about the path, and read as absent it made the look before
        // a rename report a taken name as free.
        match self.resolve_folder_uuid(&resolved).await {
            Ok(_) => {}
            Err(ProviderError::NotFound(_)) => {
                return Err(ProviderError::NotFound(resolved.to_string()))
            }
            Err(e) => return Err(e),
        }
        Ok(RemoteEntry {
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
        })
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
        if let Err(e) = self.refresh_token_once().await {
            tracing::debug!(target: "internxt", "Token refresh failed: {}, token may still be valid", e);
        }
        Ok(())
    }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        Ok(format!("Internxt Drive ({})", self.config.email))
    }

    async fn storage_info(&mut self) -> Result<StorageInfo, ProviderError> {
        // GET /drive/users/usage
        let usage_resp = self
            .send_with_reauth(|this| this.drive_request(reqwest::Method::GET, "/users/usage"))
            .await?;

        let used = if usage_resp.status().is_success() {
            let data: UsageResponse = usage_resp.json().await.unwrap_or(UsageResponse {
                drive: None,
                total: None,
            });
            data.total.or(data.drive).unwrap_or(0) as u64
        } else {
            0
        };

        // GET /drive/users/limit
        let limit_resp = self
            .send_with_reauth(|this| this.drive_request(reqwest::Method::GET, "/users/limit"))
            .await?;

        let total = if limit_resp.status().is_success() {
            let data: LimitResponse = limit_resp
                .json()
                .await
                .unwrap_or(LimitResponse { max_space_bytes: 0 });
            data.max_space_bytes as u64
        } else {
            0
        };

        Ok(StorageInfo {
            used,
            total,
            free: total.saturating_sub(used),
            versioning_bytes: None,
        })
    }

    fn transfer_optimization_hints(&self) -> super::TransferOptimizationHints {
        // Shaped-graph multipart trait (S3-T07): intentionally NotSupported
        // by design.
        //
        // Internxt's storage layer is built on Storj-style erasure coding:
        // every file is split client-side into k-of-n shards and each
        // shard is uploaded to a different farmer node bridge. The
        // multipart trait's `upload_part` contract (independent
        // append-able byte slices with a deterministic byte offset)
        // does not map onto sharding - parts and shards are not the
        // same thing, and pretending otherwise would corrupt the
        // erasure-coding metadata.
        //
        // A wrapper that buffered shards in memory and pretended each
        // shard was a `upload_part` would defeat both the bandwidth
        // budget (parallel sharding is the whole point of the protocol)
        // and the streaming property the legacy upload() relies on.
        //
        // The legacy `upload()` already streams + shards correctly, so
        // the runner picks that path for every file on this backend.
        super::TransferOptimizationHints {
            supports_resume_download: true,
            ..Default::default()
        }
    }
}

// ─── Helper methods (not part of trait) ────────────────────────────────────

impl InternxtProvider {
    /// Split "document.pdf" → ("document", "pdf"), "README" → ("README", "")
    fn split_name_ext(filename: &str) -> (String, String) {
        match filename.rfind('.') {
            Some(pos) if pos > 0 => (filename[..pos].to_string(), filename[pos + 1..].to_string()),
            _ => (filename.to_string(), String::new()),
        }
    }

    /// Create file metadata in Drive after network upload
    async fn create_file_meta(
        &mut self,
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

        let resp = self
            .send_with_reauth(|this| {
                this.drive_request(reqwest::Method::POST, "/files")
                    .header(CONTENT_TYPE, "application/json; charset=utf-8")
                    .json(&body)
            })
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let resp_body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Create file meta failed ({}): {}",
                status,
                super::sanitize_api_error(&resp_body)
            )));
        }

        resp.json::<CreateMetaResponse>().await.map_err(|e| {
            ProviderError::ServerError(format!("Failed to parse create file meta response: {}", e))
        })
    }

    /// Build and send an HTTP request with retry on 429/5xx.
    /// Accepts a RequestBuilder (from drive_request/network_request + chained modifiers),
    /// builds the final Request, and sends it through the shared http_retry module.
    async fn send_retryable(
        &self,
        rb: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, ProviderError> {
        let request = rb
            .build()
            .map_err(|e| ProviderError::ConnectionFailed(format!("Build request failed: {}", e)))?;
        send_with_retry(&self.client, request, &self.retry_config)
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("Request failed: {}", e)))
    }

    async fn send_auth_checked(
        &self,
        rb: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, ProviderError> {
        let resp = self.send_retryable(rb).await?;
        if resp.status().as_u16() == 401 {
            let body = resp.text().await.unwrap_or_default();
            return Err(internxt_auth_failure(
                "Internxt token expired or invalid",
                &super::sanitize_api_error(&body),
            ));
        }
        Ok(resp)
    }

    async fn with_reauth<T, F>(&mut self, mut op: F) -> Result<T, ProviderError>
    where
        F: for<'a> FnMut(&'a mut Self) -> BoxFuture<'a, Result<T, ProviderError>>,
    {
        match op(self).await {
            Err(ProviderError::AuthenticationFailed(msg)) => {
                tracing::warn!(
                    target: "internxt",
                    "Authenticated Internxt request failed; refreshing credentials and retrying once: {}",
                    msg
                );
                self.reauth().await?;
                op(self).await
            }
            other => other,
        }
    }

    async fn send_with_reauth<F>(
        &mut self,
        mut build: F,
    ) -> Result<reqwest::Response, ProviderError>
    where
        F: FnMut(&Self) -> reqwest::RequestBuilder,
    {
        self.with_reauth(|this| {
            let rb = build(this);
            Box::pin(async move { this.send_auth_checked(rb).await })
        })
        .await
    }

    async fn refresh_token_once(&mut self) -> Result<(), ProviderError> {
        let resp = self
            .send_retryable(self.drive_request(reqwest::Method::GET, "/users/refresh"))
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(internxt_auth_failure(
                "Internxt token refresh failed",
                &format!("{}: {}", status, super::sanitize_api_error(&body)),
            ));
        }

        let body = resp.text().await.unwrap_or_default();
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&body) {
            if let Some(new_token) = parsed.get("newToken").and_then(|v| v.as_str()) {
                if !new_token.is_empty() {
                    self.token = SecretString::from(new_token.to_string());
                    tracing::debug!(target: "internxt", "Token refreshed successfully");
                }
            }
        }
        Ok(())
    }

    fn reauth(&mut self) -> BoxFuture<'_, Result<(), ProviderError>> {
        Box::pin(async move {
            if self.refresh_token_once().await.is_ok() {
                return Ok(());
            }

            let previous_path = self.current_path.clone();
            tracing::info!(target: "internxt", "Falling back to fresh Internxt login after refresh failure");
            self.connect().await?;

            if previous_path != "/" {
                match Box::pin(self.resolve_folder_uuid(&previous_path)).await {
                    Ok(uuid) => {
                        self.current_path = previous_path;
                        self.current_folder_id = uuid;
                    }
                    Err(e) => {
                        tracing::debug!(
                            target: "internxt",
                            "Could not restore Internxt cwd after reauth: {}",
                            e
                        );
                    }
                }
            }
            Ok(())
        })
    }

    // TODO: Share links via POST /storage/share/{type}/{id}
    // TODO: Versioning via GET /files/{uuid}/versions
    // TODO: Workspace support (enterprise feature)
    // TODO: Search via POST /users/search or GET /fuzzy/{query}

    /// List trashed files and folders (paginated).
    /// Uses GET /drive/storage/trash/paginated.
    /// The endpoint requires both `type` (files|folders) and `root` (bool)
    /// query parameters: omitting either returns 400 Bad Request. So we
    /// page through `type=folders` and `type=files` separately and merge
    /// the results.
    #[allow(dead_code)]
    pub async fn list_trash(&mut self) -> Result<Vec<RemoteEntry>, ProviderError> {
        let mut results = Vec::new();

        // First pass: trashed folders.
        self.fetch_trash_page("folders", &mut results).await?;
        // Second pass: trashed files.
        self.fetch_trash_page("files", &mut results).await?;

        internxt_log(&format!("[TRASH] Listed {} trashed items", results.len()));
        Ok(results)
    }

    async fn fetch_trash_page(
        &mut self,
        kind: &str,
        out: &mut Vec<RemoteEntry>,
    ) -> Result<(), ProviderError> {
        let limit: usize = 50;
        let mut offset: usize = 0;
        loop {
            let url = format!(
                "/storage/trash/paginated?offset={}&limit={}&type={}&root=true",
                offset, limit, kind
            );
            let resp = self
                .send_with_reauth(|this| this.drive_request(reqwest::Method::GET, &url))
                .await?;

            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                return Err(ProviderError::ServerError(format!(
                    "List trash failed ({}): {}",
                    status,
                    super::sanitize_api_error(&body)
                )));
            }

            let raw = resp.text().await.map_err(|e| {
                ProviderError::ServerError(format!("Failed to read trash response: {}", e))
            })?;
            let parsed: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
                ProviderError::ServerError(format!("Failed to parse trash response: {}", e))
            })?;

            // The Internxt API returns either { "result": [...] } (newer
            // shape) or a top-level array { "files": [...], "folders": [...] }
            // (older shape). Be lenient with both.
            let items = parsed
                .get("result")
                .and_then(|v| v.as_array())
                .or_else(|| parsed.get(kind).and_then(|v| v.as_array()))
                .cloned()
                .unwrap_or_default();

            let page_count = items.len();
            for item in items {
                let uuid = item
                    .get("uuid")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let updated = item
                    .get("updatedAt")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                // The folder the item was trashed from: `folderUuid` on a
                // file, `parentUuid` on a folder.
                let mut metadata = HashMap::new();
                if let Some(parent) = item
                    .get("folderUuid")
                    .or_else(|| item.get("parentUuid"))
                    .and_then(|v| v.as_str())
                {
                    metadata.insert("parent_uuid".to_string(), parent.to_string());
                }
                if kind == "folders" {
                    let name = item
                        .get("plainName")
                        .or_else(|| item.get("name"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("unnamed")
                        .to_string();
                    out.push(RemoteEntry {
                        name,
                        path: format!("[Trash]/{}", uuid),
                        is_dir: true,
                        size: 0,
                        modified: updated,
                        permissions: None,
                        owner: None,
                        group: None,
                        is_symlink: false,
                        link_target: None,
                        mime_type: None,
                        metadata,
                    });
                } else {
                    let plain_name = item
                        .get("plainName")
                        .or_else(|| item.get("name"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("unnamed");
                    let file_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    let name = if file_type.is_empty() {
                        plain_name.to_string()
                    } else {
                        format!("{}.{}", plain_name, file_type)
                    };
                    let size = item.get("size").and_then(|v| v.as_u64()).unwrap_or(0);
                    out.push(RemoteEntry {
                        name,
                        path: format!("[Trash]/{}", uuid),
                        is_dir: false,
                        size,
                        modified: updated,
                        permissions: None,
                        owner: None,
                        group: None,
                        is_symlink: false,
                        link_target: None,
                        mime_type: None,
                        metadata,
                    });
                }
            }

            if page_count < limit {
                break;
            }
            offset += limit;
        }
        Ok(())
    }

    /// Whether this provider supports trash management
    #[allow(dead_code)]
    pub fn supports_trash(&self) -> bool {
        true
    }
}

/// The error of a refused move or rename step. Internxt answers a taken name
/// with 409: that is AlreadyExists (the CLI's exit 9), the one sync and
/// `mkdir -p` handle.
fn rename_refused(what: &str, status: reqwest::StatusCode, body: &str, to: &str) -> ProviderError {
    if status == reqwest::StatusCode::CONFLICT {
        return ProviderError::AlreadyExists(to.to_string());
    }
    ProviderError::ServerError(format!(
        "{what} failed ({}): {}",
        status,
        super::sanitize_api_error(body)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Internxt refuses a taken name and has no overwrite on rename or move, so
    /// there is no one-step replace. The answer is no, so the callers that need
    /// one (CLI `edit`, MCP `remote_edit`, the crypt marker paths) refuse before
    /// they write.
    #[tokio::test]
    async fn internxt_does_not_claim_an_atomic_replace() {
        let mut p = test_provider();
        assert!(!p.supports_atomic_replace().await.unwrap());
        assert!(!p.replace_sets_aside());
    }

    /// Internxt refuses a move or rename onto a taken name with 409, which
    /// reached the caller as a server error.
    #[test]
    fn a_taken_name_is_already_exists() {
        let taken = rename_refused(
            "Rename",
            reqwest::StatusCode::CONFLICT,
            r#"{"error":"File already exists"}"#,
            "/b.txt",
        );
        assert!(
            matches!(taken, ProviderError::AlreadyExists(_)),
            "{taken:?}"
        );
        let other = rename_refused("Rename", reqwest::StatusCode::BAD_GATEWAY, "", "/b.txt");
        assert!(matches!(other, ProviderError::ServerError(_)), "{other:?}");
    }

    /// An Internxt drive double over the root `R`, which holds the folders
    /// `src` (`S`) and `dst` (`D`), and `files` (uuid, name, folder) kept in
    /// memory. A move (PATCH) or a rename (PUT `/meta`) onto a name its
    /// folder holds answers 409, as Internxt does; a rename to a name
    /// starting with `fail`, and any change to a folder but a delete,
    /// answers 403. A rename to a name starting with `failsquat` also puts
    /// another `a.txt` (`SQ`) in `src`, as a second client taking the name
    /// meanwhile. A delete removes, and `POST /files` (the metadata of an
    /// upload) adds the file `NEW`. A file held in the folder `trash:F` is in
    /// the trash, trashed from `F`, and a purge from the trash removes it.
    /// Returns a provider on it, the files, and every change as `METHOD
    /// path`, a purge as `PURGE uuid`.
    #[allow(clippy::type_complexity)]
    async fn provider_on_drive(
        files: &[(&str, &str, &str)],
    ) -> (
        InternxtProvider,
        std::sync::Arc<std::sync::Mutex<Vec<(String, String, String)>>>,
        std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    ) {
        provider_on_drive_with(&[("S", "src", "R"), ("D", "dst", "R")], files).await
    }

    /// [`provider_on_drive`] over the folders `folders` (uuid, name, parent)
    /// in place of `src` and `dst`.
    #[allow(clippy::type_complexity)]
    async fn provider_on_drive_with(
        folders: &[(&str, &str, &str)],
        files: &[(&str, &str, &str)],
    ) -> (
        InternxtProvider,
        std::sync::Arc<std::sync::Mutex<Vec<(String, String, String)>>>,
        std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    ) {
        let (provider, _, store, changes) = provider_on_drive_tree(folders, files).await;
        (provider, store, changes)
    }

    /// [`provider_on_drive_with`], also returning the folders, which a test
    /// may change as another client would.
    #[allow(clippy::type_complexity)]
    async fn provider_on_drive_tree(
        folders: &[(&str, &str, &str)],
        files: &[(&str, &str, &str)],
    ) -> (
        InternxtProvider,
        std::sync::Arc<std::sync::Mutex<Vec<(String, String, String)>>>,
        std::sync::Arc<std::sync::Mutex<Vec<(String, String, String)>>>,
        std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    ) {
        use axum::response::IntoResponse;
        use std::sync::{Arc, Mutex};
        let folders: Arc<Mutex<Vec<(String, String, String)>>> = Arc::new(Mutex::new(
            folders
                .iter()
                .map(|(uuid, name, parent)| {
                    (uuid.to_string(), name.to_string(), parent.to_string())
                })
                .collect(),
        ));
        let tree = Arc::clone(&folders);
        let store: Arc<Mutex<Vec<(String, String, String)>>> = Arc::new(Mutex::new(
            files
                .iter()
                .map(|(uuid, name, folder)| {
                    (uuid.to_string(), name.to_string(), folder.to_string())
                })
                .collect(),
        ));
        let changes: Arc<Mutex<Vec<String>>> = Arc::default();
        let (items, seen) = (Arc::clone(&store), Arc::clone(&changes));
        let app = axum::Router::new().fallback(axum::routing::any(
            move |req: axum::extract::Request| {
                let (items, seen, folders) =
                    (Arc::clone(&items), Arc::clone(&seen), Arc::clone(&tree));
                async move {
                    let method = req.method().clone();
                    let path = req.uri().path().to_string();
                    let query = req.uri().query().unwrap_or("").to_string();
                    let body = axum::body::to_bytes(req.into_body(), 1 << 16).await.unwrap();
                    let args: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
                    let mut items = items.lock().unwrap();
                    let conflict = || {
                        (
                            axum::http::StatusCode::CONFLICT,
                            r#"{"error":"A file with this name already exists"}"#,
                        )
                            .into_response()
                    };
                    if method == axum::http::Method::GET {
                        let body = match path.as_str() {
                            // The trash, files only: a file trashed from the
                            // folder `F` is held with the folder `trash:F`.
                            "/drive/storage/trash/paginated" => {
                                let files: Vec<serde_json::Value> = items
                                    .iter()
                                    .filter(|_| query.contains("type=files"))
                                    .filter_map(|f| {
                                        let from = f.2.strip_prefix("trash:")?;
                                        let (stem, ext) = f.1.rsplit_once('.').unwrap();
                                        Some(serde_json::json!({
                                            "uuid": f.0, "plainName": stem, "type": ext,
                                            "folderUuid": from,
                                        }))
                                    })
                                    .collect();
                                serde_json::json!({ "result": files })
                            }
                            p if p.ends_with("/folders") => {
                                let parent = p
                                    .trim_start_matches("/drive/folders/v2/content/")
                                    .trim_end_matches("/folders");
                                let listed: Vec<serde_json::Value> = folders
                                    .lock()
                                    .unwrap()
                                    .iter()
                                    .filter(|f| f.2 == parent)
                                    .map(|f| serde_json::json!({ "uuid": f.0, "plainName": f.1 }))
                                    .collect();
                                serde_json::json!({ "folders": listed })
                            }
                            p => {
                                let folder = p
                                    .trim_start_matches("/drive/folders/v2/content/")
                                    .trim_end_matches("/files");
                                let files: Vec<serde_json::Value> = items
                                    .iter()
                                    .filter(|f| f.2 == folder)
                                    .map(|f| {
                                        let (stem, ext) = f.1.rsplit_once('.').unwrap();
                                        serde_json::json!({ "uuid": f.0, "plainName": stem, "type": ext })
                                    })
                                    .collect();
                                serde_json::json!({ "files": files })
                            }
                        };
                        return axum::Json(body).into_response();
                    }
                    if path == "/drive/storage/trash" {
                        let uuid = args["items"][0]["uuid"].as_str().unwrap_or("").to_string();
                        seen.lock().unwrap().push(format!("PURGE {uuid}"));
                        items.retain(|f| f.0 != uuid);
                        return axum::Json(serde_json::json!({})).into_response();
                    }
                    seen.lock().unwrap().push(format!("{method} {path}"));
                    if method == axum::http::Method::DELETE {
                        let uuid = path.rsplit('/').next().unwrap_or("");
                        items.retain(|f| f.0 != uuid);
                        return axum::Json(serde_json::json!({})).into_response();
                    }
                    if path.starts_with("/drive/folders/") {
                        return axum::http::StatusCode::FORBIDDEN.into_response();
                    }
                    if method == axum::http::Method::POST && path == "/drive/files" {
                        let name = format!(
                            "{}.{}",
                            args["plainName"].as_str().unwrap_or(""),
                            args["type"].as_str().unwrap_or("")
                        );
                        let folder = args["folderUuid"].as_str().unwrap_or("").to_string();
                        items.push(("NEW".into(), name, folder));
                        return axum::Json(serde_json::json!({ "uuid": "NEW" })).into_response();
                    }
                    let uuid = path
                        .trim_start_matches("/drive/files/")
                        .trim_end_matches("/meta")
                        .to_string();
                    let at = items.iter().position(|f| f.0 == uuid).unwrap();
                    let (name, folder) = if path.ends_with("/meta") {
                        let name = format!(
                            "{}.{}",
                            args["plainName"].as_str().unwrap_or(""),
                            args["type"].as_str().unwrap_or("")
                        );
                        if name.starts_with("fail") {
                            if name.starts_with("failsquat") {
                                items.push(("SQ".into(), "a.txt".into(), "S".into()));
                            }
                            return axum::http::StatusCode::FORBIDDEN.into_response();
                        }
                        (name, items[at].2.clone())
                    } else {
                        let folder = args["destinationFolder"].as_str().unwrap_or("").to_string();
                        (items[at].1.clone(), folder)
                    };
                    if items.iter().any(|f| f.0 != uuid && f.1 == name && f.2 == folder) {
                        return conflict();
                    }
                    items[at] = (uuid, name, folder);
                    axum::Json(serde_json::json!({})).into_response()
                }
            },
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.ok() });
        let mut provider = test_provider();
        provider.connected = true;
        provider.root_folder_id = "R".to_string();
        provider.api_base = format!("http://{addr}");
        (provider, folders, store, changes)
    }

    /// A move to another folder onto a taken name moved the source there and
    /// only then had its rename refused (409): the source was left moved, the
    /// error saying nothing was done. It is now refused before any change.
    #[tokio::test]
    async fn a_move_onto_a_taken_name_is_refused_before_any_change() {
        let (mut provider, _, changes) =
            provider_on_drive(&[("FA", "a.txt", "S"), ("FB", "b.txt", "D")]).await;
        let outcome = provider.rename("/src/a.txt", "/dst/b.txt").await;
        assert!(
            matches!(outcome, Err(ProviderError::AlreadyExists(_))),
            "{outcome:?}"
        );
        assert!(
            changes.lock().unwrap().is_empty(),
            "{:?}",
            changes.lock().unwrap()
        );
    }

    /// The move keeps the old name: with `/dst/a.txt` there Internxt refused
    /// it (409) and the rename of a file to the free `/dst/c.txt` failed as
    /// AlreadyExists. The rename goes first, in the source folder.
    #[tokio::test]
    async fn a_move_whose_destination_holds_the_old_name_renames_first() {
        let (mut provider, store, changes) =
            provider_on_drive(&[("FA", "a.txt", "S"), ("FA2", "a.txt", "D")]).await;
        provider
            .rename("/src/a.txt", "/dst/c.txt")
            .await
            .expect("rename then move");
        assert_eq!(
            *changes.lock().unwrap(),
            ["PUT /drive/files/FA/meta", "PATCH /drive/files/FA"]
        );
        let moved = store.lock().unwrap()[0].clone();
        assert_eq!((moved.1.as_str(), moved.2.as_str()), ("c.txt", "D"));
    }

    /// A folder rename that failed kept the ids cached under the old path:
    /// only a success forgot them. After a first step that went through and
    /// an undo that did not, `ls`, `mkdir` or `delete` under the old path
    /// then acted on the moved folder. They are forgotten after every
    /// outcome.
    #[tokio::test]
    async fn a_failed_folder_rename_forgets_the_ids_cached_under_it() {
        let (mut provider, _, _) = provider_on_drive(&[]).await;
        for (path, uuid) in [
            ("/src/sub", "SUB"),
            ("/src/sub/deep", "DEEP"),
            ("/src/keep", "K"),
        ] {
            provider.dir_cache_insert(
                path.to_string(),
                DirInfo {
                    uuid: uuid.to_string(),
                },
            );
        }
        let outcome = provider.rename("/src/sub", "/dst/sub2").await;
        assert!(outcome.is_err(), "{outcome:?}");
        assert!(!provider.dir_cache.contains_key("/src/sub"));
        assert!(!provider.dir_cache.contains_key("/src/sub/deep"));
        assert!(provider.dir_cache.contains_key("/src/keep"));
    }

    fn insert_folders(provider: &mut InternxtProvider, folders: &[(&str, &str)]) {
        for (path, uuid) in folders {
            provider.dir_cache_insert(
                path.to_string(),
                DirInfo {
                    uuid: uuid.to_string(),
                },
            );
        }
    }

    /// A folder lookup falls back to another letter case and caches the
    /// spelling it was given, so `/SRC/Sub` can hold the id of `src/sub`. A
    /// rename forgot only its own spelling; an rmdir only its exact key,
    /// not even the folders under it. Every capitalization of the subtree
    /// goes; a sibling sharing the prefix stays.
    #[tokio::test]
    async fn a_rename_or_rmdir_forgets_the_ids_cached_under_any_case() {
        let (mut provider, _, _) = provider_on_drive(&[]).await;
        insert_folders(
            &mut provider,
            &[
                ("/src/sub", "SUB"),
                ("/SRC/Sub/deep", "DEEP"),
                ("/Src/keep", "K"),
            ],
        );
        let outcome = provider.rename("/src/sub", "/dst/sub2").await;
        assert!(outcome.is_err(), "{outcome:?}");
        let mut cached: Vec<&str> = provider.dir_cache.keys().map(String::as_str).collect();
        cached.sort();
        // `/src` and `/dst` were resolved on the way, and stay.
        assert_eq!(cached, ["/Src/keep", "/dst", "/src"]);

        let (mut provider, _, _) = provider_on_drive_with(&[("D", "docs", "R")], &[]).await;
        insert_folders(
            &mut provider,
            &[
                ("/docs", "D"),
                ("/Docs/sub", "SUB"),
                ("/docs/sub/deep", "DEEP"),
                ("/docsx", "X"),
            ],
        );
        provider.rmdir("/docs").await.expect("rmdir");
        let mut cached: Vec<&str> = provider.dir_cache.keys().map(String::as_str).collect();
        cached.sort();
        assert_eq!(cached, ["/docsx"]);
    }

    /// The purge after a delete looked the trash up by name alone: with
    /// `/src/a.txt` and `/dst/a.txt` both trashed, `delete_permanent` of
    /// `/dst/a.txt` purged `/src/a.txt`, listed first. It takes the item
    /// trashed from the path's folder, found by exact names; with no such
    /// folder nothing is purged.
    #[tokio::test]
    async fn a_permanent_delete_takes_the_trashed_item_of_its_own_folder() {
        let (mut provider, _, changes) = provider_on_drive(&[
            ("TS", "a.txt", "trash:S"),
            ("TD", "a.txt", "trash:D"),
            ("TB", "b.txt", "trash:D"),
        ])
        .await;
        assert!(provider
            .delete_permanent("/dst/a.txt")
            .await
            .expect("purge"));
        assert_eq!(*changes.lock().unwrap(), ["PURGE TD"]);
        assert!(!provider
            .delete_permanent("/DST/b.txt")
            .await
            .expect("no folder of that case"));
        assert_eq!(changes.lock().unwrap().len(), 1);
    }

    /// `cd /docs` beside only `Docs` resolved to `Docs` through the case
    /// fallback and cached it as `/docs`. After another client made a real
    /// `docs`, `rm -r /docs` found `docs` exactly and then listed and
    /// deleted by path, which the cache still gave as `Docs`: it emptied
    /// `Docs`. A fallback caches nothing now, not even what a listing under
    /// it finds, and the tree goes by the uuid resolved exactly.
    #[tokio::test]
    async fn rm_r_after_a_fallback_cd_empties_only_the_folder_named() {
        let (mut provider, folders, store, changes) = provider_on_drive_tree(
            &[("X", "Docs", "R"), ("XS", "sub", "X")],
            &[("F1", "keep.txt", "X")],
        )
        .await;
        provider.cd("/docs").await.expect("cd through the fallback");
        provider
            .list("/docs")
            .await
            .expect("ls through the fallback");
        let spelled: Vec<String> = provider
            .dir_cache
            .keys()
            .filter(|k| k.starts_with("/docs"))
            .cloned()
            .collect();
        assert!(
            spelled.is_empty(),
            "cached through the fallback: {spelled:?}"
        );
        folders
            .lock()
            .unwrap()
            .push(("D".into(), "docs".into(), "R".into()));
        store
            .lock()
            .unwrap()
            .push(("F2".into(), "old.txt".into(), "D".into()));
        provider.rmdir_recursive("/docs").await.expect("rm -r");
        assert_eq!(
            *changes.lock().unwrap(),
            ["DELETE /drive/files/F2", "DELETE /drive/folders/D"]
        );
        assert!(store.lock().unwrap().iter().any(|f| f.0 == "F1"));
    }

    /// With only `Docs` there, `rm /docs/keep.txt` and `put /docs/keep.txt`
    /// resolved `/docs` to `Docs` through the fallback: the delete, and the
    /// delete before the upload, deleted `Docs/keep.txt`. A step that
    /// deletes resolves every folder on the path exactly.
    #[tokio::test]
    async fn a_delete_under_a_folder_of_another_case_is_refused() {
        let (mut provider, store, changes) =
            provider_on_drive_with(&[("X", "Docs", "R")], &[("F1", "keep.txt", "X")]).await;
        let outcome = provider.delete("/docs/keep.txt").await;
        assert!(
            matches!(outcome, Err(ProviderError::NotFound(_))),
            "{outcome:?}"
        );
        let empty = tempfile::NamedTempFile::new().unwrap();
        let outcome = provider
            .upload(empty.path().to_str().unwrap(), "/docs/keep.txt", None)
            .await;
        assert!(
            matches!(outcome, Err(ProviderError::NotFound(_))),
            "{outcome:?}"
        );
        assert!(
            changes.lock().unwrap().is_empty(),
            "{:?}",
            changes.lock().unwrap()
        );
        assert_eq!(store.lock().unwrap().len(), 1);
    }

    /// Internxt is taken to keep `Docs` and `docs` as two folders: `rmdir
    /// /docs` beside only `Docs` found `Docs` ignoring the case and removed
    /// it. The folder removed is the one of the exact name.
    #[tokio::test]
    async fn rmdir_does_not_take_a_folder_of_another_case() {
        let (mut provider, _, changes) = provider_on_drive_with(&[("X", "Docs", "R")], &[]).await;
        for outcome in [
            provider.rmdir("/docs").await,
            provider.rmdir_recursive("/docs").await,
        ] {
            assert!(
                matches!(outcome, Err(ProviderError::NotFound(_))),
                "{outcome:?}"
            );
        }
        assert!(
            changes.lock().unwrap().is_empty(),
            "{:?}",
            changes.lock().unwrap()
        );
    }

    /// An upload deletes the file it replaces first, and found it ignoring
    /// the case: an upload of `a.txt` beside only `A.txt` deleted `A.txt`.
    /// `rm /src/A.TXT` deleted it too. Both take the exact name only.
    #[tokio::test]
    async fn an_upload_or_delete_does_not_take_a_file_of_another_case() {
        let (mut provider, store, changes) = provider_on_drive(&[("FA", "A.txt", "S")]).await;
        let empty = tempfile::NamedTempFile::new().unwrap();
        provider
            .upload(empty.path().to_str().unwrap(), "/src/a.txt", None)
            .await
            .expect("upload");
        let outcome = provider.delete("/src/A.TXT").await;
        assert!(
            matches!(outcome, Err(ProviderError::NotFound(_))),
            "{outcome:?}"
        );
        assert_eq!(*changes.lock().unwrap(), ["POST /drive/files"]);
        let names: Vec<String> = store.lock().unwrap().iter().map(|f| f.1.clone()).collect();
        assert_eq!(names, ["A.txt", "a.txt"]);
    }

    /// With `Docs` listed before `docs` and `Report.pdf` before
    /// `report.pdf`, the lookups took the first match ignoring the case:
    /// `/docs` walked into `Docs`, and `report.pdf` was `Report.pdf`. The
    /// name as spelled comes first, on whatever page it is.
    #[tokio::test]
    async fn lookups_take_the_name_as_spelled_before_another_case() {
        let (mut provider, _) = provider_on_paged_drive(vec![
            (
                ("R", "folders"),
                vec![
                    content_page(
                        "folders",
                        serde_json::json!([{ "uuid": "X", "plainName": "Docs" }]),
                        Some("cm9vdA=="),
                    ),
                    content_page(
                        "folders",
                        serde_json::json!([{ "uuid": "D", "plainName": "docs" }]),
                        None,
                    ),
                ],
            ),
            (
                ("D", "files"),
                vec![
                    content_page(
                        "files",
                        serde_json::json!([{ "uuid": "O", "plainName": "Report", "type": "pdf" }]),
                        Some("ZG9jcw+="),
                    ),
                    content_page(
                        "files",
                        serde_json::json!([{ "uuid": "F", "plainName": "report", "type": "pdf" }]),
                        None,
                    ),
                ],
            ),
        ])
        .await;
        assert_eq!(
            provider.resolve_folder_uuid("/docs").await.expect("docs"),
            "D"
        );
        let found = provider
            .find_file_in_folder("D", "report.pdf")
            .await
            .expect("lookup");
        assert_eq!(found.map(|(uuid, _, _)| uuid), Some("F".to_string()));
    }

    /// When the rename after the move failed, the file stayed in the new
    /// folder under its old name while the error said nothing of it. The
    /// move is undone.
    #[tokio::test]
    async fn a_move_whose_rename_fails_is_moved_back() {
        let (mut provider, store, changes) = provider_on_drive(&[("FA", "a.txt", "S")]).await;
        let outcome = provider.rename("/src/a.txt", "/dst/fail.txt").await;
        assert!(outcome.is_err(), "{outcome:?}");
        assert_eq!(
            *changes.lock().unwrap(),
            [
                "PATCH /drive/files/FA",
                "PUT /drive/files/FA/meta",
                "PATCH /drive/files/FA"
            ]
        );
        let back = store.lock().unwrap()[0].clone();
        assert_eq!((back.1.as_str(), back.2.as_str()), ("a.txt", "S"));
    }

    /// The rename after the move failed, and so did the move back: another
    /// file took the old name in `src` meanwhile. The file stays in `dst`
    /// under its old name, and the error names both failures and that path
    /// (never the rename's refusal alone, which would say nothing changed).
    #[tokio::test]
    async fn a_move_whose_rename_and_undo_both_fail_says_where_the_file_is() {
        let (mut provider, store, changes) = provider_on_drive(&[("FA", "a.txt", "S")]).await;
        let outcome = provider.rename("/src/a.txt", "/dst/failsquat.txt").await;
        match &outcome {
            Err(ProviderError::Other(message)) => {
                assert!(message.contains("now at /dst/a.txt"), "{message}")
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            *changes.lock().unwrap(),
            [
                "PATCH /drive/files/FA",
                "PUT /drive/files/FA/meta",
                "PATCH /drive/files/FA"
            ]
        );
        let file = store
            .lock()
            .unwrap()
            .iter()
            .find(|f| f.0 == "FA")
            .cloned()
            .unwrap();
        assert_eq!((file.1.as_str(), file.2.as_str()), ("a.txt", "D"));
    }

    fn test_provider() -> InternxtProvider {
        let config = InternxtConfig {
            email: "alice@example.com".to_string(),
            password: SecretString::from("pw".to_string()),
            two_factor_code: None,
            initial_path: None,
        };
        InternxtProvider::new(config)
    }

    #[test]
    fn normalize_path_resolves_dotdot_and_collapses_duplicates() {
        assert_eq!(InternxtProvider::normalize_path(""), "/");
        assert_eq!(InternxtProvider::normalize_path("."), "/");
        assert_eq!(InternxtProvider::normalize_path("/"), "/");
        assert_eq!(InternxtProvider::normalize_path("/a/b"), "/a/b");
        assert_eq!(InternxtProvider::normalize_path("a/./b"), "/a/b");
        assert_eq!(InternxtProvider::normalize_path("a/b/../c"), "/a/c");
        assert_eq!(InternxtProvider::normalize_path("/a/b/../../.."), "/");
        assert_eq!(InternxtProvider::normalize_path("//a///b//"), "/a/b");
    }

    #[test]
    fn split_path_handles_root_nested_and_bare() {
        assert_eq!(InternxtProvider::split_path("/file.txt"), ("/", "file.txt"));
        assert_eq!(InternxtProvider::split_path("/a/b/c"), ("/a/b", "c"));
        assert_eq!(InternxtProvider::split_path("bare"), ("/", "bare"));
        assert_eq!(InternxtProvider::split_path("/a/"), ("/", "a"));
    }

    #[test]
    fn resolve_path_handles_absolute_relative_and_parent() {
        let mut p = test_provider();
        p.current_path = "/documents/work".to_string();
        assert_eq!(p.resolve_path("/abs"), "/abs");
        assert_eq!(p.resolve_path("child"), "/documents/work/child");
        // ".." moves one level up
        assert_eq!(p.resolve_path(".."), "/documents");
        // from root, ".." stays at root
        p.current_path = "/".to_string();
        assert_eq!(p.resolve_path(".."), "/");
    }

    #[test]
    fn internxt_auth_failure_formats_empty_and_detailed_messages() {
        let empty = internxt_auth_failure("Internxt token expired or invalid", "");
        assert!(matches!(
            empty,
            ProviderError::AuthenticationFailed(ref msg)
                if msg == "Internxt token expired or invalid"
        ));

        let detailed = internxt_auth_failure("Internxt token refresh failed", "401: expired");
        assert!(matches!(
            detailed,
            ProviderError::AuthenticationFailed(ref msg)
                if msg == "Internxt token refresh failed: 401: expired"
        ));
    }

    #[test]
    fn extract_size_reads_number_string_and_defaults_to_zero() {
        use serde_json::json;
        assert_eq!(InternxtProvider::extract_size(&Some(json!(4096))), 4096);
        assert_eq!(InternxtProvider::extract_size(&Some(json!("8192"))), 8192);
        // Non-numeric string, wrong type, and absence all fall back to 0.
        assert_eq!(
            InternxtProvider::extract_size(&Some(json!("not a number"))),
            0
        );
        assert_eq!(InternxtProvider::extract_size(&Some(json!(true))), 0);
        assert_eq!(InternxtProvider::extract_size(&None), 0);
    }

    #[test]
    fn get_filename_prefers_plain_name_then_name_then_unnamed_and_appends_type() {
        let mk = |v: serde_json::Value| serde_json::from_value::<InternxtFile>(v).unwrap();

        // plainName + type -> "plainName.type"
        let f = mk(serde_json::json!({"uuid":"u","plainName":"report","type":"pdf"}));
        assert_eq!(InternxtProvider::get_filename(&f), "report.pdf");
        // plainName missing -> falls back to name
        let f = mk(serde_json::json!({"uuid":"u","name":"legacy","type":"txt"}));
        assert_eq!(InternxtProvider::get_filename(&f), "legacy.txt");
        // both missing -> "unnamed"
        let f = mk(serde_json::json!({"uuid":"u"}));
        assert_eq!(InternxtProvider::get_filename(&f), "unnamed");
        // empty type -> no trailing dot
        let f = mk(serde_json::json!({"uuid":"u","plainName":"folderlike","type":""}));
        assert_eq!(InternxtProvider::get_filename(&f), "folderlike");
    }

    #[test]
    fn split_name_ext_splits_on_last_dot_only() {
        assert_eq!(
            InternxtProvider::split_name_ext("a.txt"),
            ("a".to_string(), "txt".to_string())
        );
        assert_eq!(
            InternxtProvider::split_name_ext("archive.tar.gz"),
            ("archive.tar".to_string(), "gz".to_string())
        );
        // No extension.
        assert_eq!(
            InternxtProvider::split_name_ext("noext"),
            ("noext".to_string(), String::new())
        );
        // Leading dot (pos 0) is treated as a dotfile, not an extension.
        assert_eq!(
            InternxtProvider::split_name_ext(".hidden"),
            (".hidden".to_string(), String::new())
        );
    }

    /// One page of a folder content listing, as the drive answers it.
    fn content_page(kind: &str, items: serde_json::Value, next: Option<&str>) -> serde_json::Value {
        serde_json::json!({ kind: items, "nextCursor": next })
    }

    /// A provider connected to a local drive that serves the folder content
    /// listings of `pages`, keyed by (folder uuid, `folders` or `files`), one
    /// body per page. A request without a cursor gets the first page, one
    /// with cursor `c` the page after the one whose `nextCursor` was `c`, and
    /// an unknown cursor a 400, as Internxt does. The route version is not
    /// checked, so a client on the offset routes reads the first page again
    /// and again. Every request URI is recorded.
    async fn provider_on_paged_drive(
        pages: Vec<((&str, &str), Vec<serde_json::Value>)>,
    ) -> (
        InternxtProvider,
        std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    ) {
        use axum::response::IntoResponse;
        use std::sync::{Arc, Mutex};
        let pages: Arc<HashMap<(String, String), Vec<serde_json::Value>>> = Arc::new(
            pages
                .into_iter()
                .map(|((uuid, kind), bodies)| ((uuid.to_string(), kind.to_string()), bodies))
                .collect(),
        );
        let requests: Arc<Mutex<Vec<String>>> = Arc::default();
        let seen = Arc::clone(&requests);
        let app =
            axum::Router::new().fallback(axum::routing::any(move |req: axum::extract::Request| {
                let (pages, seen) = (Arc::clone(&pages), Arc::clone(&seen));
                async move {
                    let uri = req.uri().clone();
                    seen.lock().unwrap().push(uri.to_string());
                    let not_found = axum::http::StatusCode::NOT_FOUND.into_response();
                    let Some(rest) = uri.path().strip_prefix("/drive/folders/") else {
                        return not_found;
                    };
                    let rest = rest.strip_prefix("v2/").unwrap_or(rest);
                    let Some((uuid, kind)) = rest
                        .strip_prefix("content/")
                        .and_then(|r| r.split_once('/'))
                    else {
                        return not_found;
                    };
                    let Some(bodies) = pages.get(&(uuid.to_string(), kind.to_string())) else {
                        return not_found;
                    };
                    let cursor = url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
                        .find(|(k, _)| k == "cursor")
                        .map(|(_, v)| v.into_owned());
                    let at = match cursor {
                        None => Some(0),
                        Some(c) => bodies
                            .iter()
                            .position(|b| b["nextCursor"].as_str() == Some(c.as_str()))
                            .map(|i| i + 1),
                    };
                    match at.and_then(|i| bodies.get(i)) {
                        Some(body) => axum::Json(body.clone()).into_response(),
                        None => {
                            (axum::http::StatusCode::BAD_REQUEST, "Invalid cursor").into_response()
                        }
                    }
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.ok() });
        let mut provider = test_provider();
        provider.connected = true;
        provider.root_folder_id = "R".to_string();
        provider.current_folder_id = "R".to_string();
        provider.api_base = format!("http://{addr}");
        (provider, requests)
    }

    /// The offset routes are deprecated; the cursor routes hand out a
    /// `nextCursor` until the last page. A listing longer than one page lost
    /// every page after the first. The cursors are base64 (`+`, `/`, `=`), so
    /// they must reach the server encoded.
    #[tokio::test]
    async fn a_listing_reads_every_cursor_page() {
        let folder =
            |uuid: &str, name: &str| serde_json::json!({ "uuid": uuid, "plainName": name });
        let file = |uuid: &str, name: &str, ext: &str| serde_json::json!({ "uuid": uuid, "plainName": name, "type": ext, "size": "3" });
        let (mut provider, requests) = provider_on_paged_drive(vec![
            (
                ("R", "folders"),
                vec![
                    content_page(
                        "folders",
                        serde_json::json!([folder("A", "a"), folder("B", "b")]),
                        Some("Zm9s+/A="),
                    ),
                    content_page(
                        "folders",
                        serde_json::json!([folder("C", "c"), folder("D", "d")]),
                        Some("Zm9s+/B=="),
                    ),
                    content_page("folders", serde_json::json!([folder("E", "e")]), None),
                ],
            ),
            (
                ("R", "files"),
                vec![
                    content_page(
                        "files",
                        serde_json::json!([file("X", "x", "txt"), file("Y", "y", "txt")]),
                        Some("ZmlsZQ+/="),
                    ),
                    content_page("files", serde_json::json!([file("Z", "z", "bin")]), None),
                ],
            ),
        ])
        .await;

        let entries = provider.list("/").await.expect("list");
        let mut names: Vec<String> = entries.iter().map(|e| e.name.clone()).collect();
        names.sort();
        assert_eq!(names, ["a", "b", "c", "d", "e", "x.txt", "y.txt", "z.bin"]);
        assert_eq!(entries.iter().filter(|e| e.is_dir).count(), 5);

        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 5, "{requests:?}");
        for uri in requests.iter() {
            assert!(uri.starts_with("/drive/folders/v2/content/R/"), "{uri}");
            assert!(!uri.contains("offset=") && !uri.contains("sort="), "{uri}");
            assert!(uri.contains("limit=") && uri.contains("order=ASC"), "{uri}");
        }
    }

    /// A server that hands back a cursor it already gave would make the
    /// listing loop forever or, cut short, look complete: it is an error.
    #[tokio::test]
    async fn a_repeated_cursor_fails_the_listing() {
        let (mut provider, _) = provider_on_paged_drive(vec![
            (
                ("R", "folders"),
                vec![
                    content_page(
                        "folders",
                        serde_json::json!([{ "uuid": "A", "plainName": "a" }]),
                        Some("same"),
                    ),
                    content_page(
                        "folders",
                        serde_json::json!([{ "uuid": "B", "plainName": "b" }]),
                        Some("same"),
                    ),
                ],
            ),
            (
                ("R", "files"),
                vec![content_page("files", serde_json::json!([]), None)],
            ),
        ])
        .await;

        match provider.list("/").await {
            Err(ProviderError::ServerError(message)) => {
                assert!(message.contains("cursor"), "{message}")
            }
            other => panic!("{other:?}"),
        }
    }

    /// A page without its item array is not an empty folder.
    #[tokio::test]
    async fn a_page_without_its_items_fails_the_listing() {
        let (mut provider, _) = provider_on_paged_drive(vec![
            (
                ("R", "folders"),
                vec![serde_json::json!({ "nextCursor": null })],
            ),
            (
                ("R", "files"),
                vec![content_page("files", serde_json::json!([]), None)],
            ),
        ])
        .await;

        match provider.list("/").await {
            Err(ProviderError::ServerError(message)) => {
                assert!(message.contains("folders"), "{message}")
            }
            other => panic!("{other:?}"),
        }
    }

    /// Path lookups page too: a folder or a file past the first page was
    /// reported missing.
    #[tokio::test]
    async fn path_lookups_follow_the_cursor_past_the_first_page() {
        let (mut provider, _) = provider_on_paged_drive(vec![
            (
                ("R", "folders"),
                vec![
                    content_page("folders", serde_json::json!([{ "uuid": "A", "plainName": "archive" }]), Some("cm9vdA==")),
                    content_page("folders", serde_json::json!([{ "uuid": "D", "plainName": "docs" }]), None),
                ],
            ),
            (
                ("D", "files"),
                vec![
                    content_page("files", serde_json::json!([{ "uuid": "O", "plainName": "other", "type": "txt" }]), Some("ZG9jcw+=")),
                    content_page("files", serde_json::json!([{ "uuid": "F", "plainName": "report", "type": "pdf", "fileId": "net-F" }]), None),
                ],
            ),
        ])
        .await;

        assert_eq!(
            provider.resolve_folder_uuid("/docs").await.expect("docs"),
            "D"
        );
        let found = provider
            .find_file_in_folder("D", "report.pdf")
            .await
            .expect("lookup");
        assert_eq!(
            found.map(|(uuid, file_id, _)| (uuid, file_id)),
            Some(("F".to_string(), "net-F".to_string()))
        );
    }

    /// One answer of the local login server: status, body and the
    /// Retry-After header it sends, if any.
    type LoginAnswer = (u16, String, Option<&'static str>);

    /// Step 1's answer on a working account: an sKey and no 2FA.
    fn login_hands_out_an_s_key() -> LoginAnswer {
        let s_key = InternxtProvider::encrypt_text("00112233445566778899aabbccddeeff").unwrap();
        (
            200,
            serde_json::json!({ "hasKeys": true, "sKey": s_key, "tfa": false }).to_string(),
            None,
        )
    }

    /// A provider pointed at a local login server. Step 1 hands out an sKey,
    /// the CLI access endpoint answers `cli` and the web login fallback
    /// answers `web` (status and body). Every request is recorded as
    /// "METHOD path".
    async fn provider_on_login_server(
        cli: (u16, &str),
        web: (u16, &str),
    ) -> (
        InternxtProvider,
        std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    ) {
        provider_on_login_steps(
            login_hands_out_an_s_key(),
            (cli.0, cli.1.to_string(), None),
            (web.0, web.1.to_string(), None),
        )
        .await
    }

    /// A provider pointed at a local login server that answers step 1
    /// (`/drive/auth/login`), the CLI access endpoint and the web login
    /// fallback with `login`, `cli` and `web`.
    async fn provider_on_login_steps(
        login: LoginAnswer,
        cli: LoginAnswer,
        web: LoginAnswer,
    ) -> (
        InternxtProvider,
        std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    ) {
        use axum::response::IntoResponse;
        use std::sync::{Arc, Mutex};
        let answers: Arc<HashMap<&'static str, LoginAnswer>> = Arc::new(HashMap::from([
            ("/drive/auth/login", login),
            ("/drive/auth/cli/login/access", cli),
            ("/drive/auth/login/access", web),
        ]));
        let requests: Arc<Mutex<Vec<String>>> = Arc::default();
        let seen = Arc::clone(&requests);
        let app =
            axum::Router::new().fallback(axum::routing::any(move |req: axum::extract::Request| {
                let (answers, seen) = (Arc::clone(&answers), Arc::clone(&seen));
                async move {
                    let path = req.uri().path().to_string();
                    seen.lock()
                        .unwrap()
                        .push(format!("{} {}", req.method(), path));
                    match answers.get(path.as_str()) {
                        Some((status, body, retry_after)) => {
                            let mut response = (
                                axum::http::StatusCode::from_u16(*status).unwrap(),
                                [(axum::http::header::CONTENT_TYPE, "application/json")],
                                body.clone(),
                            )
                                .into_response();
                            if let Some(retry_after) = retry_after {
                                response.headers_mut().insert(
                                    axum::http::header::RETRY_AFTER,
                                    axum::http::HeaderValue::from_static(retry_after),
                                );
                            }
                            response
                        }
                        None => axum::http::StatusCode::NOT_FOUND.into_response(),
                    }
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.ok() });
        let mut provider = test_provider();
        provider.api_base = format!("http://{addr}");
        (provider, requests)
    }

    const TIER_402: (u16, &str) = (
        402,
        r#"{"message":"rclone access not allowed for this user tier"}"#,
    );

    /// The CLI access endpoint answers 402 to plans without CLI/Rclone access
    /// (after checking the credentials); the web login on the same gateway
    /// then logs in. It used to go to api.internxt.com, which hangs to a 504.
    #[tokio::test]
    async fn a_plan_without_cli_access_logs_in_through_the_gateway_web_login() {
        let mnemonic = ["abandon"; 11].join(" ") + " about";
        let access = serde_json::json!({
            "token": "t",
            "newToken": "nt",
            "user": {
                "email": "alice@example.com",
                "userId": "u1",
                "mnemonic": InternxtProvider::encrypt_text_with_key(&mnemonic, "pw").unwrap(),
                "rootFolderId": "R",
                "bucket": "b1",
                "bridgeUser": "alice@example.com",
                "uuid": "user-uuid",
            },
        })
        .to_string();
        let (mut provider, requests) = provider_on_login_server(TIER_402, (200, &access)).await;

        provider.connect().await.expect("connect");
        assert!(provider.is_connected());
        assert_eq!(provider.root_folder_id, "R");
        assert_eq!(provider.mnemonic.expose_secret(), mnemonic);
        assert_eq!(
            *requests.lock().unwrap(),
            [
                "POST /drive/auth/login",
                "POST /drive/auth/cli/login/access",
                "POST /drive/auth/login/access",
            ]
        );
    }

    /// The 402 comes after the credentials were accepted, so a fallback that
    /// then fails (the old host timed out to a 504) is not a credentials
    /// problem: the error says what failed, not "check credentials".
    #[tokio::test]
    async fn a_failed_fallback_after_the_plan_refusal_does_not_blame_the_credentials() {
        let (mut provider, _) =
            provider_on_login_server(TIER_402, (504, "<html>504 Gateway Time-out</html>")).await;

        match provider.connect().await {
            Err(ProviderError::ServerError(message)) => {
                assert!(
                    message.contains("does not include CLI/WebDAV/Rclone access"),
                    "{message}"
                );
                assert!(message.contains("504"), "{message}");
            }
            other => panic!("{other:?}"),
        }
        assert!(!provider.is_connected());
    }

    /// A web login the server refuses for the account or the plan (402, 403)
    /// is a permission problem, named as such.
    #[tokio::test]
    async fn a_fallback_refused_for_the_plan_or_account_is_permission_denied() {
        for web in [
            (
                402,
                r#"{"message":"access not allowed for this user tier"}"#,
            ),
            (
                403,
                r#"{"message":"Your account has been blocked for security reasons. Please reach out to us","error":"ACCOUNT_BLOCKED"}"#,
            ),
        ] {
            let (mut provider, _) = provider_on_login_server(TIER_402, web).await;
            match provider.connect().await {
                Err(ProviderError::PermissionDenied(message)) => {
                    assert!(
                        message.contains("does not include CLI/WebDAV/Rclone access"),
                        "{message}"
                    );
                    assert!(message.contains(&web.0.to_string()), "{message}");
                }
                other => panic!("{web:?}: {other:?}"),
            }
        }
    }

    /// Only the server's own 401 blames the credentials.
    #[tokio::test]
    async fn only_a_401_blames_the_credentials() {
        let (mut provider, _) =
            provider_on_login_server(TIER_402, (401, r#"{"message":"Wrong login credentials"}"#))
                .await;
        match provider.connect().await {
            Err(ProviderError::AuthenticationFailed(message)) => {
                assert!(message.contains("Wrong login credentials"), "{message}")
            }
            other => panic!("{other:?}"),
        }

        let (mut provider, requests) = provider_on_login_server(
            (401, r#"{"message":"Wrong login credentials"}"#),
            (200, "{}"),
        )
        .await;
        match provider.connect().await {
            Err(ProviderError::AuthenticationFailed(message)) => {
                assert!(message.contains("Wrong login credentials"), "{message}")
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(requests.lock().unwrap().len(), 2, "no fallback after a 401");
    }

    /// A CLI access endpoint that fails for another reason (a 5xx) is a
    /// server error, not an authentication failure with a plan note.
    #[tokio::test]
    async fn a_cli_endpoint_server_error_is_not_an_authentication_failure() {
        let (mut provider, requests) =
            provider_on_login_server((500, r#"{"message":"Internal server error"}"#), (200, "{}"))
                .await;
        match provider.connect().await {
            Err(ProviderError::ServerError(message)) => {
                assert!(message.contains("500"), "{message}")
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(requests.lock().unwrap().len(), 2, "no fallback after a 500");
    }

    const RATE_LIMITED: &str =
        r#"{"statusCode":429,"message":"ThrottlerException: Too Many Requests"}"#;

    /// After many logins in a row the gateway answers 429 on step 1 itself.
    /// That is Internxt limiting logins, not a wrong password: the error says
    /// so, asks to retry later and keeps the wait the server asked for.
    #[tokio::test]
    async fn a_rate_limited_login_says_so_and_keeps_retry_after() {
        let (mut provider, requests) = provider_on_login_steps(
            (429, RATE_LIMITED.to_string(), Some("30")),
            (200, "{}".to_string(), None),
            (200, "{}".to_string(), None),
        )
        .await;
        match provider.connect().await {
            Err(ProviderError::ServerError(message)) => {
                assert!(message.contains("Internxt is limiting logins"), "{message}");
                assert!(message.contains("429"), "{message}");
                assert!(message.contains("retry in 30 seconds"), "{message}");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            *requests.lock().unwrap(),
            ["POST /drive/auth/login"],
            "no further login step after a 429"
        );

        let (mut provider, _) = provider_on_login_steps(
            (429, RATE_LIMITED.to_string(), None),
            (200, "{}".to_string(), None),
            (200, "{}".to_string(), None),
        )
        .await;
        match provider.connect().await {
            Err(ProviderError::ServerError(message)) => {
                assert!(message.contains("Internxt is limiting logins"), "{message}");
                assert!(message.contains("retry later"), "{message}");
            }
            other => panic!("{other:?}"),
        }
    }

    /// The CLI access endpoint and the web login fallback read a 429 the same
    /// way as step 1.
    #[tokio::test]
    async fn every_login_step_reads_a_429_as_rate_limiting() {
        let rate_limited = || (429, RATE_LIMITED.to_string(), Some("12"));
        for (cli, web) in [
            (rate_limited(), (200, "{}".to_string(), None)),
            ((TIER_402.0, TIER_402.1.to_string(), None), rate_limited()),
        ] {
            let (mut provider, _) =
                provider_on_login_steps(login_hands_out_an_s_key(), cli, web).await;
            match provider.connect().await {
                Err(ProviderError::ServerError(message)) => {
                    assert!(message.contains("Internxt is limiting logins"), "{message}");
                    assert!(message.contains("retry in 12 seconds"), "{message}");
                }
                other => panic!("{other:?}"),
            }
        }
    }

    /// Step 1 classifies its other refusals like the later steps: only a 401
    /// blames the credentials, a 403 is the account refused, a 5xx or an
    /// unreadable answer is the server failing.
    #[tokio::test]
    async fn step_one_classifies_its_refusals_like_the_later_steps() {
        let unused = || (200, "{}".to_string(), None);
        type Expected = fn(&ProviderError) -> bool;
        let cases: [(LoginAnswer, Expected); 4] = [
            (
                (401, r#"{"message":"Wrong login credentials"}"#.to_string(), None),
                |e| matches!(e, ProviderError::AuthenticationFailed(m) if m.contains("Wrong login credentials")),
            ),
            (
                (403, r#"{"message":"Your account has been blocked for security reasons. Please reach out to us","error":"ACCOUNT_BLOCKED"}"#.to_string(), None),
                |e| matches!(e, ProviderError::PermissionDenied(m) if m.contains("403")),
            ),
            (
                (503, "<html>503 Service Temporarily Unavailable</html>".to_string(), None),
                |e| matches!(e, ProviderError::ServerError(m) if m.contains("503")),
            ),
            (
                (200, "<html>not json</html>".to_string(), None),
                |e| matches!(e, ProviderError::ServerError(_)),
            ),
        ];
        let mut wrong = Vec::new();
        for (login, expected) in cases {
            let label = format!("{} {}", login.0, login.1);
            let (mut provider, _) = provider_on_login_steps(login, unused(), unused()).await;
            match provider.connect().await {
                Err(e) if expected(&e) => {}
                other => wrong.push(format!("{label}: {other:?}")),
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// A login server nobody answers on is a connection failure at step 1.
    #[tokio::test]
    async fn an_unreachable_login_server_is_a_connection_failure() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let mut provider = test_provider();
        provider.api_base = format!("http://{addr}");
        match provider.connect().await {
            Err(ProviderError::ConnectionFailed(_)) => {}
            other => panic!("{other:?}"),
        }
    }
}
