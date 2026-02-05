//! Filen.io Storage Provider
//!
//! Implements StorageProvider for Filen using their REST API.
//! Uses client-side AES-256-GCM encryption (zero-knowledge).
//! All file names, metadata, and content are encrypted locally.

use async_trait::async_trait;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use aes_gcm::aead::Aead;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use reqwest::header::{HeaderValue, CONTENT_TYPE};
use secrecy::ExposeSecret;
use serde::Deserialize;
use sha1::Sha1;
use sha2::{Sha512, Digest};
use std::collections::HashMap;
use tracing::debug;

/// Debug logging through tracing infrastructure (no file I/O)
fn filen_log(msg: &str) {
    debug!(target: "filen", "{}", msg);
}

use super::{
    StorageProvider, ProviderType, ProviderError, RemoteEntry, StorageInfo,
};
use super::types::FilenConfig;

/// Filen API gateway
const GATEWAY: &str = "https://gateway.filen.io";

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
    name: String,  // encrypted
    parent: String,
    timestamp: u64,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct FilenFile {
    uuid: String,
    metadata: String,  // encrypted JSON: {name, size, mime, key, lastModified}
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
    api_key: String,
    master_keys: Vec<String>,
    current_path: String,
    current_folder_uuid: String,
    root_uuid: String,
    /// Cache: path -> DirInfo
    dir_cache: HashMap<String, DirInfo>,
}

impl FilenProvider {
    pub fn new(config: FilenConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
            connected: false,
            api_key: String::new(),
            master_keys: Vec::new(),
            current_path: "/".to_string(),
            current_folder_uuid: String::new(),
            root_uuid: String::new(),
            dir_cache: HashMap::new(),
        }
    }

    /// Derive password hash and master key for authentication (PBKDF2-SHA512)
    /// Returns (login_password, master_key)
    fn derive_auth_credentials(password: &str, salt: &str, auth_version: u32) -> Result<(String, String), ProviderError> {
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
            if let Some(decrypted) = Self::try_decrypt_aes_gcm(encrypted, key) {
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
    fn try_decrypt_aes_gcm(encrypted: &str, key: &str) -> Option<String> {
        if encrypted.len() < 16 {
            filen_log(&format!("try_decrypt: too short ({})", encrypted.len()));
            return None;
        }

        let version = &encrypted[..3];
        filen_log(&format!("try_decrypt: version={}, len={}, first50={}", version, encrypted.len(), &encrypted[..encrypted.len().min(50)]));

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
                    filen_log(&format!("try_decrypt: 001 format but {} parts", parts.len()));
                    return None;
                }
                let iv_bytes = BASE64.decode(parts[1]).ok()?;
                let ct = BASE64.decode(parts[2]).ok()?;
                (iv_bytes, ct)
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
        let key = self.master_keys.first()
            .ok_or_else(|| ProviderError::Other("No master key".to_string()))?;

        let aes_key = Self::derive_aes_key(key);
        let cipher = Aes256Gcm::new_from_slice(&aes_key)
            .map_err(|e| ProviderError::Other(format!("Cipher error: {}", e)))?;

        // Generate 12-char random ASCII string for IV (matching Filen SDK)
        let iv_chars: String = (0..12).map(|_| {
            let idx = rand::random::<u8>() % 62;
            match idx {
                0..=25 => (b'a' + idx) as char,
                26..=51 => (b'A' + idx - 26) as char,
                _ => (b'0' + idx - 52) as char,
            }
        }).collect();
        let nonce_bytes = iv_chars.as_bytes();
        let nonce = Nonce::from_slice(nonce_bytes);

        let ciphertext = cipher.encrypt(nonce, data.as_bytes())
            .map_err(|e| ProviderError::Other(format!("Encrypt error: {}", e)))?;

        Ok(format!("002{}{}", iv_chars, BASE64.encode(ciphertext)))
    }

    /// Encrypt metadata with a specific key (static version for per-key encryption)
    fn encrypt_metadata_with_key(data: &str, key: &str) -> Result<String, ProviderError> {
        let aes_key = Self::derive_aes_key(key);
        let cipher = Aes256Gcm::new_from_slice(&aes_key)
            .map_err(|e| ProviderError::Other(format!("Cipher error: {}", e)))?;

        let iv_chars: String = (0..12).map(|_| {
            let idx = rand::random::<u8>() % 62;
            match idx {
                0..=25 => (b'a' + idx) as char,
                26..=51 => (b'A' + idx - 26) as char,
                _ => (b'0' + idx - 52) as char,
            }
        }).collect();
        let nonce = Nonce::from_slice(iv_chars.as_bytes());

        let ciphertext = cipher.encrypt(nonce, data.as_bytes())
            .map_err(|e| ProviderError::Other(format!("Encrypt error: {}", e)))?;

        Ok(format!("002{}{}", iv_chars, BASE64.encode(ciphertext)))
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

        let ciphertext = cipher.encrypt(nonce, data)
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
        cipher.decrypt(nonce, ciphertext)
            .map_err(|e| ProviderError::Other(format!("Decrypt error: {}", e)))
    }

    fn normalize_path(path: &str) -> String {
        let p = if path.starts_with('/') { path.to_string() } else { format!("/{}", path) };
        if p.len() > 1 { p.trim_end_matches('/').to_string() } else { p }
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
            let resp = self.client.post(&format!("{}/v3/dir/content", GATEWAY))
                .header("Authorization", HeaderValue::from_str(&format!("Bearer {}", self.api_key)).unwrap())
                .json(&serde_json::json!({"uuid": current_uuid}))
                .send().await
                .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

            let content: DirContentResponse = resp.json().await
                .map_err(|e| ProviderError::ParseError(e.to_string()))?;

            if !content.status {
                return Err(ProviderError::NotFound(format!("Path not found: {}", path)));
            }

            let data = content.data.unwrap_or(DirContentData { folders: vec![], uploads: vec![] });

            let mut found = false;
            for folder in &data.folders {
                if let Some(name) = self.decrypt_folder_name(&folder.name) {
                    if name == part {
                        current_uuid = folder.uuid.clone();
                        self.dir_cache.insert(built_path.clone(), DirInfo {
                            uuid: folder.uuid.clone(),
                            name: name.clone(),
                        });
                        found = true;
                        break;
                    }
                }
            }

            if !found {
                return Err(ProviderError::NotFound(format!("Folder not found: {}", part)));
            }
        }

        Ok(current_uuid)
    }
}

