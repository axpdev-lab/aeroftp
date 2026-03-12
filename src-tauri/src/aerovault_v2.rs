//! AeroVault v2 — Tauri command wrappers for the `aerovault` crate.
//!
//! All cryptographic operations are delegated to the standalone `aerovault` crate
//! published on crates.io. This module provides async Tauri command bindings
//! with JSON serialization for the frontend.

use aerovault::{EncryptionMode, Vault, CreateOptions};
use serde::{Deserialize, Serialize};

// ============================================================================
// Tauri Commands — Core Operations
// ============================================================================

/// Create a new AeroVault v2
#[tauri::command]
pub async fn vault_v2_create(
    vault_path: String,
    password: String,
    _description: Option<String>,
    cascade_mode: bool,
) -> Result<String, String> {
    let mode = if cascade_mode {
        EncryptionMode::Cascade
    } else {
        EncryptionMode::Standard
    };

    let opts = CreateOptions::new(&vault_path, password).with_mode(mode);

    Vault::create(opts).map_err(|e| e.to_string())?;
    Ok(vault_path)
}

/// Open an AeroVault v2 and return its metadata
#[tauri::command]
pub async fn vault_v2_open(
    vault_path: String,
    password: String,
) -> Result<serde_json::Value, String> {
    let vault = Vault::open(&vault_path, &password).map_err(|e| e.to_string())?;
    let entries = vault.list().map_err(|e| e.to_string())?;
    let info = vault.security_info();

    Ok(serde_json::json!({
        "version": info.version,
        "cascade_mode": vault.mode() == EncryptionMode::Cascade,
        "chunk_size": vault.chunk_size(),
        "file_count": entries.len(),
        "files": entries.iter().map(|e| serde_json::json!({
            "name": e.name,
            "size": e.size,
            "is_dir": e.is_dir,
            "modified": e.modified,
        })).collect::<Vec<_>>()
    }))
}

/// Check if a file is AeroVault v2 format
#[tauri::command]
pub async fn is_vault_v2(path: String) -> Result<bool, String> {
    Ok(Vault::is_vault(&path))
}

/// Peek at vault header to get security info without password
#[tauri::command]
pub async fn vault_v2_peek(path: String) -> Result<serde_json::Value, String> {
    let peek = Vault::peek(&path).map_err(|e| e.to_string())?;

    Ok(serde_json::json!({
        "version": peek.version,
        "cascade_mode": peek.mode == EncryptionMode::Cascade,
        "security_level": if peek.mode == EncryptionMode::Cascade { "paranoid" } else { "advanced" }
    }))
}

/// Get AeroVault v2 security info for UI display
#[tauri::command]
pub async fn vault_v2_security_info() -> serde_json::Value {
    serde_json::json!({
        "version": "2.0",
        "encryption": {
            "content": "AES-256-GCM-SIV (RFC 8452)",
            "filenames": "AES-256-SIV",
            "key_wrap": "AES-256-KW (RFC 3394)",
            "cascade": "ChaCha20-Poly1305 (optional)"
        },
        "kdf": {
            "algorithm": "Argon2id",
            "memory": "128 MiB",
            "iterations": 4,
            "parallelism": 4
        },
        "integrity": {
            "header": "HMAC-SHA512",
            "chunks": "GCM-SIV authentication tag"
        },
        "chunk_size": "64 KB",
        "features": [
            "Nonce misuse resistance",
            "Memory-hard key derivation",
            "Encrypted filenames",
            "Header integrity verification",
            "Optional cascade encryption"
        ]
    })
}

// ============================================================================
// Tauri Commands — File Operations
// ============================================================================

/// Add files to an existing AeroVault v2
#[tauri::command]
pub async fn vault_v2_add_files(
    vault_path: String,
    password: String,
    file_paths: Vec<String>,
) -> Result<serde_json::Value, String> {
    let vault = Vault::open(&vault_path, &password).map_err(|e| e.to_string())?;
    let paths: Vec<std::path::PathBuf> = file_paths.iter().map(std::path::PathBuf::from).collect();
    let added = vault.add_files(&paths).map_err(|e| e.to_string())?;
    let total = vault.list().map_err(|e| e.to_string())?.len();

    Ok(serde_json::json!({
        "added": added,
        "total": total
    }))
}

