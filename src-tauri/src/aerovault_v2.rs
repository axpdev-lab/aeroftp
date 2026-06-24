//! AeroVault v2: Tauri command wrappers for the `aerovault` crate.
//!
//! All cryptographic operations are delegated to the standalone `aerovault` crate
//! published on crates.io. This module provides async Tauri command bindings
//! with JSON serialization for the frontend.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use aerovault::{CreateOptions, EncryptionMode, Vault};
use serde::{Deserialize, Serialize};

// ============================================================================
// Tauri Commands: Core Operations
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
    log::info!(
        "vault v2 create: '{}'",
        crate::aerovault_v3::vault_basename(&vault_path)
    );
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
// Tauri Commands: File Operations
// ============================================================================

/// Add files to an existing AeroVault v2
#[tauri::command]
pub async fn vault_v2_add_files(
    vault_path: String,
    password: String,
    file_paths: Vec<String>,
) -> Result<serde_json::Value, String> {
    use crate::vault_telemetry::VaultReport;
    let started = std::time::Instant::now();

    let mut report = VaultReport::new("add_files", 2);
    let cascade = Vault::peek(&vault_path)
        .map(|p| p.mode == EncryptionMode::Cascade)
        .unwrap_or(false);
    let mut algos = vec![
        "kdf:argon2id(128MiB,t4,p4)".to_string(),
        "key_wrap:aes-256-kw".to_string(),
        "crypt:aes-256-gcm-siv".to_string(),
        "filenames:aes-256-siv".to_string(),
        "integrity:hmac-sha512".to_string(),
        "chunk:64KiB".to_string(),
    ];
    if cascade {
        algos.push("cascade:chacha20-poly1305".to_string());
    }
    report.set_algorithms(algos);
    report.set_profile(if cascade { "paranoid" } else { "advanced" });

    let plaintext_bytes: u64 = file_paths
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok().map(|m| m.len()))
        .sum();
    let size_before = std::fs::metadata(&vault_path).map(|m| m.len()).unwrap_or(0);
    report.step(format!(
        "scan: {} file(s), {} plaintext byte(s)",
        file_paths.len(),
        plaintext_bytes
    ));

    let vault = Vault::open(&vault_path, &password).map_err(|e| e.to_string())?;
    let paths: Vec<std::path::PathBuf> = file_paths.iter().map(std::path::PathBuf::from).collect();
    report.step(format!(
        "encrypt+seal: AES-256-GCM-SIV{} over 64KiB chunks, rebuild container",
        if cascade {
            " + ChaCha20-Poly1305 cascade"
        } else {
            ""
        }
    ));
    let added = vault.add_files(&paths).map_err(|e| e.to_string())?;
    let total = vault.list().map_err(|e| e.to_string())?.len();

    let size_after = std::fs::metadata(&vault_path).map(|m| m.len()).unwrap_or(0);
    for _ in 0..added {
        report.on_file(false);
    }
    // v2 is an external-crate engine: per-chunk internals are not exposed, so
    // the receipt reports observable accounting (plaintext in, ciphertext
    // growth on disk, timing) rather than chunk/dedup detail.
    report.plaintext_bytes = plaintext_bytes;
    report.encrypted_bytes = size_after.saturating_sub(size_before);
    report.step(format!(
        "done: {} file(s) added, container grew {} byte(s)",
        added, report.encrypted_bytes
    ));
    report.finish(started.elapsed().as_millis() as u64);
    log::info!(
        "vault v2 add-files: '{}' +{added} file(s)",
        crate::aerovault_v3::vault_basename(&vault_path)
    );

    Ok(serde_json::json!({
        "added": added,
        "total": total,
        "report": report
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

    let dest = std::path::Path::new(&dest_path);

    // Match v1/v3 destination semantics (the previous code derived the output
    // directory from the dest's parent and let the crate name the file after the
    // entry, so the requested filename was ignored and an existing name errored
    // with "File exists"). If `dest_path` is an existing directory, extract into
    // it (the entry keeps its own basename); otherwise `dest_path` is the literal
    // output file path and must be honored verbatim.
    if dest.is_dir() {
        let extracted = vault
            .extract(&entry_name, dest)
            .map_err(|e| e.to_string())?;
        return Ok(extracted.to_string_lossy().to_string());
    }

    // Literal-file destination. The `aerovault` crate only writes
    // `<output_dir>/<basename>` with an exclusive create, so it can neither pick
    // an arbitrary destination filename nor overwrite an existing file. Extract
    // into a unique temp dir next to the destination, then move the produced file
    // onto `dest_path`, overwriting like v3's atomic write.
    if let Some(parent) = dest.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create output directory: {}", e))?;
        }
    }
    let parent_dir = dest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| std::path::PathBuf::from("."));

    let tmp_dir = tempfile::Builder::new()
        .prefix(".aerovault_extract_")
        .tempdir_in(&parent_dir)
        .map_err(|e| format!("Failed to create temp directory: {}", e))?;

    let extracted = vault
        .extract(&entry_name, tmp_dir.path())
        .map_err(|e| e.to_string())?;

    // Overwrite any existing destination to match v1/v3.
    if dest.exists() {
        std::fs::remove_file(dest).map_err(|e| format!("Failed to replace destination: {}", e))?;
    }
    if let Err(rename_err) = std::fs::rename(&extracted, dest) {
        // Cross-device rename is not allowed: fall back to copy + remove.
        std::fs::copy(&extracted, dest).map_err(|e| {
            format!(
                "Failed to write destination: {} (rename: {})",
                e, rename_err
            )
        })?;
    }
    // `tmp_dir` is removed on drop.

    Ok(dest_path)
}

