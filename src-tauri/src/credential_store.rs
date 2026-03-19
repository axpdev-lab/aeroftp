// AeroFTP Universal Credential Vault
// Single encrypted vault for ALL credential types:
// - Server passwords (server_{id})
// - OAuth tokens (oauth_{provider}_*)
// - AI API keys (ai_apikey_{provider})
//
// Two modes:
// - Auto mode (default): vault.key stores passphrase in cleartext, protected by OS permissions
// - Master mode (optional): vault.key passphrase encrypted with Argon2id(user_password) + AES-GCM
//
// v2.0 — February 2026

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use serde::{Deserialize, Serialize};
use secrecy::zeroize::Zeroize;
use tracing::{info, warn};

// Cached unlocked vault state: (vault.db path, vault_key)
//
// M15 SECURITY NOTE: The 32-byte vault key is held in a static Mutex in user-space memory.
// Ideally we would use mlock(2) to prevent the OS from swapping this page to disk, but:
// 1. mlock requires platform-specific unsafe code (libc::mlock on Unix, VirtualLock on Windows)
// 2. The static Mutex<Option<...>> layout doesn't guarantee the key bytes are page-aligned
// 3. secrecy::SecretBox (used elsewhere) doesn't support mlock either
// This means the key could theoretically be written to swap. On modern systems with encrypted
// swap (default on macOS, optional on Linux with LUKS), this risk is mitigated.
// TODO: Consider wrapping in a custom MlockedBox<[u8; 32]> if threat model requires it.
static VAULT_CACHE: Mutex<Option<(PathBuf, [u8; 32])>> = Mutex::new(None);

// Serializes all vault write operations to prevent concurrent read-modify-write races
static VAULT_WRITE_LOCK: Mutex<()> = Mutex::new(());

const VAULT_FILENAME: &str = "vault.db";
const VAULTKEY_FILENAME: &str = "vault.key";

// vault.key binary format constants
const VAULTKEY_MAGIC: &[u8; 8] = b"AEROVKEY";
const VAULTKEY_VERSION: u8 = 2;
const MODE_AUTO: u8 = 0x00;
const MODE_MASTER: u8 = 0x01;
const PASSPHRASE_LEN: usize = 64;

// ============ Error Types ============

#[derive(Debug, thiserror::Error)]
pub enum CredentialError {
    #[error("Vault not initialized")]
    VaultNotInitialized,
    #[error("Invalid master password")]
    InvalidMasterPassword,
    #[error("Credential not found: {0}")]
    NotFound(String),
    #[error("Encryption error: {0}")]
    Encryption(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Serialization error: {0}")]
    Serialization(String),
    #[error("Invalid vault.key format")]
    InvalidKeyFile,
    #[error("Master password already set")]
    MasterAlreadySet,
    #[error("Master password not set")]
    MasterNotSet,
    #[error("Password must be at least 8 characters")]
    PasswordTooShort,
}

// ============ Vault File Format (vault.db) ============

#[derive(Serialize, Deserialize)]
struct VaultFile {
    version: u32,
    verify_nonce: Vec<u8>,   // 12 bytes - nonce for verification token
    verify_data: Vec<u8>,    // encrypted "aeroftp_vault_v2_ok" for key verification
    entries: HashMap<String, VaultEntry>,
}

#[derive(Serialize, Deserialize, Clone)]
struct VaultEntry {
    nonce: Vec<u8>,   // 12 bytes
    data: Vec<u8>,    // [ciphertext][tag 16B]
}

// ============ VaultKeyFile (vault.key) ============

/// Represents the vault.key file that stores the passphrase
struct VaultKeyFile {
    mode: VaultKeyMode,
}

enum VaultKeyMode {
    /// Passphrase stored in cleartext, protected by OS file permissions
    Auto { passphrase: [u8; PASSPHRASE_LEN] },
    /// Passphrase encrypted with Argon2id(user_password) + AES-256-GCM
    Master {
        salt: [u8; 32],
        nonce: [u8; 12],
        encrypted_passphrase: Vec<u8>, // 64 bytes passphrase + 16 bytes GCM tag = 80 bytes
    },
}

impl VaultKeyFile {
    /// Get vault.key path
    fn path() -> Result<PathBuf, CredentialError> {
        let dir = config_dir()?;
        Ok(dir.join(VAULTKEY_FILENAME))
    }