/// Add files to a specific directory inside an AeroVault v2
#[tauri::command]
pub async fn vault_v2_add_files_to_dir(
    vault_path: String,
    password: String,
    file_paths: Vec<String>,
    target_dir: String,
) -> Result<serde_json::Value, String> {
    validate_vault_relative_path(&target_dir)?;
    let vault = Vault::open(&vault_path, &password).map_err(|e| e.to_string())?;
    let paths: Vec<std::path::PathBuf> = file_paths.iter().map(std::path::PathBuf::from).collect();
    let added = vault
        .add_files_to_dir(&paths, &target_dir)
        .map_err(|e| e.to_string())?;
    let total = vault.list().map_err(|e| e.to_string())?.len();

    Ok(serde_json::json!({
        "added": added,
        "total": total
    }))
}

/// Extract a single entry from AeroVault v2
#[tauri::command]
pub async fn vault_v2_extract_entry(
    vault_path: String,
    password: String,
    entry_name: String,
    dest_path: String,
) -> Result<String, String> {
    let vault = Vault::open(&vault_path, &password).map_err(|e| e.to_string())?;

    // If dest_path looks like a file, use the parent as output directory
    let dest = std::path::Path::new(&dest_path);
    let output_dir = if dest.extension().is_some() {
        dest.parent().unwrap_or(dest)
    } else {
        dest
    };

    std::fs::create_dir_all(output_dir)
        .map_err(|e| format!("Failed to create output directory: {}", e))?;

    let extracted = vault
        .extract(&entry_name, output_dir)
        .map_err(|e| e.to_string())?;

    Ok(extracted.to_string_lossy().to_string())
}

/// Extract all entries from AeroVault v2
#[tauri::command]
pub async fn vault_v2_extract_all(
    vault_path: String,
    password: String,
    dest_dir: String,
) -> Result<serde_json::Value, String> {
    let vault = Vault::open(&vault_path, &password).map_err(|e| e.to_string())?;

    std::fs::create_dir_all(&dest_dir)
        .map_err(|e| format!("Failed to create output directory: {}", e))?;

    let count = vault.extract_all(&dest_dir).map_err(|e| e.to_string())?;

    Ok(serde_json::json!({
        "extracted": count,
        "dest": dest_dir
    }))
}

/// Create a directory inside a vault
#[tauri::command]
pub async fn vault_v2_create_directory(
    vault_path: String,
    password: String,
    dir_name: String,
) -> Result<serde_json::Value, String> {
    validate_vault_relative_path(&dir_name)?;
    let vault = Vault::open(&vault_path, &password).map_err(|e| e.to_string())?;
    let created = vault
        .create_directory(&dir_name)
        .map_err(|e| e.to_string())?;

    Ok(serde_json::json!({
        "created": created,
        "dir": dir_name
    }))
}

/// Delete a single entry from a vault
#[tauri::command]
pub async fn vault_v2_delete_entry(
    vault_path: String,
    password: String,
    entry_name: String,
) -> Result<serde_json::Value, String> {
    validate_vault_relative_path(&entry_name)?;
    let vault = Vault::open(&vault_path, &password).map_err(|e| e.to_string())?;
    vault
        .delete_entry(&entry_name)
        .map_err(|e| e.to_string())?;
    let remaining = vault.list().map_err(|e| e.to_string())?.len();

    Ok(serde_json::json!({
        "deleted": entry_name,
        "remaining": remaining
    }))
}

/// Delete multiple entries from a vault
#[tauri::command]
pub async fn vault_v2_delete_entries(
    vault_path: String,
    password: String,
    entry_names: Vec<String>,
    recursive: bool,
) -> Result<serde_json::Value, String> {
    for name in &entry_names {
        validate_vault_relative_path(name)?;
    }
    let vault = Vault::open(&vault_path, &password).map_err(|e| e.to_string())?;
    let names: Vec<&str> = entry_names.iter().map(|s| s.as_str()).collect();
    let removed = vault
        .delete_entries(&names, recursive)
        .map_err(|e| e.to_string())?;
    let remaining = vault.list().map_err(|e| e.to_string())?.len();

    Ok(serde_json::json!({
        "removed": removed,
        "remaining": remaining
    }))
}

/// Change vault password
#[tauri::command]
pub async fn vault_v2_change_password(
    vault_path: String,
    old_password: String,
    new_password: String,
) -> Result<String, String> {
    let mut vault = Vault::open(&vault_path, &old_password).map_err(|e| e.to_string())?;
    vault
        .change_password(new_password)
        .map_err(|e| e.to_string())?;
    Ok("Password changed successfully".into())
}

// ============================================================================
// Tauri Commands — Maintenance
// ============================================================================