/// Extract every entry of an opened v2 `vault` into `dest`, rebuilding the directory
/// tree. The `aerovault` crate's v2 `extract`/`extract_all` write each file under its
/// `file_name()` only, so nested files all collapse into the destination root, and
/// two files that share a basename across directories (e.g. `a/x.txt` + `b/x.txt`)
/// overwrite each other: silent data loss. This extracts each file entry into its own
/// parent directory (which the crate then appends the file name to) and pre-creates
/// every directory entry, including empty ones. Returns the count of entries written.
pub(crate) fn extract_all_v2_tree(vault: &Vault, dest: &std::path::Path) -> Result<u32, String> {
    let entries = vault.list().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(dest)
        .map_err(|e| format!("Failed to create output directory: {}", e))?;
    let mut count = 0u32;
    // Pre-create directory entries so empty dirs survive and file targets exist.
    for e in entries.iter().filter(|e| e.is_dir) {
        std::fs::create_dir_all(dest.join(&e.name))
            .map_err(|err| format!("Failed to create directory '{}': {}", e.name, err))?;
        count += 1;
    }
    for e in entries.iter().filter(|e| !e.is_dir) {
        let parent = std::path::Path::new(&e.name)
            .parent()
            .unwrap_or_else(|| std::path::Path::new(""));
        let out_dir = dest.join(parent);
        std::fs::create_dir_all(&out_dir)
            .map_err(|err| format!("Failed to create directory for '{}': {}", e.name, err))?;
        vault
            .extract(&e.name, &out_dir)
            .map_err(|err| format!("Failed to extract '{}': {}", e.name, err))?;
        count += 1;
    }
    Ok(count)
}

/// Extract all entries from AeroVault v2, preserving the directory tree.
#[tauri::command]
pub async fn vault_v2_extract_all(
    vault_path: String,
    password: String,
    dest_dir: String,
) -> Result<serde_json::Value, String> {
    let vault = Vault::open(&vault_path, &password).map_err(|e| e.to_string())?;
    let count = extract_all_v2_tree(&vault, std::path::Path::new(&dest_dir))?;

    Ok(serde_json::json!({
        "extracted": count,
        "dest": dest_dir
    }))
}

// ─── AeroMount read-only mount + Save-All for v2 (#322, Ehud idea #1) ─────────

/// Read-only adapter exposing an opened v2 (or legacy v1) `aerovault::Vault`
/// through the shared [`crate::readable_vault::ReadableVault`] trait, so a v2
/// `.aerovault` mounts and exports through the SAME `VaultStorageProvider` and
/// Save-All exporters as v3 (Deliverable B). The crate's v2 engine has no
/// seekable range read, so `supports_seek()` keeps the default `false` and the
/// mount serves large files through its first-access plaintext temp cache.
/// `read_file` extracts one entry into an auto-removed temp dir (the crate writes
/// each entry under its basename there) then streams it into the sink.
pub struct VaultV2Readable {
    vault: Vault,
}

