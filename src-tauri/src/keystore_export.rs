// AeroFTP Full Keystore Export/Import
// Exports ALL vault entries as encrypted .aeroftp-keystore file
// Uses Argon2id + AES-256-GCM (same as profile_export)

use std::collections::{HashMap, HashSet};
use std::path::Path;
use serde::{Deserialize, Serialize};

const FILE_VERSION: u32 = 1;

/// A2-01: fsync a file and its parent directory for crash durability
fn fsync_file_and_parent(file_path: &std::path::Path) -> Result<(), std::io::Error> {
    let f = std::fs::File::open(file_path)?;
    f.sync_all()?;
    if let Some(parent) = file_path.parent() {
        if let Ok(dir) = std::fs::File::open(parent) {
            dir.sync_all()?;
        }
    }
    Ok(())
}

fn normalize_merge_strategy(merge_strategy: &str) -> Result<&'static str, KeystoreExportError> {
    match merge_strategy {
        "skip" | "skip_existing" => Ok("skip_existing"),
        "overwrite" | "overwrite_all" => Ok("overwrite"),
        other => Err(KeystoreExportError::Encryption(format!("Invalid merge strategy: {}", other))),
    }
}

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
    // A2-05: Backend password minimum length check
    if password.len() < 8 {
        return Err(KeystoreExportError::Encryption("Password must be at least 8 characters".into()));
    }
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

    // A2-06: Encrypt with Argon2id (128 MiB, same strength as vault) + AES-256-GCM
    let salt = crate::crypto::random_bytes(32);
    let key = crate::crypto::derive_key_strong(password, &salt)
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
    // A2-08: Atomic write (temp+rename) + secure permissions
    let tmp_path = file_path.with_extension("tmp");
    std::fs::write(&tmp_path, file_data)?;
    // A2-01: fsync temp file before rename, then fsync parent dir after rename
    fsync_file_and_parent(&tmp_path)?;
    std::fs::rename(&tmp_path, file_path)?;
    fsync_file_and_parent(file_path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(file_path, std::fs::Permissions::from_mode(0o600));
    }

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

    // A2-06: Try strong KDF first (128 MiB, new exports), fall back to legacy (64 MiB) for old files
    let key_strong = crate::crypto::derive_key_strong(password, &export_file.salt)
        .map_err(KeystoreExportError::Encryption)?;
    let payload_json = match crate::crypto::decrypt_aes_gcm(&key_strong, &export_file.nonce, &export_file.encrypted_payload) {
        Ok(data) => data,
        Err(_) => {
            // Legacy fallback: file was exported with derive_key (64 MiB)
            let key_legacy = crate::crypto::derive_key(password, &export_file.salt)
                .map_err(KeystoreExportError::Encryption)?;
            crate::crypto::decrypt_aes_gcm(&key_legacy, &export_file.nonce, &export_file.encrypted_payload)
                .map_err(|_| KeystoreExportError::InvalidPassword)?
        }
    };

    let entries: HashMap<String, String> = serde_json::from_slice(&payload_json)?;
    let merge_strategy = normalize_merge_strategy(merge_strategy)?;

    // Get existing accounts for merge strategy
    let existing = if merge_strategy == "skip_existing" {
        store.list_accounts()
            .map_err(|e| KeystoreExportError::Encryption(e.to_string()))?
            .into_iter()
            .collect::<HashSet<_>>()
    } else {
        HashSet::new()
    };

    // GPT-A2-02: Stage entries first — collect what to import, then commit all-or-nothing
    let mut staged: Vec<(&String, &String)> = Vec::new();
    let mut originals: HashMap<String, Option<String>> = HashMap::new();
    let mut skipped = 0u32;
    let total = entries.len() as u32;

    for (account, value) in &entries {
        if merge_strategy == "skip_existing" && existing.contains(account) {
            skipped += 1;
            continue;
        }
        let original = match store.get(account) {
            Ok(existing_value) => Some(existing_value),
            Err(crate::credential_store::CredentialError::NotFound(_)) => None,
            Err(e) => return Err(KeystoreExportError::Encryption(e.to_string())),
        };
        originals.insert(account.clone(), original);
        staged.push((account, value));
    }

    // Commit phase: write all staged entries, rollback on first failure
    let mut committed: Vec<String> = Vec::new();
    for (account, value) in &staged {
        match store.store(account, value) {
            Ok(_) => committed.push((*account).clone()),
            Err(e) => {
                tracing::error!("Failed to import keystore entry '{}': {} — rolling back {} committed entries", account, e, committed.len());
                // Rollback: restore prior values for overwrites, delete only newly inserted entries
                for rollback_account in committed.iter().rev() {
                    let rollback_result = match originals.get(rollback_account) {
                        Some(Some(previous_value)) => store.store(rollback_account, previous_value),
                        Some(None) => store.delete(rollback_account),
                        None => Ok(()),
                    };
                    if let Err(re) = rollback_result {
                        tracing::warn!("Rollback failed for '{}': {}", rollback_account, re);
                    }
                }
                return Err(KeystoreExportError::Encryption(
                    format!("Import failed at '{}': {}. {} entries rolled back.", account, e, committed.len())
                ));
            }
        }
    }

    let imported = committed.len() as u32;
    tracing::info!("Keystore imported: {} entries ({} skipped) from {:?}", imported, skipped, file_path);
    Ok(KeystoreImportResult { imported, skipped, total })
}

/// Read export file metadata without decrypting
pub fn read_keystore_metadata(file_path: &Path) -> Result<KeystoreMetadata, KeystoreExportError> {
    let file_data = std::fs::read(file_path)?;
    let export_file: KeystoreExportFile = serde_json::from_slice(&file_data)?;
    Ok(export_file.metadata)
}
