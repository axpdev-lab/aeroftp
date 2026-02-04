// AeroFTP Secure Credential Store
// Dual-mode: OS Keyring (preferred) + Encrypted Vault fallback (Argon2id + AES-256-GCM)
// CVE-level hotfix: replaces all plaintext credential storage

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

const SERVICE_NAME: &str = "aeroftp";
const VAULT_FILENAME: &str = "vault.db";

// ============ Error Types ============

#[derive(Debug, thiserror::Error)]
pub enum CredentialError {
    #[error("Keyring error: {0}")]
    Keyring(String),
    #[error("Vault locked - master password required")]
    VaultLocked,
    #[error("Vault not initialized - setup master password first")]
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
}

// ============ Vault File Format ============

#[derive(Serialize, Deserialize)]
struct VaultFile {
    version: u32,
    salt: Vec<u8>,           // 32 bytes for Argon2id
    verify_nonce: Vec<u8>,   // 12 bytes - nonce for verification token
    verify_data: Vec<u8>,    // encrypted "aeroftp_vault_ok" for password verification
    entries: HashMap<String, VaultEntry>,
}

#[derive(Serialize, Deserialize, Clone)]
struct VaultEntry {
    nonce: Vec<u8>,   // 12 bytes
    data: Vec<u8>,    // [ciphertext][tag 16B]
}

// ============ Credential Backend ============

enum Backend {
    OsKeyring,
    EncryptedVault {
        path: PathBuf,
        master_key: [u8; 32],
        salt: Vec<u8>,
    },
}

// ============ Credential Store ============

pub struct CredentialStore {
    backend: Backend,
}

impl CredentialStore {
    /// Try to create a store using OS keyring. Returns None if keyring unavailable.
    pub fn with_keyring() -> Option<Self> {
        // Test keyring availability by trying a probe operation
        let entry = match keyring::Entry::new(SERVICE_NAME, "__probe__") {
            Ok(e) => e,
            Err(_) => return None,
        };
        // Try to read (will return NotFound which is fine, or a platform error)
        match entry.get_password() {
            Ok(_) => {},
            Err(keyring::Error::NoEntry) => {},
            Err(keyring::Error::NoStorageAccess(_)) => return None,
            Err(keyring::Error::PlatformFailure(_)) => {
                // On Windows, PlatformFailure during probe may be transient
                // (Credential Manager locked, pending Windows Hello, etc.)
                // Allow keyring backend — actual credential access may still work
                #[cfg(windows)]
                {
                    warn!("Keyring probe returned PlatformFailure, proceeding anyway (Windows transient)");
                }
                #[cfg(not(windows))]
                return None;
            },
            Err(_) => {},
        }
        Some(Self { backend: Backend::OsKeyring })
    }

    /// Check if OS keyring is available
    pub fn is_keyring_available() -> bool {
        let entry = match keyring::Entry::new(SERVICE_NAME, "__probe__") {
            Ok(e) => e,
            Err(_) => return false,
        };
        match entry.get_password() {
            Ok(_) | Err(keyring::Error::NoEntry) => true,
            // On Windows, PlatformFailure may be transient — report as available
            #[cfg(windows)]
            Err(keyring::Error::PlatformFailure(_)) => true,
            _ => false,
        }
    }

    /// Create a store using encrypted vault with existing master password
    pub fn with_vault(password: &str) -> Result<Self, CredentialError> {
        let vault_path = Self::vault_path()?;
        if !vault_path.exists() {
            return Err(CredentialError::VaultNotInitialized);
        }

        let vault_data = std::fs::read(&vault_path)?;
        let vault: VaultFile = serde_json::from_slice(&vault_data)
            .map_err(|e| CredentialError::Serialization(e.to_string()))?;

        let master_key = crate::crypto::derive_key(password, &vault.salt)
                .map_err(CredentialError::Encryption)?;

        // Verify password by decrypting verification token
        crate::crypto::decrypt_aes_gcm(&master_key, &vault.verify_nonce, &vault.verify_data)
                .map_err(CredentialError::Encryption)
            .map_err(|_| CredentialError::InvalidMasterPassword)?;

        Ok(Self {
            backend: Backend::EncryptedVault {
                path: vault_path,
                master_key,
                salt: vault.salt,
            },
        })
    }

