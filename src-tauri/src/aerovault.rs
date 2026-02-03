//! AeroVault — Encrypted virtual folders
//!
//! An `.aerovault` file is an AES-256 encrypted ZIP containing user files
//! plus a `__aerovault_meta.json` metadata entry. Reuses the `zip` crate
//! and `archive_browse` listing infrastructure.

use serde::{Deserialize, Serialize};
use secrecy::{ExposeSecret, SecretString};
use std::fs::File;
use std::io::{Read, Write};
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

const META_ENTRY: &str = "__aerovault_meta.json";

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AeroVaultMeta {
    pub version: u32,
    pub created: String,
    pub modified: String,
    pub description: Option<String>,
    pub file_count: u32,
}

fn base_options() -> SimpleFileOptions {
    SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .compression_level(Some(6))
}

fn now_iso() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Create a new empty AeroVault
#[tauri::command]
pub async fn vault_create(
    vault_path: String,
    password: String,
    description: Option<String>,
) -> Result<String, String> {
    let pwd = SecretString::from(password);

    let file = File::create(&vault_path)
        .map_err(|e| format!("Failed to create vault: {}", e))?;
    let mut zip = ZipWriter::new(file);

    let meta = AeroVaultMeta {
        version: 1,
        created: now_iso(),
        modified: now_iso(),
        description,
        file_count: 0,
    };

    let meta_json = serde_json::to_string_pretty(&meta)
        .map_err(|e| format!("Failed to serialize metadata: {}", e))?;

    zip.start_file(META_ENTRY, base_options().with_aes_encryption(zip::AesMode::Aes256, pwd.expose_secret()))
        .map_err(|e| format!("Failed to write metadata: {}", e))?;
    zip.write_all(meta_json.as_bytes())
        .map_err(|e| format!("Failed to write metadata: {}", e))?;

    zip.finish()
        .map_err(|e| format!("Failed to finalize vault: {}", e))?;

    Ok(vault_path)
}

/// List files in an AeroVault (excluding metadata entry)
#[tauri::command]
pub async fn vault_list(
    vault_path: String,
    password: String,
) -> Result<Vec<crate::archive_browse::ArchiveEntry>, String> {
    let entries = crate::archive_browse::list_zip(vault_path, Some(password)).await?;
    Ok(entries.into_iter().filter(|e| e.name != META_ENTRY).collect())
}

/// Read vault metadata
#[tauri::command]
pub async fn vault_get_meta(
    vault_path: String,
    password: String,
) -> Result<AeroVaultMeta, String> {
    let pwd = SecretString::from(password);

    let file = File::open(&vault_path)
        .map_err(|e| format!("Failed to open vault: {}", e))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| format!("Failed to read vault: {}", e))?;

    let mut entry = archive.by_name_decrypt(META_ENTRY, pwd.expose_secret().as_bytes())
        .map_err(|e| format!("Failed to read metadata: {}", e))?;

    let mut buf = String::new();
    entry.read_to_string(&mut buf)
        .map_err(|e| format!("Failed to read metadata: {}", e))?;

    serde_json::from_str(&buf)
        .map_err(|e| format!("Invalid vault metadata: {}", e))
}

/// Add files to an existing AeroVault (rebuilds the archive)
#[tauri::command]
pub async fn vault_add_files(
    vault_path: String,
    password: String,
    file_paths: Vec<String>,
) -> Result<String, String> {
    let pwd = SecretString::from(password);

    // Read all existing entries
    let mut existing = read_all_entries(&vault_path, &pwd)?;

    // Add new files
    for path_str in &file_paths {
        let path = std::path::Path::new(path_str);
        let name = path.file_name()
            .ok_or_else(|| format!("Invalid file name: {}", path_str))?
            .to_string_lossy()
            .to_string();

        let mut f = File::open(path)
            .map_err(|e| format!("Failed to open {}: {}", path_str, e))?;
        let mut data = Vec::new();
        f.read_to_end(&mut data)
            .map_err(|e| format!("Failed to read {}: {}", path_str, e))?;

        existing.push((name, data));
    }

    // Update metadata
    let meta = update_meta_count(&existing);

    // Rebuild vault
    write_vault(&vault_path, &pwd, &existing, &meta)
}