#[async_trait]
impl StorageProvider for FilenProvider {
    fn provider_type(&self) -> ProviderType { ProviderType::Filen }

    fn display_name(&self) -> String { format!("Filen ({})", self.config.email) }

    fn is_connected(&self) -> bool { self.connected }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        let password = self.config.password.expose_secret();

        // Step 1: Get auth info
        let auth_info_resp: AuthInfoResponse = self.client
            .post(&format!("{}/v3/auth/info", GATEWAY))
            .json(&serde_json::json!({"email": self.config.email}))
            .send().await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?
            .json().await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        if !auth_info_resp.status {
            return Err(ProviderError::AuthenticationFailed(
                auth_info_resp.message.unwrap_or_else(|| "Auth info failed".to_string())
            ));
        }

        let auth_data = auth_info_resp.data
            .ok_or_else(|| ProviderError::AuthenticationFailed("No auth info data".to_string()))?;

        // Step 2: Derive password hash and master key
        let (auth_hash, derived_master_key) = Self::derive_auth_credentials(password, &auth_data.salt, auth_data.auth_version)?;

        // Step 3: Login
        let login_resp: LoginResponse = self.client
            .post(&format!("{}/v3/login", GATEWAY))
            .json(&serde_json::json!({
                "email": self.config.email,
                "password": auth_hash,
                "authVersion": auth_data.auth_version,
                "twoFactorCode": "XXXXXX",
            }))
            .send().await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?
            .json().await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        if !login_resp.status {
            return Err(ProviderError::AuthenticationFailed(
                login_resp.message.unwrap_or_else(|| "Login failed".to_string())
            ));
        }

        let login_data = login_resp.data
            .ok_or_else(|| ProviderError::AuthenticationFailed("No login data".to_string()))?;

        self.api_key = login_data.api_key;

        // Step 4: Use derived master key and decrypt additional master keys
        self.master_keys = vec![derived_master_key.clone()];

