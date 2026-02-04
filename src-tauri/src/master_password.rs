// AeroFTP Master Password System
// Military-grade security: Argon2id 128 MiB + AES-256-GCM + Auto-lock
//
// Provides app-level encryption with:
// - Argon2id key derivation (128 MiB, t=4, p=4) — OWASP 2024 high-security
// - AES-256-GCM authenticated encryption
// - HMAC-SHA512 integrity verification
// - Auto-lock timeout with activity tracking
// - Secure memory zeroization on lock
//
// v1.8.1 — February 2026

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;
use aes_gcm::{Aes256Gcm, KeyInit, aead::Aead};
use aes_gcm::aead::generic_array::GenericArray;
use argon2::Argon2;
use hmac::{Hmac, Mac};
use sha2::Sha512;
use serde::Serialize;
use tracing::info;
use secrecy::zeroize::Zeroize;

// ============ Constants ============

const MASTER_FILENAME: &str = "master_password.dat";
const MAGIC_BYTES: &[u8; 10] = b"AEROMASTER";
const VERSION: u8 = 1;
const VERIFY_TOKEN: &[u8] = b"aeroftp_master_v1_ok";

// Argon2id parameters — OWASP 2024 high-security (same as AeroVault v2)
const ARGON2_MEM_COST: u32 = 131072;  // 128 MiB
const ARGON2_TIME_COST: u32 = 4;
const ARGON2_PARALLELISM: u32 = 4;

// Header sizes
const SALT_SIZE: usize = 32;
const NONCE_SIZE: usize = 12;
const HMAC_SIZE: usize = 64;  // SHA-512

// ============ Error Types ============

#[derive(Debug, thiserror::Error)]
pub enum MasterPasswordError {
    #[error("Master password not set")]
    NotSet,
    #[error("App is locked - master password required")]
    Locked,
    #[error("Invalid master password")]
    InvalidPassword,
    #[error("Master password already set")]
    AlreadySet,
    #[error("Encryption error: {0}")]
    Encryption(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Invalid file format")]
    InvalidFormat,
    #[error("HMAC verification failed - file may be corrupted")]
    IntegrityError,
}

// ============ File Format ============

/// Master password file format (binary)
///
/// | Field          | Size    | Description                              |
/// |----------------|---------|------------------------------------------|
/// | magic          | 10      | "AEROMASTER"                             |
/// | version        | 1       | Format version (1)                       |
/// | flags          | 1       | Reserved for future use                  |
/// | salt           | 32      | Argon2id salt                            |
/// | verify_nonce   | 12      | Nonce for verification token             |
/// | verify_data    | var     | AES-GCM encrypted verification token     |
/// | timeout        | 4       | Auto-lock timeout in seconds (0=disabled)|
/// | hmac           | 64      | HMAC-SHA512 of all preceding bytes       |
#[derive(Clone)]
struct MasterFile {
    salt: [u8; SALT_SIZE],
    verify_nonce: [u8; NONCE_SIZE],
    verify_data: Vec<u8>,  // Encrypted VERIFY_TOKEN
    timeout_seconds: u32,
}

impl MasterFile {
    fn to_bytes(&self, hmac_key: &[u8; 32]) -> Vec<u8> {
        let mut data = Vec::with_capacity(128);

        // Header
        data.extend_from_slice(MAGIC_BYTES);
        data.push(VERSION);
        data.push(0); // flags reserved

        // Cryptographic material
        data.extend_from_slice(&self.salt);
        data.extend_from_slice(&self.verify_nonce);

        // Variable-length verify data (prefixed with length)
        let vd_len = self.verify_data.len() as u16;
        data.extend_from_slice(&vd_len.to_le_bytes());
        data.extend_from_slice(&self.verify_data);

        // Timeout
        data.extend_from_slice(&self.timeout_seconds.to_le_bytes());

        // HMAC-SHA512 integrity
        let mut mac = <Hmac<Sha512> as Mac>::new_from_slice(hmac_key)
            .expect("HMAC can take key of any size");
        mac.update(&data);
        let hmac_result = mac.finalize().into_bytes();
        data.extend_from_slice(&hmac_result);

        data
    }