    /// Check if vault.key exists
    fn exists() -> bool {
        Self::path().map(|p| p.exists()).unwrap_or(false)
    }

    /// Check if vault.key is in master mode (without reading passphrase)
    fn is_master_mode() -> bool {
        let path = match Self::path() {
            Ok(p) => p,
            Err(_) => return false,
        };
        let data = match std::fs::read(&path) {
            Ok(d) => d,
            Err(_) => return false,
        };
        // Magic(8) + Version(1) + Mode(1)
        if data.len() < 10 {
            return false;
        }
        if &data[0..8] != VAULTKEY_MAGIC {
            return false;
        }
        data[9] == MODE_MASTER
    }

    /// Serialize to bytes
    fn to_bytes(&self) -> Vec<u8> {
        let mut data = Vec::with_capacity(140);
        data.extend_from_slice(VAULTKEY_MAGIC);
        data.push(VAULTKEY_VERSION);

        match &self.mode {
            VaultKeyMode::Auto { passphrase } => {
                data.push(MODE_AUTO);
                data.extend_from_slice(passphrase);
                data.extend_from_slice(&[0u8; 2]); // padding
            }
            VaultKeyMode::Master { salt, nonce, encrypted_passphrase } => {
                data.push(MODE_MASTER);
                data.extend_from_slice(salt);
                data.extend_from_slice(nonce);
                data.extend_from_slice(encrypted_passphrase);
                data.extend_from_slice(&[0u8; 2]); // padding
            }
        }
        data
    }

    /// Parse from bytes
    fn from_bytes(data: &[u8]) -> Result<Self, CredentialError> {
        // Minimum: magic(8) + version(1) + mode(1) + passphrase(64) + padding(2) = 76
        if data.len() < 76 {
            return Err(CredentialError::InvalidKeyFile);
        }
        if &data[0..8] != VAULTKEY_MAGIC {
            return Err(CredentialError::InvalidKeyFile);
        }
        if data[8] != VAULTKEY_VERSION {
            return Err(CredentialError::InvalidKeyFile);
        }

        let mode_byte = data[9];
        match mode_byte {
            MODE_AUTO => {
                if data.len() < 76 {
                    return Err(CredentialError::InvalidKeyFile);
                }
                let mut passphrase = [0u8; PASSPHRASE_LEN];
                passphrase.copy_from_slice(&data[10..10 + PASSPHRASE_LEN]);
                Ok(VaultKeyFile {
                    mode: VaultKeyMode::Auto { passphrase },
                })
            }
            MODE_MASTER => {
                // magic(8) + ver(1) + mode(1) + salt(32) + nonce(12) + enc_passphrase(80) + padding(2) = 136
                if data.len() < 136 {
                    return Err(CredentialError::InvalidKeyFile);
                }
                let mut salt = [0u8; 32];
                salt.copy_from_slice(&data[10..42]);
                let mut nonce = [0u8; 12];
                nonce.copy_from_slice(&data[42..54]);
                let encrypted_passphrase = data[54..134].to_vec();
                Ok(VaultKeyFile {
                    mode: VaultKeyMode::Master { salt, nonce, encrypted_passphrase },
                })
            }
            _ => Err(CredentialError::InvalidKeyFile),
        }
    }

    /// Read vault.key from disk
    fn read() -> Result<Self, CredentialError> {
        let path = Self::path()?;
        if !path.exists() {
            return Err(CredentialError::VaultNotInitialized);
        }
        let data = std::fs::read(&path)?;
        Self::from_bytes(&data)
    }