/// Compact result for JSON serialization
#[derive(Serialize)]
pub struct CompactResult {
    pub original_size: u64,
    pub compacted_size: u64,
    pub saved_bytes: u64,
    pub file_count: usize,
}

/// Compact vault by removing orphaned data
#[tauri::command]
pub async fn vault_v2_compact(
    vault_path: String,
    password: String,
) -> Result<CompactResult, String> {
    let vault = Vault::open(&vault_path, &password).map_err(|e| e.to_string())?;
    let result = vault.compact().map_err(|e| e.to_string())?;

    Ok(CompactResult {
        original_size: result.original_size,
        compacted_size: result.compacted_size,
        saved_bytes: result.saved_bytes,
        file_count: result.file_count,
    })
}

// ============================================================================
// Vault Bidirectional Sync
// ============================================================================

/// A conflict entry where the file exists in both vault and local with different content
#[derive(Serialize)]
pub struct VaultSyncConflict {
    pub name: String,
    pub vault_modified: String,
    pub local_modified: String,
    pub vault_size: u64,
    pub local_size: u64,
}

/// Comparison result between vault contents and a local directory
#[derive(Serialize)]
pub struct VaultSyncComparison {
    pub vault_only: Vec<String>,
    pub local_only: Vec<String>,
    pub conflicts: Vec<VaultSyncConflict>,
    pub unchanged: usize,
}

/// Compare vault contents with a local directory to determine sync actions
#[tauri::command]
pub async fn vault_v2_sync_compare(
    vault_path: String,
    password: String,
    local_dir: String,
) -> Result<VaultSyncComparison, String> {
    let local_dir_path = std::path::Path::new(&local_dir);
    if !local_dir_path.is_dir() {
        return Err(format!("Local directory does not exist: {}", local_dir));
    }
    let local_dir_canonical = local_dir_path
        .canonicalize()
        .map_err(|e| format!("Failed to resolve local directory: {}", e))?;

    let vault = Vault::open(&vault_path, &password).map_err(|e| e.to_string())?;
    let entries = vault.list().map_err(|e| e.to_string())?;

    // Build vault entries map: name -> (size, modified)
    let mut vault_files: std::collections::HashMap<String, (u64, String)> =
        std::collections::HashMap::new();
    for entry in &entries {
        if !entry.is_dir {
            vault_files.insert(entry.name.clone(), (entry.size, entry.modified.clone()));
        }
    }

    // Walk local directory
    let mut local_files: std::collections::HashMap<String, (u64, String)> =
        std::collections::HashMap::new();
    let mut scan_count: usize = 0;
    for dir_entry in walkdir::WalkDir::new(&local_dir_canonical)
        .follow_links(false)
        .max_depth(MAX_SCAN_DEPTH)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        scan_count += 1;
        if scan_count > MAX_SCAN_ENTRIES {
            return Err(format!(
                "Directory too large: exceeded {} entries",
                MAX_SCAN_ENTRIES
            ));
        }
        if dir_entry.file_type().is_dir() {
            continue;
        }
        let full_path = dir_entry.path();
        let rel_path = full_path
            .strip_prefix(&local_dir_canonical)
            .map_err(|_| "Failed to compute relative path")?;

        let rel_str = rel_path.to_string_lossy().replace('\\', "/");
        if rel_str.is_empty() {
            continue;
        }

        let metadata = std::fs::metadata(full_path)
            .map_err(|e| format!("Failed to read metadata for {}: {}", rel_str, e))?;

        let modified = metadata
            .modified()
            .map(|t| {
                let datetime: chrono::DateTime<chrono::Utc> = t.into();
                datetime.format("%Y-%m-%dT%H:%M:%SZ").to_string()
            })
            .unwrap_or_default();

        local_files.insert(rel_str, (metadata.len(), modified));
    }

    // Compare
    let mut vault_only = Vec::new();
    let mut local_only = Vec::new();
    let mut conflicts = Vec::new();
    let mut unchanged: usize = 0;

    for (name, (v_size, v_modified)) in &vault_files {
        match local_files.get(name) {
            Some((l_size, l_modified)) => {
                if v_size == l_size && v_modified == l_modified {
                    unchanged += 1;
                } else {
                    conflicts.push(VaultSyncConflict {
                        name: name.clone(),
                        vault_modified: v_modified.clone(),
                        local_modified: l_modified.clone(),
                        vault_size: *v_size,
                        local_size: *l_size,
                    });
                }
            }
            None => {
                vault_only.push(name.clone());
            }
        }
    }

    for name in local_files.keys() {
        if !vault_files.contains_key(name) {
            local_only.push(name.clone());
        }
    }

    vault_only.sort();
    local_only.sort();
    conflicts.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(VaultSyncComparison {
        vault_only,
        local_only,
        conflicts,
        unchanged,
    })
}