    /// Initialize a new vault with a master password
    pub fn setup_vault(password: &str) -> Result<Self, CredentialError> {
        let vault_path = Self::vault_path()?;

        // Generate salt
        let salt = crate::crypto::random_bytes(32);
        let master_key = crate::crypto::derive_key(password, &salt)
                .map_err(CredentialError::Encryption)?;

        // Create verification token
        let verify_nonce = crate::crypto::random_bytes(12);
        let verify_data = crate::crypto::encrypt_aes_gcm(&master_key, &verify_nonce, b"aeroftp_vault_ok")
                .map_err(CredentialError::Encryption)?;

        let vault = VaultFile {
            version: 1,
            salt: salt.clone(),
            verify_nonce,
            verify_data,
            entries: HashMap::new(),
        };

        Self::write_vault(&vault_path, &vault)?;
        info!("Credential vault initialized");

        Ok(Self {
            backend: Backend::EncryptedVault {
                path: vault_path,
                master_key,
                salt,
            },
        })
    }

    /// Check if a vault file exists
    pub fn vault_exists() -> bool {
        Self::vault_path().map(|p| p.exists()).unwrap_or(false)
    }

    /// Store a credential
    pub fn store(&self, account: &str, secret: &str) -> Result<(), CredentialError> {
        match &self.backend {
            Backend::OsKeyring => {
                let entry = keyring::Entry::new(SERVICE_NAME, account)
                    .map_err(|e| CredentialError::Keyring(e.to_string()))?;
                entry.set_password(secret)
                    .map_err(|e| CredentialError::Keyring(e.to_string()))?;
                info!("Credential stored in OS keyring: {}", account);
            }
            Backend::EncryptedVault { path, master_key, .. } => {
                let mut vault = Self::read_vault(path)?;
                let nonce = crate::crypto::random_bytes(12);
                let data = crate::crypto::encrypt_aes_gcm(master_key, &nonce, secret.as_bytes())
                        .map_err(CredentialError::Encryption)?;
                vault.entries.insert(account.to_string(), VaultEntry { nonce, data });
                Self::write_vault(path, &vault)?;
                info!("Credential stored in vault: {}", account);
            }
        }
        Ok(())
    }

    /// Retrieve a credential
    pub fn get(&self, account: &str) -> Result<String, CredentialError> {
        match &self.backend {
            Backend::OsKeyring => {
                let entry = keyring::Entry::new(SERVICE_NAME, account)
                    .map_err(|e| CredentialError::Keyring(e.to_string()))?;
                entry.get_password()
                    .map_err(|e| match e {
                        keyring::Error::NoEntry => CredentialError::NotFound(account.to_string()),
                        other => CredentialError::Keyring(other.to_string()),
                    })
            }
            Backend::EncryptedVault { path, master_key, .. } => {
                let vault = Self::read_vault(path)?;
                let entry = vault.entries.get(account)
                    .ok_or_else(|| CredentialError::NotFound(account.to_string()))?;
                let plaintext = crate::crypto::decrypt_aes_gcm(master_key, &entry.nonce, &entry.data)
                        .map_err(CredentialError::Encryption)?;
                String::from_utf8(plaintext)
                    .map_err(|e| CredentialError::Encryption(e.to_string()))
            }
        }
    }

    /// Delete a credential
    pub fn delete(&self, account: &str) -> Result<(), CredentialError> {
        match &self.backend {
            Backend::OsKeyring => {
                let entry = keyring::Entry::new(SERVICE_NAME, account)
                    .map_err(|e| CredentialError::Keyring(e.to_string()))?;
                entry.delete_credential()
                    .map_err(|e| CredentialError::Keyring(e.to_string()))?;
                info!("Credential deleted from OS keyring: {}", account);
            }
            Backend::EncryptedVault { path, .. } => {
                let mut vault = Self::read_vault(path)?;
                vault.entries.remove(account);
                Self::write_vault(path, &vault)?;
                info!("Credential deleted from vault: {}", account);
            }
        }
        Ok(())
    }