        // Try to decrypt the encrypted master keys from the response
        filen_log(&format!("master_keys field len={}, first80={}", login_data.master_keys.len(), &login_data.master_keys[..login_data.master_keys.len().min(80)]));
        filen_log(&format!("derived_master_key len={}, first20={}", derived_master_key.len(), &derived_master_key[..derived_master_key.len().min(20)]));
        if let Some(decrypted) = Self::try_decrypt_aes_gcm(&login_data.master_keys, &derived_master_key) {
            self.master_keys = decrypted.split('|')
                .map(|s| s.to_string())
                .collect();
            if !self.master_keys.contains(&derived_master_key) {
                self.master_keys.push(derived_master_key);
            }
        }

        // Step 5: Get root folder UUID from user info
        let user_resp: serde_json::Value = self.client
            .get(&format!("{}/v3/user/baseFolder", GATEWAY))
            .header("Authorization", HeaderValue::from_str(&format!("Bearer {}", self.api_key)).unwrap())
            .send().await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?
            .json().await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        self.root_uuid = user_resp["data"]["uuid"].as_str()
            .unwrap_or("").to_string();

        if self.root_uuid.is_empty() {
            return Err(ProviderError::ConnectionFailed("Failed to get root folder UUID".to_string()));
        }

        self.current_folder_uuid = self.root_uuid.clone();
        self.current_path = "/".to_string();
        self.dir_cache.insert("/".to_string(), DirInfo {
            uuid: self.root_uuid.clone(),
            name: "/".to_string(),
        });