impl VaultV2Readable {
    /// Open an encrypted v2 (or v1) `.aerovault` for read-only access.
    pub fn open(vault_path: &str, password: &str) -> Result<Self, String> {
        let vault = Vault::open(vault_path, password).map_err(|e| e.to_string())?;
        Ok(Self { vault })
    }
}

impl crate::readable_vault::ReadableVault for VaultV2Readable {
    fn walk(&self) -> Result<Vec<crate::readable_vault::ReadableEntry>, String> {
        let entries = self.vault.list().map_err(|e| e.to_string())?;
        Ok(entries
            .iter()
            .map(|e| crate::readable_vault::ReadableEntry {
                rel_path: e.name.clone(),
                is_dir: e.is_dir,
                size: e.size,
                handle: String::new(),
            })
            .collect())
    }

    fn read_file(
        &self,
        entry: &crate::readable_vault::ReadableEntry,
        sink: &mut dyn std::io::Write,
    ) -> Result<(), String> {
        let tmp_dir = tempfile::Builder::new()
            .prefix(".aerovault-v2-export-")
            .tempdir()
            .map_err(|e| format!("temp dir: {e}"))?;
        // The crate writes the entry under its basename inside `tmp_dir` and
        // returns the produced path; stream that back into the sink.
        let extracted = self
            .vault
            .extract(&entry.rel_path, tmp_dir.path())
            .map_err(|e| e.to_string())?;
        let mut f =
            std::fs::File::open(&extracted).map_err(|e| format!("reopen extracted entry: {e}"))?;
        std::io::copy(&mut f, sink).map_err(|e| format!("stream extracted entry: {e}"))?;
        Ok(())
    }
}