    /// List all stored account names
    pub fn list_accounts(&self) -> Result<Vec<String>, CredentialError> {
        match &self.backend {
            Backend::OsKeyring => {
                // Keyring doesn't support listing; we maintain a manifest
                let manifest_key = "__aeroftp_accounts__";
                let entry = keyring::Entry::new(SERVICE_NAME, manifest_key)
                    .map_err(|e| CredentialError::Keyring(e.to_string()))?;
                match entry.get_password() {
                    Ok(json) => {
                        let accounts: Vec<String> = serde_json::from_str(&json)
                            .unwrap_or_default();
                        Ok(accounts)
                    }
                    Err(keyring::Error::NoEntry) => Ok(vec![]),
                    Err(e) => Err(CredentialError::Keyring(e.to_string())),
                }
            }
            Backend::EncryptedVault { path, .. } => {
                let vault = Self::read_vault(path)?;
                Ok(vault.entries.keys().cloned().collect())
            }
        }
    }

    /// Update the keyring account manifest (call after store/delete when using keyring)
    fn update_keyring_manifest(&self, accounts: &[String]) -> Result<(), CredentialError> {
        if let Backend::OsKeyring = &self.backend {
            let manifest_key = "__aeroftp_accounts__";
            let entry = keyring::Entry::new(SERVICE_NAME, manifest_key)
                .map_err(|e| CredentialError::Keyring(e.to_string()))?;
            let json = serde_json::to_string(accounts)
                .map_err(|e| CredentialError::Serialization(e.to_string()))?;
            entry.set_password(&json)
                .map_err(|e| CredentialError::Keyring(e.to_string()))?;
        }
        Ok(())
    }

    /// Store credential and update manifest (keyring-aware)
    pub fn store_and_track(&self, account: &str, secret: &str) -> Result<(), CredentialError> {
        self.store(account, secret)?;
        if let Backend::OsKeyring = &self.backend {
            let mut accounts = self.list_accounts().unwrap_or_default();
            if !accounts.contains(&account.to_string()) {
                accounts.push(account.to_string());
                self.update_keyring_manifest(&accounts)?;
            }
        }
        Ok(())
    }

    /// Delete credential and update manifest (keyring-aware)
    pub fn delete_and_track(&self, account: &str) -> Result<(), CredentialError> {
        self.delete(account)?;
        if let Backend::OsKeyring = &self.backend {
            let mut accounts = self.list_accounts().unwrap_or_default();
            accounts.retain(|a| a != account);
            self.update_keyring_manifest(&accounts)?;
        }
        Ok(())
    }

    // ============ Internal Helpers ============

    fn vault_path() -> Result<PathBuf, CredentialError> {
        let base = dirs::config_dir()
            .or_else(|| dirs::home_dir())
            .ok_or_else(|| CredentialError::Io(
                std::io::Error::new(std::io::ErrorKind::NotFound, "No config directory")
            ))?;
        let dir = base.join("aeroftp");
        if !dir.exists() {
            std::fs::create_dir_all(&dir)?;
            ensure_secure_permissions(&dir)?;
        }
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
        std::fs::write(path, &data)?;
        ensure_secure_permissions(path)?;
        Ok(())
    }

}

// ============ Permission Hardening ============