/// Action to apply during vault sync
#[derive(Deserialize)]
pub struct VaultSyncAction {
    pub name: String,
    pub action: String,
}

/// Result of applying vault sync actions
#[derive(Serialize)]
pub struct VaultSyncResult {
    pub to_vault: usize,
    pub to_local: usize,
    pub skipped: usize,
    pub errors: Vec<String>,
}

/// Apply sync decisions between vault and local directory
#[tauri::command]
pub async fn vault_v2_sync_apply(
    vault_path: String,
    password: String,
    local_dir: String,
    actions: Vec<VaultSyncAction>,
) -> Result<VaultSyncResult, String> {
    if actions.len() > MAX_SCAN_ENTRIES {
        return Err(format!("Too many sync actions: {} (max {})", actions.len(), MAX_SCAN_ENTRIES));
    }
    let local_dir_path = std::path::Path::new(&local_dir);
    if !local_dir_path.is_dir() {
        return Err(format!("Local directory does not exist: {}", local_dir));
    }
    let local_dir_canonical = local_dir_path
        .canonicalize()
        .map_err(|e| format!("Failed to resolve local directory: {}", e))?;

    let mut to_vault_count: usize = 0;
    let mut to_local_count: usize = 0;
    let mut skipped_count: usize = 0;
    let mut errors: Vec<String> = Vec::new();

    // Separate actions by type
    let mut to_vault_files: Vec<String> = Vec::new();
    let mut to_local_files: Vec<String> = Vec::new();

    for action in &actions {
        match action.action.as_str() {
            "to_vault" => to_vault_files.push(action.name.clone()),
            "to_local" => to_local_files.push(action.name.clone()),
            "skip" => skipped_count += 1,
            other => errors.push(format!("Unknown action '{}' for '{}'", other, action.name)),
        }
    }

    // Process to_vault: add local files to vault
    if !to_vault_files.is_empty() {
        // First delete existing entries that will be overwritten (conflicts)
        let vault = Vault::open(&vault_path, &password).map_err(|e| e.to_string())?;
        let existing: Vec<String> = vault
            .list()
            .map_err(|e| e.to_string())?
            .iter()
            .filter(|e| !e.is_dir)
            .map(|e| e.name.clone())
            .collect();

        let to_delete: Vec<&str> = to_vault_files
            .iter()
            .filter(|name| existing.contains(name))
            .map(|s| s.as_str())
            .collect();

        if !to_delete.is_empty() {
            if let Err(e) = vault.delete_entries(&to_delete, false) {
                errors.push(format!("Failed to delete existing entries for overwrite: {}", e));
            }
        }
        drop(vault);

        // Group files by directory
        let mut root_files: Vec<std::path::PathBuf> = Vec::new();
        let mut dir_files: std::collections::HashMap<String, Vec<std::path::PathBuf>> =
            std::collections::HashMap::new();

        for name in &to_vault_files {
            let local_path = local_dir_canonical.join(name);
            if !local_path.exists() {
                errors.push(format!("Local file not found: {}", name));
                continue;
            }
            let canonical = match local_path.canonicalize() {
                Ok(c) => c,
                Err(e) => {
                    errors.push(format!("Failed to resolve path {}: {}", name, e));
                    continue;
                }
            };
            if !canonical.starts_with(&local_dir_canonical) {
                errors.push(format!("Path traversal detected: {}", name));
                continue;
            }

            if let Some(parent) = std::path::Path::new(name).parent() {
                let parent_str = parent.to_string_lossy().replace('\\', "/");
                if parent_str.is_empty() {
                    root_files.push(canonical);
                } else {
                    dir_files.entry(parent_str).or_default().push(canonical);
                }
            } else {
                root_files.push(canonical);
            }
        }

        // Create necessary directories and add files
        let vault = Vault::open(&vault_path, &password).map_err(|e| e.to_string())?;

        for dir_path in dir_files.keys() {
            if let Err(e) = vault.create_directory(dir_path) {
                errors.push(format!("Failed to create vault dir '{}': {}", dir_path, e));
            }
        }
        drop(vault);

        if !root_files.is_empty() {
            let vault = Vault::open(&vault_path, &password).map_err(|e| e.to_string())?;
            match vault.add_files(&root_files) {
                Ok(added) => to_vault_count += added as usize,
                Err(e) => errors.push(format!("Failed to add root files to vault: {}", e)),
            }
        }

        for (dir, file_paths) in &dir_files {
            let vault = Vault::open(&vault_path, &password).map_err(|e| e.to_string())?;
            match vault.add_files_to_dir(file_paths, dir) {
                Ok(added) => to_vault_count += added as usize,
                Err(e) => {
                    errors.push(format!("Failed to add files to vault dir '{}': {}", dir, e))
                }
            }
        }
    }

    // Process to_local: extract vault entries to local dir
    for name in &to_local_files {
        if name.contains("..") || name.starts_with('/') || name.starts_with('\\') || name.contains('\0') || name.contains('\\') {
            errors.push(format!("Invalid file name blocked: {}", name));
            skipped_count += 1;
            continue;
        }

        let dest = local_dir_canonical.join(name);
        if let Some(parent) = dest.parent() {
            if !parent.exists() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    errors.push(format!("Failed to create directory for '{}': {}", name, e));
                    continue;
                }
            }
        }

        match vault_v2_extract_entry(
            vault_path.clone(),
            password.clone(),
            name.clone(),
            dest.to_string_lossy().to_string(),
        )
        .await
        {
            Ok(_) => to_local_count += 1,
            Err(e) => errors.push(format!("Failed to extract '{}': {}", name, e)),
        }
    }

    Ok(VaultSyncResult {
        to_vault: to_vault_count,
        to_local: to_local_count,
        skipped: skipped_count,
        errors,
    })
}

