// AeroFTP Full Keystore Export/Import
// Exports ALL vault entries as encrypted .aeroftp-keystore file
// Uses Argon2id + AES-256-GCM (same as profile_export)

use std::collections::HashMap;
use std::path::Path;
use serde::{Deserialize, Serialize};

const FILE_VERSION: u32 = 1;

// ============ Error Types ============

#[derive(Debug, thiserror::Error)]
pub enum KeystoreExportError {
    #[error("Invalid password")]
    InvalidPassword,
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("Encryption error: {0}")]
    Encryption(String),
    #[error("Unsupported file version: {0}")]
    UnsupportedVersion(u32),
    #[error("Vault not ready")]
    VaultNotReady,
}

// ============ File Format ============

#[derive(Serialize, Deserialize)]
struct KeystoreExportFile {
    version: u32,
    salt: Vec<u8>,
    nonce: Vec<u8>,
    encrypted_payload: Vec<u8>,
    metadata: KeystoreMetadata,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct KeystoreMetadata {
    pub export_date: String,
    pub aeroftp_version: String,
    pub entries_count: u32,
    pub categories: KeystoreCategories,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct KeystoreCategories {
    pub server_credentials: u32,
    pub server_profiles: u32,
    pub ai_keys: u32,
    pub oauth_tokens: u32,
    pub config_entries: u32,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct KeystoreImportResult {
    pub imported: u32,
    pub skipped: u32,
    pub total: u32,
}

// ============ Categorization ============

/// Categorize a vault account name into its logical group
fn categorize_account(name: &str) -> &'static str {
    if name.starts_with("server_") && !name.starts_with("server_profile_") {
        "server_credentials"
    } else if name.starts_with("server_profile_") || name.starts_with("config_server") {
        "server_profiles"
    } else if name.starts_with("ai_apikey_") {
        "ai_keys"
    } else if name.starts_with("oauth_") {
        "oauth_tokens"
    } else {
        "config_entries"
    }
}

fn count_categories(accounts: &[String]) -> KeystoreCategories {
    let mut cats = KeystoreCategories {
        server_credentials: 0,
        server_profiles: 0,
        ai_keys: 0,
        oauth_tokens: 0,
        config_entries: 0,
    };
    for name in accounts {
        match categorize_account(name) {
            "server_credentials" => cats.server_credentials += 1,
            "server_profiles" => cats.server_profiles += 1,
            "ai_keys" => cats.ai_keys += 1,
            "oauth_tokens" => cats.oauth_tokens += 1,
            _ => cats.config_entries += 1,
        }
    }
    cats
}

// ============ Export/Import ============

/// Export all vault entries to an encrypted file
pub fn export_keystore(
    password: &str,
    file_path: &Path,
) -> Result<KeystoreMetadata, KeystoreExportError> {
    let store = crate::credential_store::CredentialStore::from_cache()
        .ok_or(KeystoreExportError::VaultNotReady)?;

    // List all accounts and read their values
    let accounts = store.list_accounts()
        .map_err(|e| KeystoreExportError::Encryption(e.to_string()))?;

    let mut entries: HashMap<String, String> = HashMap::new();
    for account in &accounts {
        if let Ok(value) = store.get(account) {
            entries.insert(account.clone(), value);
        }
    }

    let categories = count_categories(&accounts);
    let metadata = KeystoreMetadata {
        export_date: chrono::Utc::now().to_rfc3339(),
        aeroftp_version: env!("CARGO_PKG_VERSION").to_string(),
        entries_count: entries.len() as u32,
        categories,
    };

    // Serialize entries to JSON
    let payload_json = serde_json::to_vec(&entries)?;

    // Encrypt with Argon2id + AES-256-GCM
    let salt = crate::crypto::random_bytes(32);
    let key = crate::crypto::derive_key(password, &salt)
        .map_err(KeystoreExportError::Encryption)?;
    let nonce = crate::crypto::random_bytes(12);
    let encrypted = crate::crypto::encrypt_aes_gcm(&key, &nonce, &payload_json)
        .map_err(KeystoreExportError::Encryption)?;

    let export_file = KeystoreExportFile {
        version: FILE_VERSION,
        salt,
        nonce,
        encrypted_payload: encrypted,
        metadata: metadata.clone(),
    };

    let file_data = serde_json::to_vec_pretty(&export_file)?;
    std::fs::write(file_path, file_data)?;

    tracing::info!("Keystore exported: {} entries to {:?}", entries.len(), file_path);
    Ok(metadata)
}

/// Import vault entries from an encrypted file
pub fn import_keystore(
    password: &str,
    file_path: &Path,
    merge_strategy: &str,
) -> Result<KeystoreImportResult, KeystoreExportError> {
    let store = crate::credential_store::CredentialStore::from_cache()
        .ok_or(KeystoreExportError::VaultNotReady)?;

    // Read and parse file
    let file_data = std::fs::read(file_path)?;
    let export_file: KeystoreExportFile = serde_json::from_slice(&file_data)?;

    if export_file.version > FILE_VERSION {
        return Err(KeystoreExportError::UnsupportedVersion(export_file.version));
    }

    // Decrypt
    let key = crate::crypto::derive_key(password, &export_file.salt)
        .map_err(KeystoreExportError::Encryption)?;
    let payload_json = crate::crypto::decrypt_aes_gcm(&key, &export_file.nonce, &export_file.encrypted_payload)
        .map_err(|_| KeystoreExportError::InvalidPassword)?;

    let entries: HashMap<String, String> = serde_json::from_slice(&payload_json)?;

    // Get existing accounts for merge strategy
    let existing = if merge_strategy == "skip_existing" {
        store.list_accounts()
            .map_err(|e| KeystoreExportError::Encryption(e.to_string()))?
            .into_iter()
            .collect::<std::collections::HashSet<_>>()
    } else {
        std::collections::HashSet::new()
    };

    let mut imported = 0u32;
    let mut skipped = 0u32;
    let total = entries.len() as u32;

    for (account, value) in &entries {
        if merge_strategy == "skip_existing" && existing.contains(account) {
            skipped += 1;
            continue;
        }
        match store.store(account, value) {
            Ok(_) => imported += 1,
            Err(e) => {
                tracing::warn!("Failed to import keystore entry '{}': {}", account, e);
                skipped += 1;
            }
        }
    }

    tracing::info!("Keystore imported: {} entries ({} skipped) from {:?}", imported, skipped, file_path);
    Ok(KeystoreImportResult { imported, skipped, total })
}

/// Read export file metadata without decrypting
pub fn read_keystore_metadata(file_path: &Path) -> Result<KeystoreMetadata, KeystoreExportError> {
    let file_data = std::fs::read(file_path)?;
    let export_file: KeystoreExportFile = serde_json::from_slice(&file_data)?;
    Ok(export_file.metadata)
}