    fn from_bytes(data: &[u8], hmac_key: &[u8; 32]) -> Result<Self, MasterPasswordError> {
        // Minimum size: magic(10) + version(1) + flags(1) + salt(32) + nonce(12) + vd_len(2) + vd(16+16) + timeout(4) + hmac(64)
        if data.len() < 10 + 1 + 1 + 32 + 12 + 2 + 32 + 4 + 64 {
            return Err(MasterPasswordError::InvalidFormat);
        }

        // Verify magic
        if &data[0..10] != MAGIC_BYTES {
            return Err(MasterPasswordError::InvalidFormat);
        }

        // Verify version
        if data[10] != VERSION {
            return Err(MasterPasswordError::InvalidFormat);
        }

        // Verify HMAC first (before parsing)
        let hmac_start = data.len() - HMAC_SIZE;
        let stored_hmac = &data[hmac_start..];
        let payload = &data[..hmac_start];

        let mut mac = <Hmac<Sha512> as Mac>::new_from_slice(hmac_key)
            .expect("HMAC can take key of any size");
        mac.update(payload);
        mac.verify_slice(stored_hmac)
            .map_err(|_| MasterPasswordError::IntegrityError)?;

        // Parse fields
        let mut offset = 12; // After magic + version + flags

        let mut salt = [0u8; SALT_SIZE];
        salt.copy_from_slice(&data[offset..offset + SALT_SIZE]);
        offset += SALT_SIZE;

        let mut verify_nonce = [0u8; NONCE_SIZE];
        verify_nonce.copy_from_slice(&data[offset..offset + NONCE_SIZE]);
        offset += NONCE_SIZE;

        let vd_len = u16::from_le_bytes([data[offset], data[offset + 1]]) as usize;
        offset += 2;

        if offset + vd_len + 4 + HMAC_SIZE != data.len() {
            return Err(MasterPasswordError::InvalidFormat);
        }

        let verify_data = data[offset..offset + vd_len].to_vec();
        offset += vd_len;

        let timeout_seconds = u32::from_le_bytes([
            data[offset], data[offset + 1], data[offset + 2], data[offset + 3]
        ]);

        Ok(Self {
            salt,
            verify_nonce,
            verify_data,
            timeout_seconds,
        })
    }
}

// ============ Global State ============

/// Thread-safe global state for master password
pub struct MasterPasswordState {
    /// Whether the app is currently locked
    locked: AtomicBool,
    /// Timestamp of last activity (for auto-lock)
    last_activity_ms: AtomicU64,
    /// In-memory master key (zeroized on lock)
    master_key: tokio::sync::Mutex<Option<[u8; 32]>>,
    /// In-memory HMAC key (zeroized on lock)
    hmac_key: tokio::sync::Mutex<Option<[u8; 32]>>,
    /// Auto-lock timeout in seconds (0 = disabled)
    timeout_seconds: AtomicU64,
    /// Start time for activity tracking
    start_instant: Instant,
}

impl MasterPasswordState {
    pub fn new() -> Self {
        Self {
            locked: AtomicBool::new(true),
            last_activity_ms: AtomicU64::new(0),
            master_key: tokio::sync::Mutex::new(None),
            hmac_key: tokio::sync::Mutex::new(None),
            timeout_seconds: AtomicU64::new(0),
            start_instant: Instant::now(),
        }
    }

    /// Update last activity timestamp (call on user interaction)
    pub fn update_activity(&self) {
        let now = self.start_instant.elapsed().as_millis() as u64;
        self.last_activity_ms.store(now, Ordering::SeqCst);
    }

    /// Check if auto-lock timeout has expired
    pub fn check_timeout(&self) -> bool {
        let timeout = self.timeout_seconds.load(Ordering::SeqCst);
        if timeout == 0 {
            return false; // Auto-lock disabled
        }

        let last = self.last_activity_ms.load(Ordering::SeqCst);
        let now = self.start_instant.elapsed().as_millis() as u64;
        let elapsed_secs = (now.saturating_sub(last)) / 1000;

        elapsed_secs >= timeout
    }