    /// Write vault.key to disk with secure permissions
    fn write(&self) -> Result<(), CredentialError> {
        let path = Self::path()?;
        let data = self.to_bytes();
        // Atomic write via temp+rename (with Windows fallback)
        let tmp_path = path.with_extension("tmp");
        std::fs::write(&tmp_path, &data)?;
        if let Ok(f) = std::fs::File::open(&tmp_path) {
            let _ = f.sync_all();
        }
        // Rename temp → target (Windows: retry with remove-first, then direct write fallback)
        if std::fs::rename(&tmp_path, &path).is_err() {
            #[cfg(windows)]
            {
                let _ = std::fs::remove_file(&path);
                if std::fs::rename(&tmp_path, &path).is_err() {
                    std::fs::write(&path, &data)?;
                    let _ = std::fs::remove_file(&tmp_path);
                }
            }
            #[cfg(not(windows))]
            {
                return Err(CredentialError::Io(
                    std::io::Error::other("vault.key rename failed")
                ));
            }
        }
        if let Ok(f) = std::fs::File::open(&path) {
            let _ = f.sync_all();
        }
        #[cfg(unix)]
        if let Some(parent) = path.parent() {
            if let Ok(dir) = std::fs::File::open(parent) {
                let _ = dir.sync_all();
            }
        }
        ensure_secure_permissions(&path)?;
        Ok(())
    }

    /// Decrypt passphrase using master password (master mode only)
    fn decrypt_passphrase(&self, password: &str) -> Result<[u8; PASSPHRASE_LEN], CredentialError> {
        match &self.mode {
            VaultKeyMode::Auto { passphrase } => Ok(*passphrase),
            VaultKeyMode::Master { salt, nonce, encrypted_passphrase } => {
                let key = crate::crypto::derive_key_strong(password, salt)
                    .map_err(CredentialError::Encryption)?;
                let plaintext = crate::crypto::decrypt_aes_gcm(&key, nonce, encrypted_passphrase)
                    .map_err(|_| CredentialError::InvalidMasterPassword)?;
                if plaintext.len() != PASSPHRASE_LEN {
                    return Err(CredentialError::InvalidKeyFile);
                }
                let mut passphrase = [0u8; PASSPHRASE_LEN];
                passphrase.copy_from_slice(&plaintext);
                Ok(passphrase)
            }
        }
    }
}

// ============ Credential Store ============

pub struct CredentialStore {
    vault_path: PathBuf,
    vault_key: [u8; 32],
}

/// GAP-E01: Zeroize vault key on drop to prevent key material lingering in memory
impl Drop for CredentialStore {
    fn drop(&mut self) {
        self.vault_key.zeroize();
    }
}

impl CredentialStore {
    // ---- Initialization ----

    /// VER-005: File-based lock to prevent concurrent vault creation (CLI+GUI TOCTOU).
    /// Uses `create_new(true)` as an atomic mutex. Stale locks (>30s) are auto-removed.
    const VAULT_INIT_LOCK_FILENAME: &'static str = ".vault.lock";
    const VAULT_INIT_LOCK_STALE_SECS: u64 = 30;
    const VAULT_INIT_LOCK_RETRY_MS: u64 = 200;
    const VAULT_INIT_LOCK_MAX_RETRIES: u32 = 20; // 20 * 200ms = 4s max wait

    /// Acquire a file-based init lock. Returns the lock file path on success.
    fn acquire_init_lock() -> Result<PathBuf, CredentialError> {
        let dir = config_dir()?;
        let lock_path = dir.join(Self::VAULT_INIT_LOCK_FILENAME);

        for attempt in 0..Self::VAULT_INIT_LOCK_MAX_RETRIES {
            // Try atomic creation — fails if file already exists
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock_path)
            {
                Ok(_file) => {
                    return Ok(lock_path);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    // Lock file exists — check if stale
                    if let Ok(metadata) = std::fs::metadata(&lock_path) {
                        if let Ok(modified) = metadata.modified() {
                            if let Ok(elapsed) = modified.elapsed() {
                                if elapsed.as_secs() > Self::VAULT_INIT_LOCK_STALE_SECS {
                                    warn!("Removing stale vault init lock ({}s old)", elapsed.as_secs());
                                    let _ = std::fs::remove_file(&lock_path);
                                    // Retry immediately after removing stale lock
                                    continue;
                                }
                            }
                        }
                    }
                    // Lock is held by another process — wait and retry
                    if attempt < Self::VAULT_INIT_LOCK_MAX_RETRIES - 1 {
                        std::thread::sleep(std::time::Duration::from_millis(Self::VAULT_INIT_LOCK_RETRY_MS));
                    }
                }
                Err(e) => return Err(CredentialError::Io(e)),
            }
        }