/// Remove a file from an AeroVault (rebuilds the archive)
#[tauri::command]
pub async fn vault_remove_file(
    vault_path: String,
    password: String,
    entry_name: String,
) -> Result<String, String> {
    let pwd = SecretString::from(password);

    let mut existing = read_all_entries(&vault_path, &pwd)?;
    let before = existing.len();
    existing.retain(|(name, _)| name != &entry_name);

    if existing.len() == before {
        return Err(format!("Entry '{}' not found in vault", entry_name));
    }

    let meta = update_meta_count(&existing);
    write_vault(&vault_path, &pwd, &existing, &meta)
}

/// Extract a single file from vault
#[tauri::command]
pub async fn vault_extract_entry(
    vault_path: String,
    password: String,
    entry_name: String,
    output_path: String,
) -> Result<String, String> {
    crate::archive_browse::extract_zip_entry(vault_path, entry_name, output_path, Some(password)).await
}

/// Change vault password (decrypt all, re-encrypt with new password)
#[tauri::command]
pub async fn vault_change_password(
    vault_path: String,
    old_password: String,
    new_password: String,
) -> Result<String, String> {
    let old_pwd = SecretString::from(old_password);
    let new_pwd = SecretString::from(new_password);

    let existing = read_all_entries(&vault_path, &old_pwd)?;
    let meta = update_meta_count(&existing);
    write_vault(&vault_path, &new_pwd, &existing, &meta)
}

// ─── Internal helpers ──────────────────────────────────────────────────────────

/// Read all entries (excluding meta) from a vault into memory
fn read_all_entries(vault_path: &str, pwd: &SecretString) -> Result<Vec<(String, Vec<u8>)>, String> {
    let file = File::open(vault_path)
        .map_err(|e| format!("Failed to open vault: {}", e))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| format!("Failed to read vault: {}", e))?;

    let mut entries = Vec::new();
    for i in 0..archive.len() {
        let mut entry = archive.by_index_decrypt(i, pwd.expose_secret().as_bytes())
            .map_err(|e| format!("Failed to decrypt entry: {}", e))?;
        let name = entry.name().to_string();
        if name == META_ENTRY { continue; }

        let mut data = Vec::new();
        entry.read_to_end(&mut data)
            .map_err(|e| format!("Failed to read entry {}: {}", name, e))?;
        entries.push((name, data));
    }

    Ok(entries)
}

/// Create updated metadata with correct file count
fn update_meta_count(entries: &[(String, Vec<u8>)]) -> AeroVaultMeta {
    AeroVaultMeta {
        version: 1,
        created: now_iso(), // ideally preserve original, but OK for now
        modified: now_iso(),
        description: None,
        file_count: entries.len() as u32,
    }
}

/// Write vault from entries + metadata
fn write_vault(
    vault_path: &str,
    pwd: &SecretString,
    entries: &[(String, Vec<u8>)],
    meta: &AeroVaultMeta,
) -> Result<String, String> {
    let tmp_path = format!("{}.tmp", vault_path);

    let file = File::create(&tmp_path)
        .map_err(|e| format!("Failed to create temp vault: {}", e))?;
    let mut zip = ZipWriter::new(file);
    let aes_opts = || base_options().with_aes_encryption(zip::AesMode::Aes256, pwd.expose_secret());

    // Write metadata first
    let meta_json = serde_json::to_string_pretty(meta)
        .map_err(|e| format!("Failed to serialize metadata: {}", e))?;
    zip.start_file(META_ENTRY, aes_opts())
        .map_err(|e| format!("Failed to write metadata: {}", e))?;
    zip.write_all(meta_json.as_bytes())
        .map_err(|e| format!("Failed to write metadata: {}", e))?;

    // Write all entries
    for (name, data) in entries {
        zip.start_file(name, aes_opts())
            .map_err(|e| format!("Failed to add {}: {}", name, e))?;
        zip.write_all(data)
            .map_err(|e| format!("Failed to write {}: {}", name, e))?;
    }

    zip.finish()
        .map_err(|e| format!("Failed to finalize vault: {}", e))?;

    // Atomic replace
    std::fs::rename(&tmp_path, vault_path)
        .map_err(|e| format!("Failed to replace vault: {}", e))?;

    Ok(vault_path.to_string())
}