// ============================================================================
// Helpers — Path Validation
// ============================================================================

/// Validate a relative path for vault entry safety.
/// Rejects path traversal, absolute paths, null bytes, and Windows drive letters.
fn validate_vault_relative_path(path: &str) -> Result<(), String> {
    if path.contains("..")
        || path.starts_with('/')
        || path.starts_with('\\')
        || path.contains('\0')
        || path.contains('\\')
    {
        return Err(format!("Invalid path: {}", path));
    }
    #[cfg(windows)]
    if path.len() >= 2 && path.as_bytes()[1] == b':' {
        return Err(format!("Absolute path not allowed: {}", path));
    }
    Ok(())
}

// ============================================================================
// Tauri Commands — Recursive Directory Encryption
// ============================================================================

const MAX_SCAN_DEPTH: usize = 100;
const MAX_SCAN_ENTRIES: usize = 500_000;

/// Scan a local directory and return file/directory counts and total size.
/// This is a preview command — no vault operations are performed.
#[tauri::command]
pub async fn vault_v2_scan_directory(
    source_dir: String,
) -> Result<serde_json::Value, String> {
    let source = std::path::Path::new(&source_dir)
        .canonicalize()
        .map_err(|e| format!("Failed to resolve directory: {}", e))?;

    if !source.is_dir() {
        return Err(format!("Not a directory: {}", source_dir));
    }

    let mut file_count: u64 = 0;
    let mut dir_count: u64 = 0;
    let mut total_size: u64 = 0;
    let mut entry_count: usize = 0;

    for entry in walkdir::WalkDir::new(&source)
        .follow_links(false)
        .max_depth(MAX_SCAN_DEPTH)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        // Skip the root directory itself
        if entry.path() == source {
            continue;
        }

        entry_count += 1;
        if entry_count > MAX_SCAN_ENTRIES {
            return Err(format!(
                "Directory exceeds maximum entry limit ({})",
                MAX_SCAN_ENTRIES
            ));
        }

        if entry.file_type().is_dir() {
            dir_count += 1;
        } else {
            file_count += 1;
            total_size += entry.metadata().map(|m| m.len()).unwrap_or(0);
        }
    }

    Ok(serde_json::json!({
        "file_count": file_count,
        "dir_count": dir_count,
        "total_size": total_size
    }))
}