        Err(CredentialError::Io(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "Failed to acquire vault init lock after maximum retries",
        )))
    }

    /// Release the file-based init lock
    fn release_init_lock(lock_path: &Path) {
        let _ = std::fs::remove_file(lock_path);
    }

    /// Initialize the credential store at app startup.
    /// Returns "OK" if vault is open, "MASTER_PASSWORD_REQUIRED" if locked.
    pub fn init() -> Result<String, CredentialError> {
        if !VaultKeyFile::exists() {
            // VER-005: Acquire file lock to prevent concurrent CLI+GUI vault creation
            let lock_path = Self::acquire_init_lock()?;
            let result = if !VaultKeyFile::exists() {
                // Double-check after acquiring lock (another process may have created it)
                Self::first_run_init()
            } else {
                Ok(())
            };
            Self::release_init_lock(&lock_path);
            result?;

            // If another process created the vault while we waited, fall through
            // to the normal open path below. If we created it, first_run_init
            // already cached the key, so return OK.
            if Self::from_cache().is_some() {
                return Ok("OK".to_string());
            }
        }

        let key_file = VaultKeyFile::read()?;
        match &key_file.mode {
            VaultKeyMode::Auto { passphrase } => {
                let vault_key = crate::crypto::derive_from_passphrase(passphrase);
                let vault_path = Self::vault_path()?;

                // If vault.db doesn't exist yet (shouldn't happen but be safe), create it
                if !vault_path.exists() {
                    Self::create_empty_vault(&vault_path, &vault_key)?;
                }

                Self::open_and_cache(vault_path, vault_key)?;
                Ok("OK".to_string())
            }
            VaultKeyMode::Master { .. } => {
                Ok("MASTER_PASSWORD_REQUIRED".to_string())
            }
        }
    }

    /// First run: generate random passphrase, create vault.key (auto) + vault.db (empty)
    fn first_run_init() -> Result<(), CredentialError> {
        // Ensure config directory exists with secure permissions
        let dir = config_dir()?;

        // Generate 64-byte random passphrase (512 bits of entropy)
        let mut passphrase_bytes = crate::crypto::random_bytes(PASSPHRASE_LEN);
        let mut passphrase = [0u8; PASSPHRASE_LEN];
        passphrase.copy_from_slice(&passphrase_bytes);
        // A2-03: Zeroize intermediate buffer immediately
        passphrase_bytes.zeroize();

        // Write vault.key in auto mode
        let key_file = VaultKeyFile {
            mode: VaultKeyMode::Auto { passphrase },
        };
        key_file.write()?;

        // Derive vault key and create empty vault.db
        let vault_key = crate::crypto::derive_from_passphrase(&passphrase);
        let vault_path = dir.join(VAULT_FILENAME);
        Self::create_empty_vault(&vault_path, &vault_key)?;

        // A2-03: Zeroize passphrase after use
        passphrase.zeroize();

        // Cache vault key in memory
        Self::open_and_cache(vault_path, vault_key)?;

        // Harden the entire config directory
        let _ = harden_config_directory();

        info!("Universal credential vault initialized (auto mode)");
        Ok(())
    }

    /// Create an empty vault.db with a verification token
    fn create_empty_vault(path: &Path, vault_key: &[u8; 32]) -> Result<(), CredentialError> {
        let verify_nonce = crate::crypto::random_bytes(12);
        let verify_data = crate::crypto::encrypt_aes_gcm(vault_key, &verify_nonce, b"aeroftp_vault_v2_ok")
            .map_err(CredentialError::Encryption)?;

        let vault = VaultFile {
            version: 2,
            verify_nonce,
            verify_data,
            entries: HashMap::new(),
        };

        Self::write_vault(path, &vault)?;
        Ok(())
    }

    /// Derive vault key from passphrase using HKDF-SHA256
    fn derive_vault_key(passphrase: &[u8; PASSPHRASE_LEN]) -> [u8; 32] {
        crate::crypto::derive_from_passphrase(passphrase)
    }

    /// Open vault.db and cache the key in memory
    fn open_and_cache(vault_path: PathBuf, vault_key: [u8; 32]) -> Result<(), CredentialError> {
        // Verify we can read and decrypt the vault
        let vault = Self::read_vault(&vault_path)?;
        crate::crypto::decrypt_aes_gcm(&vault_key, &vault.verify_nonce, &vault.verify_data)
            .map_err(|_| CredentialError::InvalidMasterPassword)?;

        // Cache in static
        if let Ok(mut cache) = VAULT_CACHE.lock() {
            *cache = Some((vault_path, vault_key));
        }
        info!("Credential vault opened and cached");
        Ok(())
    }

    // ---- Cache Management ----

    /// Get a store instance from cache (vault must be open)
    pub fn from_cache() -> Option<Self> {
        let cache = VAULT_CACHE.lock().ok()?;
        let (path, key) = cache.as_ref()?;
        Some(Self {
            vault_path: path.clone(),
            vault_key: *key,
        })
    }

    /// Clear the vault cache (on lock)
    pub fn clear_cache() {
        if let Ok(mut cache) = VAULT_CACHE.lock() {
            if let Some((_, ref mut key)) = *cache {
                // GAP-E01: Zeroize vault key via Zeroize trait (constant-time)
                key.zeroize();
            }
            *cache = None;
        }
    }

    // ---- Master Password Management ----

    /// Unlock vault with master password (master mode only).
    /// Note: For TOTP-aware unlock, use verify_master() + cache_vault() instead.
    #[allow(dead_code)]
    pub fn unlock_with_master(password: &str) -> Result<(), CredentialError> {
        let key_file = VaultKeyFile::read()?;
        let passphrase = key_file.decrypt_passphrase(password)?;
        let vault_key = Self::derive_vault_key(&passphrase);
        let vault_path = Self::vault_path()?;
        Self::open_and_cache(vault_path, vault_key)
    }

    /// A2-08: Verify master password and return vault key material WITHOUT caching.
    /// Used by unlock_credential_store to defer caching until after TOTP verification.
    pub fn verify_master(password: &str) -> Result<(PathBuf, [u8; 32]), CredentialError> {
        let key_file = VaultKeyFile::read()?;
        let passphrase = key_file.decrypt_passphrase(password)?;
        let vault_key = Self::derive_vault_key(&passphrase);
        let vault_path = Self::vault_path()?;

        // Verify we can read and decrypt the vault (same as open_and_cache)
        let vault = Self::read_vault(&vault_path)?;
        crate::crypto::decrypt_aes_gcm(&vault_key, &vault.verify_nonce, &vault.verify_data)
            .map_err(|_| CredentialError::InvalidMasterPassword)?;

        Ok((vault_path, vault_key))
    }

    /// A2-08: Cache previously verified vault key material.
    /// Called after TOTP verification succeeds.
    pub(crate) fn cache_vault(vault_path: PathBuf, vault_key: [u8; 32]) {
        if let Ok(mut cache) = VAULT_CACHE.lock() {
            *cache = Some((vault_path, vault_key));
        }
        info!("Credential vault opened and cached");
    }

    /// Enable master password: encrypt vault.key passphrase with user password
    pub fn enable_master_password(password: &str) -> Result<(), CredentialError> {
        if password.len() < 8 {
            return Err(CredentialError::PasswordTooShort);
        }

        let key_file = VaultKeyFile::read()?;
        let passphrase = match &key_file.mode {
            VaultKeyMode::Auto { passphrase } => *passphrase,
            VaultKeyMode::Master { .. } => return Err(CredentialError::MasterAlreadySet),
        };

        // Encrypt passphrase with Argon2id(password) + AES-GCM
        let salt = crate::crypto::random_bytes(32);
        let mut salt_arr = [0u8; 32];
        salt_arr.copy_from_slice(&salt);

        let key = crate::crypto::derive_key_strong(password, &salt)
            .map_err(CredentialError::Encryption)?;

        let nonce_bytes = crate::crypto::random_bytes(12);
        let mut nonce_arr = [0u8; 12];
        nonce_arr.copy_from_slice(&nonce_bytes);

        let encrypted_passphrase = crate::crypto::encrypt_aes_gcm(&key, &nonce_bytes, &passphrase)
            .map_err(CredentialError::Encryption)?;

        let new_key_file = VaultKeyFile {
            mode: VaultKeyMode::Master {
                salt: salt_arr,
                nonce: nonce_arr,
                encrypted_passphrase,
            },
        };
        new_key_file.write()?;

        info!("Master password enabled — vault.key encrypted");
        Ok(())
    }

    /// Disable master password: decrypt passphrase and store in cleartext
    pub fn disable_master_password(password: &str) -> Result<(), CredentialError> {
        let key_file = VaultKeyFile::read()?;
        let passphrase = match &key_file.mode {
            VaultKeyMode::Master { .. } => key_file.decrypt_passphrase(password)?,
            VaultKeyMode::Auto { .. } => return Err(CredentialError::MasterNotSet),
        };

        let new_key_file = VaultKeyFile {
            mode: VaultKeyMode::Auto { passphrase },
        };
        new_key_file.write()?;

        info!("Master password disabled — vault.key in auto mode");
        Ok(())
    }

    /// Change master password
    pub fn change_master_password(old_password: &str, new_password: &str) -> Result<(), CredentialError> {
        if new_password.len() < 8 {
            return Err(CredentialError::PasswordTooShort);
        }

        let key_file = VaultKeyFile::read()?;
        let passphrase = key_file.decrypt_passphrase(old_password)?;

        // Re-encrypt with new password
        let salt = crate::crypto::random_bytes(32);
        let mut salt_arr = [0u8; 32];
        salt_arr.copy_from_slice(&salt);

        let key = crate::crypto::derive_key_strong(new_password, &salt)
            .map_err(CredentialError::Encryption)?;

        let nonce_bytes = crate::crypto::random_bytes(12);
        let mut nonce_arr = [0u8; 12];
        nonce_arr.copy_from_slice(&nonce_bytes);

        let encrypted_passphrase = crate::crypto::encrypt_aes_gcm(&key, &nonce_bytes, &passphrase)
            .map_err(CredentialError::Encryption)?;

        let new_key_file = VaultKeyFile {
            mode: VaultKeyMode::Master {
                salt: salt_arr,
                nonce: nonce_arr,
                encrypted_passphrase,
            },
        };
        new_key_file.write()?;

        info!("Master password changed");
        Ok(())
    }

    /// Lock: clear VAULT_CACHE
    pub fn lock() {
        Self::clear_cache();
        info!("Credential vault locked");
    }

    /// Check if vault.key is in master mode
    pub fn is_master_mode() -> bool {
        VaultKeyFile::is_master_mode()
    }

    /// Check if vault.key exists
    pub fn vault_exists() -> bool {
        VaultKeyFile::exists()
    }

    // ---- CRUD Operations ----

    // A2-07: Reserved account names that must not be overwritten by external callers
    const RESERVED_KEYS: &[&str] = &["totp_secret", "master_password_hash", "vault_key"];

    /// Store a credential
    pub fn store(&self, account: &str, secret: &str) -> Result<(), CredentialError> {
        // A2-07: Prevent overwriting system-reserved keys
        if Self::RESERVED_KEYS.contains(&account) {
            return Err(CredentialError::Encryption("Reserved account name".to_string()));
        }
        self.store_entry(account, secret)
    }

    /// A2-05: Internal store that bypasses reserved key check.
    /// Used by totp_enable to atomically store the TOTP secret.
    pub(crate) fn store_internal(&self, account: &str, secret: &str) -> Result<(), CredentialError> {
        self.store_entry(account, secret)
    }

    /// Shared store implementation
    fn store_entry(&self, account: &str, secret: &str) -> Result<(), CredentialError> {
        let _lock = VAULT_WRITE_LOCK.lock().map_err(|_| CredentialError::Encryption("vault write lock poisoned".to_string()))?;
        let mut vault = Self::read_vault(&self.vault_path)?;
        let nonce = crate::crypto::random_bytes(12);
        let data = crate::crypto::encrypt_aes_gcm(&self.vault_key, &nonce, secret.as_bytes())
            .map_err(CredentialError::Encryption)?;
        vault.entries.insert(account.to_string(), VaultEntry { nonce, data });
        Self::write_vault(&self.vault_path, &vault)?;
        info!("Credential stored: {}", account);
        Ok(())
    }

    /// Retrieve a credential
    pub fn get(&self, account: &str) -> Result<String, CredentialError> {
        let vault = Self::read_vault(&self.vault_path)?;
        let entry = vault.entries.get(account)
            .ok_or_else(|| CredentialError::NotFound(account.to_string()))?;
        let plaintext = crate::crypto::decrypt_aes_gcm(&self.vault_key, &entry.nonce, &entry.data)
            .map_err(CredentialError::Encryption)?;
        String::from_utf8(plaintext)
            .map_err(|e| CredentialError::Encryption(e.to_string()))
    }

    /// Delete a credential
    pub fn delete(&self, account: &str) -> Result<(), CredentialError> {
        let _lock = VAULT_WRITE_LOCK.lock().map_err(|_| CredentialError::Encryption("vault write lock poisoned".to_string()))?;
        let mut vault = Self::read_vault(&self.vault_path)?;
        vault.entries.remove(account);
        Self::write_vault(&self.vault_path, &vault)?;
        info!("Credential deleted: {}", account);
        Ok(())
    }

    /// List all stored account names
    pub fn list_accounts(&self) -> Result<Vec<String>, CredentialError> {
        let vault = Self::read_vault(&self.vault_path)?;
        Ok(vault.entries.keys().cloned().collect())
    }

    // ---- Internal Helpers ----

    fn vault_path() -> Result<PathBuf, CredentialError> {
        let dir = config_dir()?;
        Ok(dir.join(VAULT_FILENAME))
    }

    fn read_vault(path: &Path) -> Result<VaultFile, CredentialError> {
        let data = std::fs::read(path)?;
        serde_json::from_slice(&data)
            .map_err(|e| CredentialError::Serialization(e.to_string()))
    }

    fn write_vault(path: &Path, vault: &VaultFile) -> Result<(), CredentialError> {
        let data = serde_json::to_vec_pretty(vault)
            .map_err(|e| CredentialError::Serialization(e.to_string()))?;
        // A2-01: Atomic write via temp+rename to prevent corruption on crash
        let tmp_path = path.with_extension("tmp");
        std::fs::write(&tmp_path, &data)?;
        // fsync temp file before rename
        if let Ok(f) = std::fs::File::open(&tmp_path) {
            let _ = f.sync_all();
        }
        // Rename temp → target. On Windows, rename can fail if the destination is
        // locked by a concurrent read (VAULT_WRITE_LOCK only covers writes, not reads).
        // Fallback: remove destination first, then retry rename; last resort: direct write.
        if std::fs::rename(&tmp_path, path).is_err() {
            #[cfg(windows)]
            {
                // Windows: try remove + rename, then fallback to direct overwrite
                let _ = std::fs::remove_file(path);
                if std::fs::rename(&tmp_path, path).is_err() {
                    // Last resort: direct write (non-atomic but functional)
                    std::fs::write(path, &data)?;
                    let _ = std::fs::remove_file(&tmp_path);
                }
            }
            #[cfg(not(windows))]
            {
                // On Unix rename is atomic even if destination exists — this shouldn't fail
                return Err(CredentialError::Io(
                    std::io::Error::other("vault.db rename failed")
                ));
            }
        }
        // fsync the final file
        if let Ok(f) = std::fs::File::open(path) {
            let _ = f.sync_all();
        }
        // fsync parent directory (Unix only — Windows doesn't support opening dirs as files)
        #[cfg(unix)]
        if let Some(parent) = path.parent() {
            if let Ok(dir) = std::fs::File::open(parent) {
                let _ = dir.sync_all();
            }
        }
        ensure_secure_permissions(path)?;
        Ok(())
    }
}