/// Ensure secure file/directory permissions (0o600 files, 0o700 dirs on Unix; ACL on Windows)
pub fn ensure_secure_permissions(path: &Path) -> Result<(), CredentialError> {
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
pub fn harden_config_directory() -> Result<(), CredentialError> {
    let base = dirs::config_dir()
        .or_else(|| dirs::home_dir())
        .ok_or_else(|| CredentialError::Io(
            std::io::Error::new(std::io::ErrorKind::NotFound, "No config directory")
        ))?;
    let aeroftp_dir = base.join("aeroftp");
    if aeroftp_dir.exists() {
        ensure_secure_permissions(&aeroftp_dir)?;
        // Harden all files in the directory
        if let Ok(entries) = std::fs::read_dir(&aeroftp_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                ensure_secure_permissions(&path)?;
                // Also harden subdirectories (e.g. oauth_tokens/)
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

/// Securely delete a file (overwrite with zeros, then remove)
pub fn secure_delete(path: &Path) -> Result<(), CredentialError> {
    if path.exists() {
        let size = std::fs::metadata(path)?.len();
        if size > 0 {
            let zeros = vec![0u8; size as usize];
            std::fs::write(path, &zeros)?;
            // Second pass with random data
            let random = crate::crypto::random_bytes(size as usize);
            std::fs::write(path, &random)?;
        }
        std::fs::remove_file(path)?;
        info!("Securely deleted: {:?}", path);
    }
    Ok(())
}

// ============ Migration ============

#[derive(Serialize)]
pub struct MigrationResult {
    pub migrated_count: u32,
    pub errors: Vec<String>,
    pub old_file_deleted: bool,
}

/// Migrate plaintext credentials from server_credentials.json to the credential store
pub fn migrate_server_credentials(store: &CredentialStore) -> Result<MigrationResult, CredentialError> {
    let creds_path = dirs::config_dir()
        .or_else(|| dirs::home_dir())
        .unwrap_or_else(|| PathBuf::from("."))
        .join("aeroftp")
        .join("server_credentials.json");

    let mut result = MigrationResult {
        migrated_count: 0,
        errors: vec![],
        old_file_deleted: false,
    };

    // Also check for interrupted previous migration
    let migrating_path = creds_path.with_extension("json.migrating");
    if migrating_path.exists() && !creds_path.exists() {
        // Previous migration completed but cleanup failed - just delete
        let _ = secure_delete(&migrating_path);
        result.old_file_deleted = true;
        return Ok(result);
    }

    if !creds_path.exists() {
        return Ok(result);
    }

    // Fix permissions first
    let _ = ensure_secure_permissions(&creds_path);

    #[derive(Deserialize)]
    struct OldCreds {
        server: String,
        username: String,
        password: String,
    }

    let content = std::fs::read_to_string(&creds_path)?;
    let creds_map: HashMap<String, OldCreds> = serde_json::from_str(&content)
        .map_err(|e| CredentialError::Serialization(e.to_string()))?;

    for (profile_name, creds) in &creds_map {
        // Store as JSON blob with server+username+password
        let value = serde_json::json!({
            "server": creds.server,
            "username": creds.username,
            "password": creds.password,
        });
        let value_str = value.to_string();
        match store.store_and_track(&format!("server_{}", profile_name), &value_str) {
            Ok(_) => result.migrated_count += 1,
            Err(e) => result.errors.push(format!("{}: {}", profile_name, e)),
        }
    }

    // Atomic delete: rename to .migrating first, then secure-delete
    if result.errors.is_empty() {
        let migrating_path = creds_path.with_extension("json.migrating");
        match std::fs::rename(&creds_path, &migrating_path) {
            Ok(_) => {
                match secure_delete(&migrating_path) {
                    Ok(_) => result.old_file_deleted = true,
                    Err(e) => result.errors.push(format!("Failed to delete migrated file: {}", e)),
                }
            }
            Err(e) => result.errors.push(format!("Failed to rename for migration: {}", e)),
        }
    }

    Ok(result)
}

/// Migrate OAuth tokens from JSON files to credential store
pub fn migrate_oauth_tokens(store: &CredentialStore) -> Result<u32, CredentialError> {
    let base = match dirs::config_dir() {
        Some(d) => d,
        None => return Ok(0),
    };
    let token_dir = base.join("aeroftp").join("oauth_tokens");
    if !token_dir.exists() {
        return Ok(0);
    }

    let mut count = 0;
    let entries = std::fs::read_dir(&token_dir)?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().map(|e| e == "json").unwrap_or(false) {
            if let Ok(json) = std::fs::read_to_string(&path) {
                // Extract provider name from filename: oauth2_google.json -> oauth_google
                let stem = path.file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                let account = stem.replace("oauth2_", "oauth_");
                match store.store_and_track(&account, &json) {
                    Ok(_) => {
                        let _ = secure_delete(&path);
                        count += 1;
                    }
                    Err(e) => {
                        warn!("Failed to migrate OAuth token {}: {}", stem, e);
                    }
                }
            }
        }
    }

    // Remove empty directory
    if std::fs::read_dir(&token_dir).map(|mut d| d.next().is_none()).unwrap_or(false) {
        let _ = std::fs::remove_dir(&token_dir);
    }

    Ok(count)
}