    /// Lock the app and zeroize keys
    pub async fn lock(&self) {
        self.locked.store(true, Ordering::SeqCst);

        // Zeroize master key
        if let Some(ref mut key) = *self.master_key.lock().await {
            key.zeroize();
        }
        *self.master_key.lock().await = None;

        // Zeroize HMAC key
        if let Some(ref mut key) = *self.hmac_key.lock().await {
            key.zeroize();
        }
        *self.hmac_key.lock().await = None;

        info!("App locked - master key zeroized");
    }

    /// Unlock with password
    pub async fn unlock(&self, password: &str) -> Result<(), MasterPasswordError> {
        let file = Self::read_master_file(password).await?;

        // Derive keys
        let master_key = derive_key_strong(password, &file.salt)?;
        let hmac_key = derive_hmac_key(&master_key, &file.salt);

        // Verify password by decrypting token
        let cipher = Aes256Gcm::new(GenericArray::from_slice(&master_key));
        let nonce = GenericArray::from_slice(&file.verify_nonce);
        let decrypted = cipher.decrypt(nonce, file.verify_data.as_slice())
            .map_err(|_| MasterPasswordError::InvalidPassword)?;

        if decrypted != VERIFY_TOKEN {
            return Err(MasterPasswordError::InvalidPassword);
        }

        // Store keys and unlock
        *self.master_key.lock().await = Some(master_key);
        *self.hmac_key.lock().await = Some(hmac_key);
        self.timeout_seconds.store(file.timeout_seconds as u64, Ordering::SeqCst);
        self.locked.store(false, Ordering::SeqCst);
        self.update_activity();

        info!("App unlocked successfully");
        Ok(())
    }

    /// Check if locked
    pub fn is_locked(&self) -> bool {
        self.locked.load(Ordering::SeqCst)
    }

    /// Get current timeout setting
    pub fn get_timeout(&self) -> u64 {
        self.timeout_seconds.load(Ordering::SeqCst)
    }

    /// Read master file (requires password for HMAC verification)
    async fn read_master_file(password: &str) -> Result<MasterFile, MasterPasswordError> {
        let path = master_file_path()?;
        if !path.exists() {
            return Err(MasterPasswordError::NotSet);
        }

        let data = tokio::fs::read(&path).await?;

        // We need to derive keys to verify HMAC
        // First, extract salt without HMAC verification
        if data.len() < 12 + SALT_SIZE {
            return Err(MasterPasswordError::InvalidFormat);
        }

        let mut salt = [0u8; SALT_SIZE];
        salt.copy_from_slice(&data[12..12 + SALT_SIZE]);

        // Derive keys
        let master_key = derive_key_strong(password, &salt)?;
        let hmac_key = derive_hmac_key(&master_key, &salt);

        // Now parse with HMAC verification
        MasterFile::from_bytes(&data, &hmac_key)
    }
}

// ============ Key Derivation ============

/// Derive master key using Argon2id with strong parameters
fn derive_key_strong(password: &str, salt: &[u8]) -> Result<[u8; 32], MasterPasswordError> {
    let params = argon2::Params::new(
        ARGON2_MEM_COST,
        ARGON2_TIME_COST,
        ARGON2_PARALLELISM,
        Some(32),
    ).map_err(|e| MasterPasswordError::Encryption(format!("Argon2 params: {}", e)))?;

    let argon2 = Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);

    let mut key = [0u8; 32];
    argon2.hash_password_into(password.as_bytes(), salt, &mut key)
        .map_err(|e| MasterPasswordError::Encryption(format!("Argon2 derive: {}", e)))?;

    Ok(key)
}

/// Derive HMAC key from master key using HKDF-like expansion
fn derive_hmac_key(master_key: &[u8; 32], salt: &[u8]) -> [u8; 32] {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(master_key);
    hasher.update(salt);
    hasher.update(b"aeroftp_hmac_key_v1");
    let result = hasher.finalize();
    let mut key = [0u8; 32];
    key.copy_from_slice(&result);
    key
}

/// Generate cryptographically secure random bytes
fn random_bytes<const N: usize>() -> [u8; N] {
    use rand::RngCore;
    let mut buf = [0u8; N];
    rand::thread_rng().fill_bytes(&mut buf);
    buf
}