/// Recursively add an entire local directory into an AeroVault v2.
///
/// Walks `source_dir`, creates vault directories in depth order, then adds files
/// in per-directory batches. Emits `vault-add-progress` events throttled to 150ms.
#[tauri::command]
pub async fn vault_v2_add_directory(
    app: tauri::AppHandle,
    vault_path: String,
    password: String,
    source_dir: String,
    target_prefix: Option<String>,
) -> Result<serde_json::Value, String> {
    use tauri::Emitter;

    let source = std::path::Path::new(&source_dir)
        .canonicalize()
        .map_err(|e| format!("Failed to resolve directory: {}", e))?;

    if !source.is_dir() {
        return Err(format!("Not a directory: {}", source_dir));
    }

    // Collect all entries with relative paths
    struct DirEntry {
        rel_path: String,
        is_dir: bool,
        abs_path: std::path::PathBuf,
        depth: usize,
    }

    let mut all_entries: Vec<DirEntry> = Vec::new();

    for entry in walkdir::WalkDir::new(&source)
        .follow_links(false)
        .max_depth(MAX_SCAN_DEPTH)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.path() == source {
            continue;
        }

        if all_entries.len() >= MAX_SCAN_ENTRIES {
            return Err(format!(
                "Directory exceeds maximum entry limit ({})",
                MAX_SCAN_ENTRIES
            ));
        }

        let rel_path = entry
            .path()
            .strip_prefix(&source)
            .map_err(|_| "Failed to compute relative path")?
            .to_string_lossy()
            .replace('\\', "/");

        if rel_path.is_empty() {
            continue;
        }

        validate_vault_relative_path(&rel_path)?;

        let full_rel = match &target_prefix {
            Some(prefix) => {
                let trimmed = prefix.trim_matches('/');
                if trimmed.is_empty() {
                    rel_path.clone()
                } else {
                    format!("{}/{}", trimmed, rel_path)
                }
            }
            None => rel_path.clone(),
        };

        // Validate the composed path (covers target_prefix traversal)
        validate_vault_relative_path(&full_rel)?;

        all_entries.push(DirEntry {
            rel_path: full_rel,
            is_dir: entry.file_type().is_dir(),
            abs_path: entry.path().to_path_buf(),
            depth: entry.depth(),
        });
    }

    // Separate directories and files
    let mut dirs: Vec<&DirEntry> = all_entries.iter().filter(|e| e.is_dir).collect();
    let files: Vec<&DirEntry> = all_entries.iter().filter(|e| !e.is_dir).collect();

    // Sort directories by depth ascending (create parents before children)
    dirs.sort_by_key(|d| d.depth);

    let total_files = files.len();
    let mut added_files: usize = 0;
    let mut added_dirs: usize = 0;

    // First pass: create all directories
    if !dirs.is_empty() {
        let vault = Vault::open(&vault_path, &password).map_err(|e| e.to_string())?;
        for dir_entry in &dirs {
            match vault.create_directory(&dir_entry.rel_path) {
                Ok(_) => added_dirs += 1,
                Err(e) => {
                    // Ignore "already exists" errors for intermediate dirs
                    let err_str = e.to_string();
                    if !err_str.contains("already exists") {
                        return Err(format!(
                            "Failed to create directory '{}': {}",
                            dir_entry.rel_path, err_str
                        ));
                    }
                    added_dirs += 1;
                }
            }
        }
    }

    // Second pass: add files grouped by parent directory
    let mut files_by_dir: std::collections::BTreeMap<String, Vec<&DirEntry>> =
        std::collections::BTreeMap::new();

    for file_entry in &files {
        let parent = std::path::Path::new(&file_entry.rel_path)
            .parent()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        files_by_dir.entry(parent).or_default().push(file_entry);
    }

    let mut last_emit = std::time::Instant::now();
    let throttle = std::time::Duration::from_millis(150);

    for (dir_key, dir_files) in &files_by_dir {
        let paths: Vec<std::path::PathBuf> =
            dir_files.iter().map(|f| f.abs_path.clone()).collect();

        let vault = Vault::open(&vault_path, &password).map_err(|e| e.to_string())?;

        let added = if dir_key.is_empty() {
            vault.add_files(&paths).map_err(|e| e.to_string())?
        } else {
            vault
                .add_files_to_dir(&paths, dir_key)
                .map_err(|e| e.to_string())?
        };

        added_files += added as usize;

        // Emit progress (per-batch, throttled)
        if last_emit.elapsed() >= throttle || added_files == total_files {
            let current_file = dir_files
                .last()
                .map(|f| f.rel_path.as_str())
                .unwrap_or("");
            let _ = app.emit(
                "vault-add-progress",
                serde_json::json!({
                    "current": added_files,
                    "total": total_files,
                    "current_file": current_file
                }),
            );
            last_emit = std::time::Instant::now();
        }
    }

    Ok(serde_json::json!({
        "added_files": added_files,
        "added_dirs": added_dirs,
        "total_entries": added_files + added_dirs
    }))
}