        self.connected = true;
        filen_log(&format!("Connected as {}, root_uuid={}, master_keys={}",
            self.config.email, self.root_uuid, self.master_keys.len()));
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.connected = false;
        self.api_key.clear();
        self.master_keys.clear();
        self.dir_cache.clear();
        Ok(())
    }

    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let folder_uuid = if path == "." || path.is_empty() {
            self.current_folder_uuid.clone()
        } else {
            self.resolve_folder_uuid(path).await?
        };

        let resp = self.client.post(&format!("{}/v3/dir/content", GATEWAY))
            .header("Authorization", HeaderValue::from_str(&format!("Bearer {}", self.api_key)).unwrap())
            .json(&serde_json::json!({"uuid": folder_uuid}))
            .send().await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        let resp_text = resp.text().await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        // Log raw response for debugging (truncated)
        let preview = if resp_text.len() > 500 { &resp_text[..500] } else { &resp_text };
        filen_log(&format!("dir/content uuid={} response: {}", folder_uuid, preview));

        let content: DirContentResponse = serde_json::from_str(&resp_text)
            .map_err(|e| ProviderError::ParseError(format!("JSON parse error: {} - response: {}", e, preview)))?;

        if !content.status {
            return Err(ProviderError::ServerError(
                content.message.unwrap_or_else(|| "List failed".to_string())
            ));
        }

        let data = content.data.unwrap_or(DirContentData { folders: vec![], uploads: vec![] });
        filen_log(&format!("list '{}' uuid={}: {} folders, {} files", path, folder_uuid, data.folders.len(), data.uploads.len()));
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

                self.dir_cache.insert(entry_path.clone(), DirInfo {
                    uuid: folder.uuid.clone(),
                    name: name.clone(),
                });

                entries.push(RemoteEntry {
                    name,
                    path: entry_path,
                    is_dir: true,
                    size: 0,
                    modified: Some(
                        chrono::DateTime::from_timestamp(folder.timestamp as i64, 0)
                            .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
                            .unwrap_or_default()
                    ),
                    permissions: None, owner: None, group: None,
                    is_symlink: false, link_target: None, mime_type: None,
                    metadata: {
                        let mut m = HashMap::new();
                        m.insert("uuid".to_string(), folder.uuid);
                        m
                    },
                });
            } else {
                filen_log(&format!("FAILED decrypt folder: uuid={}, len={}, start={}",
                    folder.uuid, folder.name.len(), &folder.name[..folder.name.len().min(50)]));
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
                        permissions: None, owner: None, group: None,
                        is_symlink: false, link_target: None,
                        mime_type: if meta.mime.is_empty() { None } else { Some(meta.mime) },
                        metadata: {
                            let mut m = HashMap::new();
                            m.insert("uuid".to_string(), file.uuid);
                            m.insert("key".to_string(), meta.key);
                            m.insert("bucket".to_string(), file.bucket);
                            m.insert("region".to_string(), file.region);
                            m.insert("chunks".to_string(), file.chunks.to_string());
                            m
                        },
                    });
                }
            } else {
                filen_log(&format!("FAILED decrypt file: uuid={}, len={}, start={}",
                    file.uuid, file.metadata.len(), &file.metadata[..file.metadata.len().min(50)]));
            }
        }

        Ok(entries)
    }

    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        let new_path = if path.starts_with('/') {
            Self::normalize_path(path)
        } else {
            let base = if self.current_path == "/" { String::new() } else { self.current_path.clone() };
            Self::normalize_path(&format!("{}/{}", base, path))
        };

        let folder_uuid = self.resolve_folder_uuid(&new_path).await?;
        self.current_path = new_path;
        self.current_folder_uuid = folder_uuid;
        Ok(())
    }

    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        if self.current_path == "/" { return Ok(()); }
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

    async fn download(&mut self, remote_path: &str, local_path: &str, _progress: Option<Box<dyn Fn(u64, u64) + Send>>) -> Result<(), ProviderError> {
        // Find the file to get its metadata (uuid, key, region, bucket, chunks)
        let normalized = Self::normalize_path(remote_path);
        let (parent_path, file_name) = match normalized.rfind('/') {
            Some(pos) if pos > 0 => (&normalized[..pos], &normalized[pos + 1..]),
            _ => ("/", normalized.trim_start_matches('/')),
        };

        let entries = self.list(parent_path).await?;
        let file_entry = entries.iter()
            .find(|e| !e.is_dir && e.name == file_name)
            .ok_or_else(|| ProviderError::NotFound(format!("File not found: {}", file_name)))?;

        let uuid = file_entry.metadata.get("uuid")
            .ok_or_else(|| ProviderError::Other("No UUID for file".to_string()))?;
        let file_key = file_entry.metadata.get("key")
            .ok_or_else(|| ProviderError::Other("No encryption key for file".to_string()))?;
        let region = file_entry.metadata.get("region")
            .ok_or_else(|| ProviderError::Other("No region for file".to_string()))?;
        let bucket = file_entry.metadata.get("bucket")
            .ok_or_else(|| ProviderError::Other("No bucket for file".to_string()))?;
        let chunks: u32 = file_entry.metadata.get("chunks")
            .and_then(|s| s.parse().ok())
            .unwrap_or(1);

        // Download and decrypt each chunk
        let mut all_data = Vec::new();
        for chunk_idx in 0..chunks {
            let download_url = format!("https://egest.filen.io/{}/{}/{}/{}", region, bucket, uuid, chunk_idx);

            let resp = self.client.get(&download_url)
                .send().await
                .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

            if !resp.status().is_success() {
                return Err(ProviderError::TransferFailed(format!("Download chunk {} failed: {}", chunk_idx, resp.status())));
            }

            let encrypted = resp.bytes().await
                .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

            let decrypted = Self::decrypt_file_content(&encrypted, file_key)?;
            all_data.extend_from_slice(&decrypted);
        }

        tokio::fs::write(local_path, &all_data).await
            .map_err(|e| ProviderError::IoError(e))?;

        Ok(())
    }

    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        // Find the file to get its metadata (uuid, key, region, bucket, chunks)
        let normalized = Self::normalize_path(remote_path);
        let (parent_path, file_name) = match normalized.rfind('/') {
            Some(pos) if pos > 0 => (&normalized[..pos], &normalized[pos + 1..]),
            _ => ("/", normalized.trim_start_matches('/')),
        };

        let entries = self.list(parent_path).await?;
        let file_entry = entries.iter()
            .find(|e| !e.is_dir && e.name == file_name)
            .ok_or_else(|| ProviderError::NotFound(format!("File not found: {}", file_name)))?;

        let uuid = file_entry.metadata.get("uuid")
            .ok_or_else(|| ProviderError::Other("No UUID for file".to_string()))?;
        let file_key = file_entry.metadata.get("key")
            .ok_or_else(|| ProviderError::Other("No encryption key for file".to_string()))?;
        let region = file_entry.metadata.get("region")
            .ok_or_else(|| ProviderError::Other("No region for file".to_string()))?;
        let bucket = file_entry.metadata.get("bucket")
            .ok_or_else(|| ProviderError::Other("No bucket for file".to_string()))?;
        let chunks: u32 = file_entry.metadata.get("chunks")
            .and_then(|s| s.parse().ok())
            .unwrap_or(1);

        // Download and decrypt each chunk
        let mut all_data = Vec::new();
        for chunk_idx in 0..chunks {
            let download_url = format!("https://egest.filen.io/{}/{}/{}/{}", region, bucket, uuid, chunk_idx);

            let resp = self.client.get(&download_url)
                .send().await
                .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

            if !resp.status().is_success() {
                return Err(ProviderError::TransferFailed(format!("Download chunk {} failed: {}", chunk_idx, resp.status())));
            }

            let encrypted = resp.bytes().await
                .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

            let decrypted = Self::decrypt_file_content(&encrypted, file_key)?;
            all_data.extend_from_slice(&decrypted);
        }

        Ok(all_data)
    }

    async fn upload(&mut self, local_path: &str, remote_path: &str, _progress: Option<Box<dyn Fn(u64, u64) + Send>>) -> Result<(), ProviderError> {
        let normalized = Self::normalize_path(remote_path);
        let (parent_path, file_name) = match normalized.rfind('/') {
            Some(pos) if pos > 0 => (&normalized[..pos], &normalized[pos + 1..]),
            _ => ("/", normalized.trim_start_matches('/')),
        };

        let parent_uuid = self.resolve_folder_uuid(parent_path).await?;
        let data = tokio::fs::read(local_path).await
            .map_err(|e| ProviderError::IoError(e))?;
        let file_size = data.len();
        let mime_type = mime_guess::from_path(file_name).first_or_octet_stream().to_string();

        // Generate per-file encryption key and upload key
        let file_key: String = (0..32).map(|_| format!("{:02x}", rand::random::<u8>())).collect();
        let upload_key: String = (0..32).map(|_| format!("{:02x}", rand::random::<u8>())).collect();
        let file_uuid = uuid::Uuid::new_v4().to_string();

        // Encrypt file content
        let encrypted = Self::encrypt_file_content(&data, &file_key)?;

        // Hash of encrypted data (SHA-512)
        let mut hash_hasher = Sha512::new();
        hash_hasher.update(&encrypted);
        let chunk_hash = hex::encode(hash_hasher.finalize());

        // Encrypt metadata
        let now = chrono::Utc::now().timestamp_millis();
        let metadata = serde_json::json!({
            "name": file_name,
            "size": file_size,
            "mime": mime_type,
            "key": file_key,
            "lastModified": now,
        });
        let encrypted_metadata = self.encrypt_metadata(&metadata.to_string())?;

        // Encrypt name and size for upload/done
        let encrypted_name = self.encrypt_metadata(file_name)?;
        let encrypted_size = self.encrypt_metadata(&file_size.to_string())?;

        // Hash of file name: SHA-1(SHA-512(name.toLowerCase()).hex()).hex()
        let name_hashed = Self::hash_name(file_name);

        // Build URL params and checksum header (matching Filen SDK)
        let url_params = format!(
            "uuid={}&index=0&parent={}&uploadKey={}",
            file_uuid, parent_uuid, upload_key
        );
        let upload_url = format!(
            "https://ingest.filen.io/v3/upload?{}&hash={}",
            url_params, chunk_hash
        );

        // Checksum header: SHA-512 of JSON stringified URL params (must match JS key order)
        let checksum_input = format!(
            r#"{{"uuid":"{}","index":"0","parent":"{}","uploadKey":"{}","hash":"{}"}}"#,
            file_uuid, parent_uuid, upload_key, chunk_hash
        );
        let mut checksum_hasher = Sha512::new();
        checksum_hasher.update(checksum_input.as_bytes());
        let checksum = hex::encode(checksum_hasher.finalize());

        // Send encrypted data as raw POST body (not multipart)
        filen_log(&format!("upload: {} bytes encrypted, hash={}..., params={}", encrypted.len(), &chunk_hash[..16], url_params));
        let resp = self.client.post(&upload_url)
            .header("Authorization", HeaderValue::from_str(&format!("Bearer {}", self.api_key)).unwrap())
            .header("Checksum", HeaderValue::from_str(&checksum).unwrap())
            .body(encrypted)
            .send().await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        let status = resp.status();
        let resp_text = resp.text().await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        filen_log(&format!("upload response: status={}, body={}", status, &resp_text[..resp_text.len().min(500)]));

        if !status.is_success() {
            return Err(ProviderError::TransferFailed(format!("Upload chunk failed: {} - {}", status, resp_text)));
        }

        let upload_resp: serde_json::Value = serde_json::from_str(&resp_text)
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        if upload_resp["status"].as_bool() != Some(true) {
            return Err(ProviderError::TransferFailed(
                format!("Upload rejected: {}", upload_resp["message"].as_str().unwrap_or("unknown"))
            ));
        }

        // Generate random string for rm parameter
        let rm: String = (0..32).map(|_| format!("{:02x}", rand::random::<u8>())).collect();

        // Mark upload as done
        let done_resp: serde_json::Value = self.client
            .post(&format!("{}/v3/upload/done", GATEWAY))
            .header("Authorization", HeaderValue::from_str(&format!("Bearer {}", self.api_key)).unwrap())
            .header(CONTENT_TYPE, "application/json")
            .json(&serde_json::json!({
                "uuid": file_uuid,
                "name": encrypted_name,
                "nameHashed": name_hashed,
                "size": encrypted_size,
                "chunks": 1,
                "mime": mime_type,
                "rm": rm,
                "metadata": encrypted_metadata,
                "version": 2,
                "uploadKey": upload_key,
            }))
            .send().await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?
            .json().await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        if done_resp["status"].as_bool() != Some(true) {
            return Err(ProviderError::TransferFailed(
                done_resp["message"].as_str().unwrap_or("Upload finalization failed").to_string()
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

        let resp: CreateFolderResponse = self.client
            .post(&format!("{}/v3/dir/create", GATEWAY))
            .header("Authorization", HeaderValue::from_str(&format!("Bearer {}", self.api_key)).unwrap())
            .json(&serde_json::json!({
                "uuid": folder_uuid,
                "name": encrypted_name,
                "nameHashed": Self::hash_name(folder_name),
                "parent": parent_uuid,
            }))
            .send().await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?
            .json().await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        if !resp.status {
            let msg = resp.message.unwrap_or_else(|| "mkdir failed".to_string());
            filen_log(&format!("mkdir FAILED '{}': {}", path, msg));
            return Err(ProviderError::Other(msg));
        }

        // Call v3/dir/metadata for each master key (required for Filen webapp compatibility)
        for key in &self.master_keys {
            let encrypted_for_key = Self::encrypt_metadata_with_key(&name_json, key)?;
            let _: serde_json::Value = self.client
                .post(&format!("{}/v3/dir/metadata", GATEWAY))
                .header("Authorization", HeaderValue::from_str(&format!("Bearer {}", self.api_key)).unwrap())
                .json(&serde_json::json!({
                    "uuid": folder_uuid,
                    "encrypted": encrypted_for_key,
                }))
                .send().await
                .map_err(|e| ProviderError::NetworkError(e.to_string()))?
                .json().await
                .map_err(|e| ProviderError::ParseError(e.to_string()))?;
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
        let entry = entries.iter()
            .find(|e| e.name == file_name)
            .ok_or_else(|| ProviderError::NotFound(file_name.to_string()))?;

        let uuid = entry.metadata.get("uuid")
            .ok_or_else(|| ProviderError::Other("No UUID".to_string()))?;

        let endpoint = if entry.is_dir { "v3/dir/trash" } else { "v3/file/trash" };

        let resp: GenericResponse = self.client
            .post(&format!("{}/{}", GATEWAY, endpoint))
            .header("Authorization", HeaderValue::from_str(&format!("Bearer {}", self.api_key)).unwrap())
            .json(&serde_json::json!({"uuid": uuid}))
            .send().await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?
            .json().await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        if !resp.status {
            return Err(ProviderError::Other(resp.message.unwrap_or_else(|| "Delete failed".to_string())));
        }

        Ok(())
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let folder_uuid = self.resolve_folder_uuid(path).await?;

        let resp: GenericResponse = self.client
            .post(&format!("{}/v3/dir/trash", GATEWAY))
            .header("Authorization", HeaderValue::from_str(&format!("Bearer {}", self.api_key)).unwrap())
            .json(&serde_json::json!({"uuid": folder_uuid}))
            .send().await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?
            .json().await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        if !resp.status {
            return Err(ProviderError::Other(resp.message.unwrap_or_else(|| "rmdir failed".to_string())));
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
        let new_name = std::path::Path::new(to).file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| to.to_string());

        // Find the item
        let normalized = Self::normalize_path(from);
        let (parent_path, old_name) = match normalized.rfind('/') {
            Some(pos) if pos > 0 => (&normalized[..pos], &normalized[pos + 1..]),
            _ => ("/", normalized.trim_start_matches('/')),
        };

        let entries = self.list(parent_path).await?;
        let entry = entries.iter()
            .find(|e| e.name == old_name)
            .ok_or_else(|| ProviderError::NotFound(old_name.to_string()))?;

        let uuid = entry.metadata.get("uuid")
            .ok_or_else(|| ProviderError::Other("No UUID".to_string()))?;

        filen_log(&format!("rename: '{}' -> '{}', is_dir={}, uuid={}", from, to, entry.is_dir, uuid));

        let name_hashed = Self::hash_name(&new_name);

        if entry.is_dir {
            // Folder rename: name is JSON {"name":"..."}, also call dir/metadata
            let name_json = serde_json::json!({"name": new_name}).to_string();
            let encrypted_name = self.encrypt_metadata(&name_json)?;

            let resp: GenericResponse = self.client
                .post(&format!("{}/v3/dir/rename", GATEWAY))
                .header("Authorization", HeaderValue::from_str(&format!("Bearer {}", self.api_key)).unwrap())
                .json(&serde_json::json!({
                    "uuid": uuid,
                    "name": encrypted_name,
                    "nameHashed": name_hashed,
                }))
                .send().await
                .map_err(|e| ProviderError::NetworkError(e.to_string()))?
                .json().await
                .map_err(|e| ProviderError::ParseError(e.to_string()))?;

            if !resp.status {
                let msg = resp.message.unwrap_or_else(|| "Rename failed".to_string());
                filen_log(&format!("rename dir FAILED: {}", msg));
                return Err(ProviderError::Other(msg));
            }

            // Update dir/metadata for webapp compatibility
            for key in &self.master_keys.clone() {
                let enc = Self::encrypt_metadata_with_key(&name_json, key)?;
                let _ = self.client
                    .post(&format!("{}/v3/dir/metadata", GATEWAY))
                    .header("Authorization", HeaderValue::from_str(&format!("Bearer {}", self.api_key)).unwrap())
                    .json(&serde_json::json!({"uuid": uuid, "encrypted": enc}))
                    .send().await;
            }
        } else {
            // File rename: need encrypted name + metadata JSON with updated name
            let encrypted_name = self.encrypt_metadata(&new_name)?;
            let file_key = entry.metadata.get("key").cloned().unwrap_or_default();
            let mime = entry.mime_type.clone().unwrap_or_else(|| "application/octet-stream".to_string());
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64;

            let meta_json = serde_json::json!({
                "name": new_name,
                "size": entry.size,
                "mime": mime,
                "key": file_key,
                "lastModified": now,
            });
            let encrypted_metadata = self.encrypt_metadata(&meta_json.to_string())?;

            let resp: GenericResponse = self.client
                .post(&format!("{}/v3/file/rename", GATEWAY))
                .header("Authorization", HeaderValue::from_str(&format!("Bearer {}", self.api_key)).unwrap())
                .json(&serde_json::json!({
                    "uuid": uuid,
                    "name": encrypted_name,
                    "nameHashed": name_hashed,
                    "metadata": encrypted_metadata,
                }))
                .send().await
                .map_err(|e| ProviderError::NetworkError(e.to_string()))?
                .json().await
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
        entries.into_iter()
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

    async fn keep_alive(&mut self) -> Result<(), ProviderError> { Ok(()) }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        Ok("Filen.io (E2E Encrypted Cloud Storage)".to_string())
    }

    async fn storage_info(&mut self) -> Result<StorageInfo, ProviderError> {
        let resp: UserInfoResponse = self.client
            .get(&format!("{}/v3/user/info", GATEWAY))
            .header("Authorization", HeaderValue::from_str(&format!("Bearer {}", self.api_key)).unwrap())
            .send().await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?
            .json().await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        let data = resp.data
            .ok_or_else(|| ProviderError::Other("No user info data".to_string()))?;

        Ok(StorageInfo {
            total: data.max_storage,
            used: data.storage_used,
            free: data.max_storage.saturating_sub(data.storage_used),
        })
    }

    fn supports_share_links(&self) -> bool { true }

    async fn create_share_link(&mut self, path: &str, _expires_in_secs: Option<u64>) -> Result<String, ProviderError> {
        // Find file/folder UUID from path
        let normalized = Self::normalize_path(path);
        let (parent_path, name) = match normalized.rfind('/') {
            Some(pos) if pos > 0 => (&normalized[..pos], &normalized[pos + 1..]),
            _ => ("/", normalized.trim_start_matches('/')),
        };

        let entries = self.list(parent_path).await?;
        let entry = entries.iter()
            .find(|e| e.name == name)
            .ok_or_else(|| ProviderError::NotFound(name.to_string()))?;

        let uuid = entry.metadata.get("uuid")
            .ok_or_else(|| ProviderError::Other("No UUID".to_string()))?;

        let endpoint = if entry.is_dir { "v3/dir/link/edit" } else { "v3/file/link/edit" };

        // Generate a link UUID
        let link_uuid = uuid::Uuid::new_v4().to_string();

        let resp: LinkEditResponse = self.client
            .post(&format!("{}/{}", GATEWAY, endpoint))
            .header("Authorization", HeaderValue::from_str(&format!("Bearer {}", self.api_key)).unwrap())
            .json(&serde_json::json!({
                "uuid": uuid,
                "linkUUID": link_uuid,
                "expiration": "never",
                "password": "empty",
                "downloadBtn": true,
                "type": "enable",
            }))
            .send().await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?
            .json().await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        if !resp.status {
            return Err(ProviderError::Other(
                resp.message.unwrap_or_else(|| "Failed to create share link".to_string())
            ));
        }

        // Filen share links follow this pattern
        Ok(format!("https://filen.io/d/{}", link_uuid))
    }

    async fn remove_share_link(&mut self, path: &str) -> Result<(), ProviderError> {
        let normalized = Self::normalize_path(path);
        let (parent_path, name) = match normalized.rfind('/') {
            Some(pos) if pos > 0 => (&normalized[..pos], &normalized[pos + 1..]),
            _ => ("/", normalized.trim_start_matches('/')),
        };

        let entries = self.list(parent_path).await?;
        let entry = entries.iter()
            .find(|e| e.name == name)
            .ok_or_else(|| ProviderError::NotFound(name.to_string()))?;

        let uuid = entry.metadata.get("uuid")
            .ok_or_else(|| ProviderError::Other("No UUID".to_string()))?;

        let endpoint = if entry.is_dir { "v3/dir/link/edit" } else { "v3/file/link/edit" };

        let resp: GenericResponse = self.client
            .post(&format!("{}/{}", GATEWAY, endpoint))
            .header("Authorization", HeaderValue::from_str(&format!("Bearer {}", self.api_key)).unwrap())
            .json(&serde_json::json!({
                "uuid": uuid,
                "type": "disable",
            }))
            .send().await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?
            .json().await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        if !resp.status {
            return Err(ProviderError::Other(
                resp.message.unwrap_or_else(|| "Failed to remove share link".to_string())
            ));
        }
        Ok(())
    }

    fn supports_find(&self) -> bool { true }

    async fn find(&mut self, path: &str, pattern: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let pattern_lower = pattern.to_lowercase();

        // Filen has no server-side search (zero-knowledge)
        // Recursively list and decrypt, filtering client-side
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
                        if entry.name.to_lowercase().contains(&pattern_lower) {
                            results.push(entry.clone());
                        }
                        if entry.is_dir && results.len() < 500 {
                            next_dirs.push(entry.path.clone());
                        }
                    }
                }
                if results.len() >= 500 { break; }
            }

            dirs_to_scan = next_dirs;
        }

        Ok(results)
    }
}