// ============ File Operations ============

fn master_file_path() -> Result<PathBuf, MasterPasswordError> {
    let base = dirs::config_dir()
        .or_else(|| dirs::home_dir())
        .ok_or_else(|| MasterPasswordError::Io(
            std::io::Error::new(std::io::ErrorKind::NotFound, "No config directory")
        ))?;
    let dir = base.join("aeroftp");
    if !dir.exists() {
        std::fs::create_dir_all(&dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        }
        #[cfg(windows)]
        {
            crate::windows_acl::restrict_to_owner(&dir);
        }
    }
    Ok(dir.join(MASTER_FILENAME))
}

// ============ Public API ============

/// Check if master password is configured
pub fn is_master_password_set() -> bool {
    master_file_path()
        .map(|p| p.exists())
        .unwrap_or(false)
}

/// Set up a new master password
pub async fn setup_master_password(
    password: &str,
    timeout_seconds: u32,
    state: &MasterPasswordState,
) -> Result<(), MasterPasswordError> {
    if is_master_password_set() {
        return Err(MasterPasswordError::AlreadySet);
    }

    // Validate password strength
    if password.len() < 8 {
        return Err(MasterPasswordError::Encryption(
            "Password must be at least 8 characters".to_string()
        ));
    }

    // Generate salt
    let salt: [u8; SALT_SIZE] = random_bytes();

    // Derive keys
    let master_key = derive_key_strong(password, &salt)?;
    let hmac_key = derive_hmac_key(&master_key, &salt);

    // Encrypt verification token
    let verify_nonce: [u8; NONCE_SIZE] = random_bytes();
    let cipher = Aes256Gcm::new(GenericArray::from_slice(&master_key));
    let nonce = GenericArray::from_slice(&verify_nonce);
    let verify_data = cipher.encrypt(nonce, VERIFY_TOKEN)
        .map_err(|e| MasterPasswordError::Encryption(format!("AES-GCM encrypt: {}", e)))?;

    // Create and write file
    let file = MasterFile {
        salt,
        verify_nonce,
        verify_data,
        timeout_seconds,
    };

    let path = master_file_path()?;
    let data = file.to_bytes(&hmac_key);
    tokio::fs::write(&path, &data).await?;

    // Secure permissions
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(windows)]
    {
        crate::windows_acl::restrict_to_owner(&path);
    }

    // Unlock state
    *state.master_key.lock().await = Some(master_key);
    *state.hmac_key.lock().await = Some(hmac_key);
    state.timeout_seconds.store(timeout_seconds as u64, Ordering::SeqCst);
    state.locked.store(false, Ordering::SeqCst);
    state.update_activity();

    info!("Master password configured with {}s timeout", timeout_seconds);
    Ok(())
}