// ============ Shared Helpers ============

// fsync_file_and_parent removed — fsync logic is now inline in write_vault/VaultKeyFile::write
// with platform-specific handling (Windows can't open directories as files)

/// Get aeroftp config directory, creating it with secure permissions if needed
fn config_dir() -> Result<PathBuf, CredentialError> {
    let base = dirs::config_dir()
        .or_else(dirs::home_dir)
        .ok_or_else(|| CredentialError::Io(
            std::io::Error::new(std::io::ErrorKind::NotFound, "No config directory")
        ))?;
    let dir = base.join("aeroftp");
    if !dir.exists() {
        std::fs::create_dir_all(&dir)?;
        ensure_secure_permissions(&dir)?;
    }
    Ok(dir)
}

/// Ensure secure file/directory permissions (0o600 files, 0o700 dirs on Unix; ACL on Windows)
pub(crate) fn ensure_secure_permissions(path: &Path) -> Result<(), CredentialError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if path.is_dir() { 0o700 } else { 0o600 };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    }
    #[cfg(windows)]
    {
        crate::windows_acl::restrict_to_owner(path);
    }
    Ok(())
}

/// Ensure the entire aeroftp config directory has secure permissions
pub(crate) fn harden_config_directory() -> Result<(), CredentialError> {
    let dir = config_dir()?;
    if dir.exists() {
        ensure_secure_permissions(&dir)?;
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                ensure_secure_permissions(&path)?;
                if path.is_dir() {
                    if let Ok(sub_entries) = std::fs::read_dir(&path) {
                        for sub_entry in sub_entries.flatten() {
                            ensure_secure_permissions(&sub_entry.path())?;
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// Securely delete a file (overwrite in-place with zeros then random, then remove)
/// Uses OpenOptions without truncate to preserve original file size on disk.
/// Chunked writes (1 MiB) prevent OOM on large files.
pub fn secure_delete(path: &Path) -> Result<(), CredentialError> {
    use std::io::{Write, Seek, SeekFrom};
    use std::fs::OpenOptions;

    if path.exists() {
        let size = std::fs::metadata(path)?.len() as usize;
        if size > 0 {
            const CHUNK: usize = 1024 * 1024; // 1 MiB
            let zeros = vec![0u8; CHUNK.min(size)];

            // Pass 1: overwrite in-place with zeros (no truncate)
            {
                let mut f = OpenOptions::new().write(true).open(path)?;
                f.seek(SeekFrom::Start(0))?;
                let mut remaining = size;
                while remaining > 0 {
                    let n = remaining.min(CHUNK);
                    f.write_all(&zeros[..n])?;
                    remaining -= n;
                }
                f.sync_all()?;
            }

            // Pass 2: overwrite in-place with random data
            {
                let mut f = OpenOptions::new().write(true).open(path)?;
                f.seek(SeekFrom::Start(0))?;
                let mut remaining = size;
                while remaining > 0 {
                    let n = remaining.min(CHUNK);
                    let random = crate::crypto::random_bytes(n);
                    f.write_all(&random)?;
                    remaining -= n;
                }
                f.sync_all()?;
            }
        }
        std::fs::remove_file(path)?;
        info!("Securely deleted: {:?}", path);
    }
    Ok(())
}