/// Save-All for an encrypted v2 `.aerovault` (#322, Ehud idea #1): export the
/// whole decrypted tree to a single `.zip` or `.aerozip`. `folder` keeps using
/// the existing `vault_v2_extract_all`. SECURITY: writes PLAINTEXT to `dest_path`.
#[tauri::command]
pub async fn vault_v2_save_all(
    vault_path: String,
    password: String,
    dest_path: String,
    target: String,
    app: tauri::AppHandle,
) -> Result<crate::readable_vault::ExportReport, String> {
    crate::filesystem::validate_path(&dest_path)?;
    match target.as_str() {
        "zip" => tokio::task::spawn_blocking(
            move || -> Result<crate::readable_vault::ExportReport, String> {
                let adapter = VaultV2Readable::open(&vault_path, &password)?;
                let mut emit = crate::aerovault_v3::vault_extract_progress_emitter(app);
                crate::readable_vault::export_to_zip(
                    &adapter,
                    std::path::Path::new(&dest_path),
                    &mut emit,
                )
            },
        )
        .await
        .map_err(|e| format!("zip export task failed: {e}"))?,
        "aerozip" => {
            crate::aerovault_v3::ensure_aerovz_product_path(&dest_path)?;
            let _ = &app; // the single packing call has no incremental progress
            tokio::task::spawn_blocking(
                move || -> Result<crate::readable_vault::ExportReport, String> {
                    let scratch = tempfile::Builder::new()
                        .prefix(".aerovault-saveall-")
                        .tempdir()
                        .map_err(|e| format!("Create scratch dir: {e}"))?;
                    {
                        let vault =
                            Vault::open(&vault_path, &password).map_err(|e| e.to_string())?;
                        extract_all_v2_tree(&vault, scratch.path())?;
                    }
                    let (files, dirs) = crate::aerovault_v3::create_aerozip_from_dir(
                        scratch.path(),
                        std::path::Path::new(&dest_path),
                    )?;
                    Ok(crate::readable_vault::ExportReport {
                        files: files as u64,
                        dirs: dirs as u64,
                        skipped: Vec::new(),
                    })
                },
            )
            .await
            .map_err(|e| format!("aerozip export task failed: {e}"))?
        }
        other => Err(format!("Unknown save-all target: {other}")),
    }
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
    vault.delete_entry(&entry_name).map_err(|e| e.to_string())?;
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

/// Move an entry (file or directory) inside a vault
#[tauri::command]
pub async fn vault_v2_move_entry(
    vault_path: String,
    password: String,
    from: String,
    to: String,
) -> Result<serde_json::Value, String> {
    validate_vault_relative_path(&from)?;
    validate_vault_relative_path(&to)?;
    let vault = Vault::open(&vault_path, &password).map_err(|e| e.to_string())?;
    vault.move_entry(&from, &to).map_err(|e| e.to_string())?;

    Ok(serde_json::json!({
        "moved": true,
        "from": from,
        "to": to
    }))
}

/// Rename an entry while keeping the same parent directory
#[tauri::command]
pub async fn vault_v2_rename_entry(
    vault_path: String,
    password: String,
    current_name: String,
    new_name: String,
) -> Result<serde_json::Value, String> {
    validate_vault_relative_path(&current_name)?;
    if new_name.trim().is_empty()
        || new_name.contains('/')
        || new_name.contains('\\')
        || new_name.contains("..")
        || new_name.contains('\0')
    {
        return Err("Invalid new name".to_string());
    }

    let vault = Vault::open(&vault_path, &password).map_err(|e| e.to_string())?;
    vault
        .rename_entry(&current_name, &new_name)
        .map_err(|e| e.to_string())?;

    Ok(serde_json::json!({
        "renamed": true,
        "from": current_name,
        "to": new_name
    }))
}

/// Copy an entry (file or directory) inside a vault
#[tauri::command]
pub async fn vault_v2_copy_entry(
    vault_path: String,
    password: String,
    from: String,
    to: String,
) -> Result<serde_json::Value, String> {
    validate_vault_relative_path(&from)?;
    validate_vault_relative_path(&to)?;
    let vault = Vault::open(&vault_path, &password).map_err(|e| e.to_string())?;
    vault.copy_entry(&from, &to).map_err(|e| e.to_string())?;

    Ok(serde_json::json!({
        "copied": true,
        "from": from,
        "to": to
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
// Tauri Commands: Maintenance
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
        return Err(format!(
            "Too many sync actions: {} (max {})",
            actions.len(),
            MAX_SCAN_ENTRIES
        ));
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
                errors.push(format!(
                    "Failed to delete existing entries for overwrite: {}",
                    e
                ));
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
                Err(e) => errors.push(format!("Failed to add files to vault dir '{}': {}", dir, e)),
            }
        }
    }

    // Process to_local: extract vault entries to local dir
    for name in &to_local_files {
        if name.contains("..")
            || name.starts_with('/')
            || name.starts_with('\\')
            || name.contains('\0')
            || name.contains('\\')
        {
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
// Helpers: Path Validation
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
// Tauri Commands: Recursive Directory Encryption
// ============================================================================

const MAX_SCAN_DEPTH: usize = 100;
const MAX_SCAN_ENTRIES: usize = 500_000;

/// Scan a local directory and return file/directory counts and total size.
/// This is a preview command: no vault operations are performed.
#[tauri::command]
pub async fn vault_v2_scan_directory(source_dir: String) -> Result<serde_json::Value, String> {
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
    let (added_files, added_dirs) = vault_v2_add_directory_core(
        vault_path,
        password,
        source_dir,
        target_prefix,
        |current, total, current_file| {
            let _ = app.emit(
                "vault-add-progress",
                serde_json::json!({
                    "current": current,
                    "total": total,
                    "current_file": current_file,
                }),
            );
        },
    )
    .await?;
    Ok(serde_json::json!({
        "added_files": added_files,
        "added_dirs": added_dirs,
        "total_entries": added_files + added_dirs
    }))
}

/// AppHandle-free core of [`vault_v2_add_directory`]: recursively add `source_dir`
/// into the v2 vault (optionally under `target_prefix`), invoking `on_progress(added,
/// total, current_file)` throttled to ~150ms. Shared by the GUI command (emits a
/// Tauri event) and the CLI `vault add-dir` verb. Returns `(added_files, added_dirs)`.
pub async fn vault_v2_add_directory_core(
    vault_path: String,
    password: String,
    source_dir: String,
    target_prefix: Option<String>,
    mut on_progress: impl FnMut(usize, usize, &str),
) -> Result<(usize, usize), String> {
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
        let paths: Vec<std::path::PathBuf> = dir_files.iter().map(|f| f.abs_path.clone()).collect();

        let vault = Vault::open(&vault_path, &password).map_err(|e| e.to_string())?;

        let added = if dir_key.is_empty() {
            vault.add_files(&paths).map_err(|e| e.to_string())?
        } else {
            vault
                .add_files_to_dir(&paths, dir_key)
                .map_err(|e| e.to_string())?
        };

        added_files += added as usize;

        // Report progress (per-batch, throttled)
        if last_emit.elapsed() >= throttle || added_files == total_files {
            let current_file = dir_files.last().map(|f| f.rel_path.as_str()).unwrap_or("");
            on_progress(added_files, total_files, current_file);
            last_emit = std::time::Instant::now();
        }
    }

    Ok((added_files, added_dirs))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    /// Regression for F1: `vault extract` must write the entry to the exact
    /// destination filename the caller asked for, and must overwrite a
    /// pre-existing destination — matching v1/v3. The legacy v2 wrapper derived
    /// the output *directory* from the dest's parent and let the crate name the
    /// file after the entry, so the requested filename was ignored and a second
    /// extract failed with "File exists (os error 17)". This test exercises all
    /// three on-disk formats so the bug can't regress in any of them.
    #[test]
    fn extract_honors_named_destination_for_all_versions() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        let pw = "extract-dest-pw-1234";

        // Source payload whose basename ("av_payload.bin") differs from every
        // requested destination filename, so honoring the dest is observable.
        let payload: Vec<u8> = (0..40_000u32).map(|i| (i % 251) as u8).collect();
        let src = base.join("av_payload.bin");
        fs::write(&src, &payload).unwrap();
        let entry_name = "av_payload.bin".to_string();

        // (label, vault path, build closure, extract closure)
        struct Case {
            label: &'static str,
            vault: std::path::PathBuf,
        }
        let cases = [
            Case {
                label: "v1",
                vault: base.join("v1.aerovault"),
            },
            Case {
                label: "v2",
                vault: base.join("v2.av"),
            },
            Case {
                label: "v2-cascade",
                vault: base.join("v2c.av"),
            },
            Case {
                label: "v3",
                vault: base.join("v3.av3"),
            },
        ];

        let rt = rt();
        for case in &cases {
            let vault_path = case.vault.to_string_lossy().to_string();
            let src_path = src.to_string_lossy().to_string();

            // --- create + add, per format ---
            match case.label {
                "v1" => {
                    rt.block_on(crate::aerovault::vault_create(
                        vault_path.clone(),
                        pw.to_string(),
                        None,
                    ))
                    .unwrap_or_else(|e| panic!("[{}] create: {e}", case.label));
                    rt.block_on(crate::aerovault::vault_add_files(
                        vault_path.clone(),
                        pw.to_string(),
                        vec![src_path.clone()],
                    ))
                    .unwrap_or_else(|e| panic!("[{}] add: {e}", case.label));
                }
                "v2" | "v2-cascade" => {
                    let cascade = case.label == "v2-cascade";
                    rt.block_on(super::vault_v2_create(
                        vault_path.clone(),
                        pw.to_string(),
                        None,
                        cascade,
                    ))
                    .unwrap_or_else(|e| panic!("[{}] create: {e}", case.label));
                    rt.block_on(super::vault_v2_add_files(
                        vault_path.clone(),
                        pw.to_string(),
                        vec![src_path.clone()],
                    ))
                    .unwrap_or_else(|e| panic!("[{}] add: {e}", case.label));
                }
                "v3" => {
                    rt.block_on(crate::aerovault_v3::vault_v3_create(
                        vault_path.clone(),
                        pw.to_string(),
                        None,
                    ))
                    .unwrap_or_else(|e| panic!("[{}] create: {e}", case.label));
                    rt.block_on(crate::aerovault_v3::vault_v3_add_files_inner(
                        vault_path.clone(),
                        pw.to_string(),
                        vec![src_path.clone()],
                        None,
                    ))
                    .unwrap_or_else(|e| panic!("[{}] add: {e}", case.label));
                }
                _ => unreachable!(),
            }

            // Destination filename intentionally differs from the entry basename.
            let dest = base.join(format!("{}_renamed.out", case.label));
            let dest_str = dest.to_string_lossy().to_string();

            let extract = |dest_str: String| -> Result<String, String> {
                match case.label {
                    "v1" => rt.block_on(crate::aerovault::vault_extract_entry(
                        vault_path.clone(),
                        pw.to_string(),
                        entry_name.clone(),
                        dest_str,
                    )),
                    "v2" | "v2-cascade" => rt.block_on(super::vault_v2_extract_entry(
                        vault_path.clone(),
                        pw.to_string(),
                        entry_name.clone(),
                        dest_str,
                    )),
                    "v3" => rt.block_on(crate::aerovault_v3::vault_v3_extract_entry(
                        vault_path.clone(),
                        pw.to_string(),
                        entry_name.clone(),
                        dest_str,
                    )),
                    _ => unreachable!(),
                }
            };

            // First extract: must create the file at the EXACT requested path.
            let out = extract(dest_str.clone())
                .unwrap_or_else(|e| panic!("[{}] first extract: {e}", case.label));
            assert!(
                Path::new(&out).exists(),
                "[{}] reported path does not exist: {out}",
                case.label
            );
            assert!(
                dest.exists(),
                "[{}] requested destination filename was not honored (no {dest_str})",
                case.label
            );
            assert_eq!(
                fs::read(&dest).unwrap(),
                payload,
                "[{}] extracted bytes mismatch",
                case.label
            );

            // Second extract to the SAME path: must overwrite, not error with
            // "File exists" (the core of the F1 regression for v2).
            extract(dest_str.clone())
                .unwrap_or_else(|e| panic!("[{}] overwrite extract failed: {e}", case.label));
            assert_eq!(
                fs::read(&dest).unwrap(),
                payload,
                "[{}] bytes after overwrite mismatch",
                case.label
            );
        }
    }

    // ---- v2 tree-preserving extraction stress matrix -----------------------
    // The crate's v2 `extract`/`extract_all` write each file under its `file_name()`
    // only, so nested files collapse into the destination root and two files that
    // share a basename across directories silently overwrite each other (data loss).
    // `extract_all_v2_tree` rebuilds the tree; these tests stress it with the worst
    // cases: duplicate basenames, deep nesting, empty dirs/files, unicode, and both
    // Standard and Cascade (paranoid) modes.

    /// Snapshot a directory tree as a sorted map of relative-path -> contents
    /// (`None` marks a directory, `Some(bytes)` a file). Lets two trees be compared
    /// for exact structural + byte equality regardless of walk order.
    fn snapshot_tree(root: &Path) -> std::collections::BTreeMap<String, Option<Vec<u8>>> {
        let mut map = std::collections::BTreeMap::new();
        for entry in walkdir::WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.path() == root {
                continue;
            }
            let rel = entry
                .path()
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if entry.file_type().is_dir() {
                map.insert(rel, None);
            } else {
                map.insert(rel, Some(fs::read(entry.path()).unwrap()));
            }
        }
        map
    }

    /// Build a v2 vault at `path` (cascade or not) containing `dirs` (created
    /// parents-first) and `files` ((relpath, bytes)). Mirrors the two-pass add the
    /// GUI command uses, via the crate `Vault` API directly.
    fn build_v2_vault(
        path: &Path,
        pw: &str,
        cascade: bool,
        dirs: &[&str],
        files: &[(&str, Vec<u8>)],
        scratch: &Path,
    ) {
        let mode = if cascade {
            super::EncryptionMode::Cascade
        } else {
            super::EncryptionMode::Standard
        };
        let opts = super::CreateOptions::new(path, pw).with_mode(mode);
        super::Vault::create(opts).unwrap();
        // Create directories shortest-path-first so parents exist before children.
        let mut sorted_dirs = dirs.to_vec();
        sorted_dirs.sort_by_key(|d| d.matches('/').count());
        {
            let v = super::Vault::open(path, pw).unwrap();
            for d in &sorted_dirs {
                v.create_directory(d).unwrap();
            }
        }
        // Stage each file on disk, then add it into its parent dir inside the vault.
        for (i, (rel, bytes)) in files.iter().enumerate() {
            let staged = scratch.join(format!("stage_{i}"));
            fs::create_dir_all(&staged).unwrap();
            let fname = Path::new(rel)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_string();
            let staged_file = staged.join(&fname);
            fs::write(&staged_file, bytes).unwrap();
            let parent = Path::new(rel)
                .parent()
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            let v = super::Vault::open(path, pw).unwrap();
            if parent.is_empty() {
                v.add_files(&[&staged_file]).unwrap();
            } else {
                v.add_files_to_dir(&[&staged_file], &parent).unwrap();
            }
        }
    }

    /// The worst-case tree: a root file, duplicate basenames across two dirs with
    /// DIFFERENT content (the data-loss trap), deep nesting, a multi-chunk file, an
    /// empty file, an empty directory, and a unicode path.
    fn stress_tree() -> (Vec<&'static str>, Vec<(&'static str, Vec<u8>)>) {
        let dirs = vec!["d1", "d2", "d1/deep", "d1/deep/deeper", "empty_dir", "café"];
        let files = vec![
            ("root.txt", b"root-level file".to_vec()),
            ("d1/x.txt", b"content of d1 x".to_vec()),
            ("d2/x.txt", b"DIFFERENT content of d2 x".to_vec()),
            ("d1/deep/deeper/leaf.bin", vec![0xABu8; 70_000]),
            ("d1/empty.txt", Vec::new()),
            (
                "café/ünïcode.txt",
                "naïve café façade \u{1F600}".as_bytes().to_vec(),
            ),
        ];
        (dirs, files)
    }

    #[test]
    fn v2_extract_all_preserves_tree_and_distinguishes_duplicate_basenames() {
        for cascade in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let vault = dir.path().join("stress.aerovault");
            let scratch = dir.path().join("scratch");
            fs::create_dir_all(&scratch).unwrap();
            let pw = "v2-tree-stress-pw-12345";
            let (dirs, files) = stress_tree();
            build_v2_vault(&vault, pw, cascade, &dirs, &files, &scratch);

            let out = dir.path().join("out");
            let v = super::Vault::open(&vault, pw).unwrap();
            super::extract_all_v2_tree(&v, &out).unwrap();
            let snap = snapshot_tree(&out);

            // Every file landed at its FULL path with the right bytes.
            for (rel, bytes) in &files {
                assert_eq!(
                    snap.get(*rel),
                    Some(&Some(bytes.clone())),
                    "[cascade={cascade}] file '{rel}' missing or wrong content"
                );
            }
            // The duplicate-basename trap: d1/x.txt and d2/x.txt must stay distinct.
            assert_ne!(
                snap.get("d1/x.txt").unwrap(),
                snap.get("d2/x.txt").unwrap(),
                "[cascade={cascade}] duplicate basenames collapsed: data loss"
            );
            // No stray flattened copy at the root.
            assert!(
                !out.join("x.txt").exists() && !out.join("leaf.bin").exists(),
                "[cascade={cascade}] a nested file leaked to the destination root"
            );
            // Empty directory survives.
            assert!(
                out.join("empty_dir").is_dir(),
                "[cascade={cascade}] empty directory was not recreated"
            );
        }
    }

    #[test]
    fn v2_extract_all_command_roundtrips_nested_tree() {
        let dir = tempfile::tempdir().unwrap();
        let vault = dir.path().join("cmd.aerovault");
        let scratch = dir.path().join("scratch");
        fs::create_dir_all(&scratch).unwrap();
        let pw = "v2-cmd-tree-pw-12345";
        let (dirs, files) = stress_tree();
        build_v2_vault(&vault, pw, false, &dirs, &files, &scratch);

        let out = dir.path().join("out");
        let res = rt()
            .block_on(super::vault_v2_extract_all(
                vault.to_string_lossy().to_string(),
                pw.to_string(),
                out.to_string_lossy().to_string(),
            ))
            .unwrap();
        // extracted count covers every dir + file entry.
        assert_eq!(
            res["extracted"].as_u64().unwrap() as usize,
            dirs.len() + files.len()
        );
        let snap = snapshot_tree(&out);
        for (rel, bytes) in &files {
            assert_eq!(
                snap.get(*rel),
                Some(&Some(bytes.clone())),
                "'{rel}' mismatch"
            );
        }
    }

    /// The AppHandle-free `vault_v2_add_directory_core` (shared by the GUI command and
    /// the CLI `add-dir` verb) must add a nested tree preserving structure, with no
    /// progress sink, and round-trip through the tree-preserving extractor.
    #[test]
    fn v2_add_directory_core_roundtrips_tree_without_apphandle() {
        let dir = tempfile::tempdir().unwrap();
        let vault = dir.path().join("core.aerovault");
        let src = dir.path().join("src");
        let (dirs, files) = stress_tree();
        for d in &dirs {
            fs::create_dir_all(src.join(d)).unwrap();
        }
        for (rel, bytes) in &files {
            let p = src.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, bytes).unwrap();
        }
        let pw = "v2-core-pw-123456";
        let opts = super::CreateOptions::new(&vault, pw).with_mode(super::EncryptionMode::Standard);
        super::Vault::create(opts).unwrap();

        let mut ticks = 0;
        let (added_files, _added_dirs) = rt()
            .block_on(super::vault_v2_add_directory_core(
                vault.to_string_lossy().to_string(),
                pw.to_string(),
                src.to_string_lossy().to_string(),
                None,
                |_, _, _| ticks += 1,
            ))
            .unwrap();
        assert_eq!(added_files, files.len());
        let _ = ticks; // progress is best-effort; presence of the sink is the contract

        let out = dir.path().join("out");
        let v = super::Vault::open(&vault, pw).unwrap();
        super::extract_all_v2_tree(&v, &out).unwrap();
        let snap = snapshot_tree(&out);
        for (rel, bytes) in &files {
            assert_eq!(
                snap.get(*rel),
                Some(&Some(bytes.clone())),
                "'{rel}' mismatch via add_directory_core"
            );
        }
        assert_ne!(
            snap.get("d1/x.txt").unwrap(),
            snap.get("d2/x.txt").unwrap(),
            "duplicate basenames collapsed via core path"
        );
    }

    /// AeroMount (#322): the v2 `ReadableVault` adapter must walk the full tree and
    /// stream each file byte-identical, including the worst cases (duplicate
    /// basenames across dirs, deep nesting, a multi-chunk file, an empty file, an
    /// empty dir, unicode). This is the seam the read-only mount and Save-All ride.
    #[test]
    fn v2_readable_adapter_walks_and_reads_byte_identical() {
        use crate::readable_vault::ReadableVault;
        let dir = tempfile::tempdir().unwrap();
        let vault = dir.path().join("mountme.aerovault");
        let scratch = dir.path().join("scratch");
        fs::create_dir_all(&scratch).unwrap();
        let pw = "v2-readable-pw-123456";
        let (dirs, files) = stress_tree();
        build_v2_vault(&vault, pw, false, &dirs, &files, &scratch);

        let adapter = super::VaultV2Readable::open(&vault.to_string_lossy(), pw).unwrap();
        let entries = adapter.walk().unwrap();

        for (rel, bytes) in &files {
            let e = entries
                .iter()
                .find(|e| e.rel_path == *rel && !e.is_dir)
                .unwrap_or_else(|| panic!("walk missing file '{rel}'"));
            assert_eq!(e.size, bytes.len() as u64, "size mismatch for '{rel}'");
            let mut buf = Vec::new();
            adapter.read_file(e, &mut buf).unwrap();
            assert_eq!(&buf, bytes, "content mismatch for '{rel}'");
        }
        // Duplicate basenames across directories stay distinct (the v2 data-loss trap).
        let d1x = entries.iter().find(|e| e.rel_path == "d1/x.txt").unwrap();
        let d2x = entries.iter().find(|e| e.rel_path == "d2/x.txt").unwrap();
        let (mut b1, mut b2) = (Vec::new(), Vec::new());
        adapter.read_file(d1x, &mut b1).unwrap();
        adapter.read_file(d2x, &mut b2).unwrap();
        assert_ne!(b1, b2, "adapter collapsed duplicate basenames: data loss");
        // Directory entries (incl. empty ones) are surfaced for the mount tree.
        assert!(
            entries
                .iter()
                .any(|e| e.is_dir && e.rel_path == "empty_dir"),
            "walk dropped an empty directory"
        );
    }
}