/// Change master password (requires current password)
pub async fn change_master_password(
    old_password: &str,
    new_password: &str,
    new_timeout: Option<u32>,
    state: &MasterPasswordState,
) -> Result<(), MasterPasswordError> {
    // Verify old password
    let old_file = MasterPasswordState::read_master_file(old_password).await?;

    // Derive old keys and verify
    let old_master_key = derive_key_strong(old_password, &old_file.salt)?;
    let cipher = Aes256Gcm::new(GenericArray::from_slice(&old_master_key));
    let nonce = GenericArray::from_slice(&old_file.verify_nonce);
    cipher.decrypt(nonce, old_file.verify_data.as_slice())
        .map_err(|_| MasterPasswordError::InvalidPassword)?;

    // Validate new password
    if new_password.len() < 8 {
        return Err(MasterPasswordError::Encryption(
            "Password must be at least 8 characters".to_string()
        ));
    }

    // Generate new salt
    let new_salt: [u8; SALT_SIZE] = random_bytes();

    // Derive new keys
    let new_master_key = derive_key_strong(new_password, &new_salt)?;
    let new_hmac_key = derive_hmac_key(&new_master_key, &new_salt);

    // Encrypt new verification token
    let new_verify_nonce: [u8; NONCE_SIZE] = random_bytes();
    let cipher = Aes256Gcm::new(GenericArray::from_slice(&new_master_key));
    let nonce = GenericArray::from_slice(&new_verify_nonce);
    let new_verify_data = cipher.encrypt(nonce, VERIFY_TOKEN)
        .map_err(|e| MasterPasswordError::Encryption(format!("AES-GCM encrypt: {}", e)))?;

    // Create new file
    let timeout = new_timeout.unwrap_or(old_file.timeout_seconds);
    let new_file = MasterFile {
        salt: new_salt,
        verify_nonce: new_verify_nonce,
        verify_data: new_verify_data,
        timeout_seconds: timeout,
    };

    // Atomic write: write to temp, then rename
    let path = master_file_path()?;
    let temp_path = path.with_extension("dat.new");
    let data = new_file.to_bytes(&new_hmac_key);
    tokio::fs::write(&temp_path, &data).await?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temp_path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(windows)]
    {
        crate::windows_acl::restrict_to_owner(&temp_path);
    }

    tokio::fs::rename(&temp_path, &path).await?;

    // Update state
    *state.master_key.lock().await = Some(new_master_key);
    *state.hmac_key.lock().await = Some(new_hmac_key);
    state.timeout_seconds.store(timeout as u64, Ordering::SeqCst);
    state.update_activity();

    info!("Master password changed successfully");
    Ok(())
}

/// Remove master password protection (requires current password)
pub async fn remove_master_password(
    password: &str,
    state: &MasterPasswordState,
) -> Result<(), MasterPasswordError> {
    // Verify password first
    let file = MasterPasswordState::read_master_file(password).await?;
    let master_key = derive_key_strong(password, &file.salt)?;

    let cipher = Aes256Gcm::new(GenericArray::from_slice(&master_key));
    let nonce = GenericArray::from_slice(&file.verify_nonce);
    cipher.decrypt(nonce, file.verify_data.as_slice())
        .map_err(|_| MasterPasswordError::InvalidPassword)?;

    // Secure delete the file
    let path = master_file_path()?;
    if path.exists() {
        // Overwrite with random data before deletion
        let size = tokio::fs::metadata(&path).await?.len();
        let random_data: Vec<u8> = (0..size).map(|_| rand::random()).collect();
        tokio::fs::write(&path, &random_data).await?;
        tokio::fs::remove_file(&path).await?;
    }

    // Clear state
    state.lock().await;

    info!("Master password removed");
    Ok(())
}

/// Update auto-lock timeout (requires unlocked state)
pub async fn update_timeout(
    timeout_seconds: u32,
    password: &str,
    state: &MasterPasswordState,
) -> Result<(), MasterPasswordError> {
    if state.is_locked() {
        return Err(MasterPasswordError::Locked);
    }

    // Read current file
    let mut file = MasterPasswordState::read_master_file(password).await?;
    file.timeout_seconds = timeout_seconds;

    // Get HMAC key for writing
    let hmac_key = state.hmac_key.lock().await
        .ok_or(MasterPasswordError::Locked)?;

    // Write updated file
    let path = master_file_path()?;
    let data = file.to_bytes(&hmac_key);
    tokio::fs::write(&path, &data).await?;

    // Update state
    state.timeout_seconds.store(timeout_seconds as u64, Ordering::SeqCst);

    info!("Auto-lock timeout updated to {}s", timeout_seconds);
    Ok(())
}

// ============ Status Response ============

#[derive(Serialize)]
pub struct MasterPasswordStatus {
    pub is_set: bool,
    pub is_locked: bool,
    pub timeout_seconds: u64,
    pub last_activity_ago_seconds: u64,
}

impl MasterPasswordStatus {
    pub fn new(state: &MasterPasswordState) -> Self {
        let last = state.last_activity_ms.load(Ordering::SeqCst);
        let now = state.start_instant.elapsed().as_millis() as u64;
        let ago = (now.saturating_sub(last)) / 1000;

        Self {
            is_set: is_master_password_set(),
            is_locked: state.is_locked(),
            timeout_seconds: state.get_timeout(),
            last_activity_ago_seconds: ago,
        }
    }
}
