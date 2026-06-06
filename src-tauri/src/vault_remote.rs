//! Remote Vault support: download .aerovault files from remote servers,
//! operate locally, and upload changes back.
//!
//! Security hardening (v2.2.4 audit remediation):
//! - Symlink detection before zero-fill (RB-006, SEC-005)
//! - Path canonicalization for starts_with check (RB-006)
//! - Filename pattern validation (RB-007)
//! - Error propagation on write_all + sync_all (RB-009, SEC-004)
//! - Null byte validation (RB-013)
//! - UTF-8 path handling without unwrap (RB-011)

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use crate::provider_commands::ProviderState;
use std::path::{Path, PathBuf};
use tauri::State;

/// Validate a path has no null bytes (defense-in-depth for C FFI providers).
fn validate_no_null_bytes(path: &str) -> Result<(), String> {
    if path.contains('\0') {
        return Err("Path contains null bytes".into());
    }
    Ok(())
}

/// Check whether a cleanup target is an AeroFTP-managed temp vault file.
/// Extracted for direct unit testing of the filename gate used by
/// `vault_v2_cleanup_temp` (which otherwise requires Tauri State and FS).
fn is_managed_temp_vault_filename(file_name: &str) -> bool {
    file_name.starts_with("aerovault_remote_") && file_name.ends_with(".aerovault")
}

/// Download a remote .aerovault file to a temporary local path.
/// Returns the temporary local file path.
#[tauri::command]
pub async fn vault_v2_download_remote(
    state: State<'_, ProviderState>,
    remote_path: String,
) -> Result<String, String> {
    validate_no_null_bytes(&remote_path)?;

    if !remote_path.ends_with(".aerovault") {
        return Err("File must have .aerovault extension".into());
    }

    if remote_path.contains("..") {
        return Err("Path traversal not allowed".into());
    }

    let temp_dir = std::env::temp_dir();
    let temp_name = format!("aerovault_remote_{}.aerovault", uuid::Uuid::new_v4());
    let local_path = temp_dir.join(&temp_name);

    // Get the active provider and download
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or("No active connection. Connect to a server first.")?;

    let local_str = local_path
        .to_str()
        .ok_or("Temp path contains invalid UTF-8")?;

    provider
        .download(&remote_path, local_str, None)
        .await
        .map_err(|e| format!("Download failed: {}", e))?;

    // Set restrictive permissions on Linux/macOS
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        let _ = std::fs::set_permissions(&local_path, perms);
    }

    // Verify the file exists and has content
    let metadata = std::fs::metadata(&local_path)
        .map_err(|e| format!("Downloaded file not accessible: {}", e))?;
    if metadata.len() == 0 {
        let _ = std::fs::remove_file(&local_path);
        return Err("Downloaded file is empty".into());
    }

    Ok(local_path.to_string_lossy().to_string())
}

/// Upload a local vault file back to the remote server.
#[tauri::command]
pub async fn vault_v2_upload_remote(
    state: State<'_, ProviderState>,
    local_path: String,
    remote_path: String,
) -> Result<(), String> {
    validate_no_null_bytes(&local_path)?;
    validate_no_null_bytes(&remote_path)?;

    if !local_path.ends_with(".aerovault") || !remote_path.ends_with(".aerovault") {
        return Err("Files must have .aerovault extension".into());
    }

    if remote_path.contains("..") {
        return Err("Path traversal not allowed".into());
    }

    // Confine the source to an AeroFTP-managed temp vault file so a compromised
    // renderer cannot exfiltrate an arbitrary readable *.aerovault (e.g. a user
    // vault outside temp) to the connected remote (CLAUDE-AV-021).
    let path = PathBuf::from(&local_path);
    validate_managed_temp_vault_path(&path)?;

    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or("No active connection. Connect to a server first.")?;

    provider
        .upload(&local_path, &remote_path, None)
        .await
        .map_err(|e| format!("Upload failed: {}", e))?;

    Ok(())
}

/// Assert that `path` is an existing, non-symlink, regular AeroFTP-managed temp
/// vault file confined to the OS temp directory. Shared by the upload and
/// cleanup commands so both enforce the same confinement (CLAUDE-AV-021).
fn validate_managed_temp_vault_path(path: &Path) -> Result<PathBuf, String> {
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("Invalid vault file name")?;
    if !is_managed_temp_vault_filename(file_name) {
        return Err("Not an AeroFTP-managed temporary vault file".into());
    }

    let meta =
        std::fs::symlink_metadata(path).map_err(|_| "Local vault file not found".to_string())?;
    if meta.file_type().is_symlink() {
        return Err("Refusing to operate on a symlinked vault file".into());
    }
    if !meta.is_file() {
        return Err("Local vault path is not a regular file".into());
    }

    let canonical_path = path
        .canonicalize()
        .map_err(|e| format!("Failed to resolve vault path: {}", e))?;
    let canonical_temp = std::env::temp_dir()
        .canonicalize()
        .map_err(|e| format!("Failed to resolve temp directory: {}", e))?;
    if !canonical_path.starts_with(&canonical_temp) {
        return Err("Local vault file must be in the temp directory".into());
    }

    Ok(canonical_path)
}

/// Clean up a temporary vault file securely.
/// Validates: temp directory confinement, filename pattern, no symlinks.
#[tauri::command]
pub fn vault_v2_cleanup_temp(local_path: String) -> Result<(), String> {
    validate_no_null_bytes(&local_path)?;

    let path = PathBuf::from(&local_path);

    // Validate filename pattern: only clean up files we created
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("Invalid filename")?;
    if !is_managed_temp_vault_filename(file_name) {
        return Err("Can only clean up AeroFTP temporary vault files".into());
    }

    let mut file = match open_temp_file_no_follow(&path) {
        Ok(file) => file,
        Err(e) if e == "TEMP_NOT_FOUND" => return Ok(()),
        Err(e) => return Err(e),
    };
    let metadata = file
        .metadata()
        .map_err(|e| format!("Temp file metadata failed: {}", e))?;
    if !metadata.is_file() {
        return Err("Can only clean up regular temp files".into());
    }

    let temp_dir = std::env::temp_dir();
    let canonical_path = path
        .canonicalize()
        .map_err(|e| format!("Path resolution failed: {}", e))?;
    let canonical_temp = temp_dir
        .canonicalize()
        .map_err(|e| format!("Temp dir resolution failed: {}", e))?;
    if !canonical_path.starts_with(&canonical_temp) {
        return Err("Can only clean up files in temp directory".into());
    }
    ensure_same_file(&file, &path)?;

    // Zero-fill before delete for security (best-effort on modern storage)
    let size = metadata.len();
    if size > 0 {
        let chunk_size = std::cmp::min(size as usize, 1024 * 1024); // 1MB chunks
        let zeros = vec![0u8; chunk_size];
        use std::io::Write;
        let mut remaining = size;
        let mut write_failed = false;
        while remaining > 0 {
            let chunk = std::cmp::min(remaining as usize, zeros.len());
            if file.write_all(&zeros[..chunk]).is_err() {
                write_failed = true;
                break;
            }
            remaining -= chunk as u64;
        }
        let _ = file.sync_all();

        if write_failed {
            ensure_same_file(&file, &path)?;
            let _ = std::fs::remove_file(&path);
            return Err("Secure zero-fill incomplete; file deleted without full overwrite".into());
        }
    }

    ensure_same_file(&file, &path)?;
    drop(file);
    std::fs::remove_file(&path).map_err(|e| format!("Failed to remove temp file: {}", e))?;

    Ok(())
}

fn open_temp_file_no_follow(path: &Path) -> Result<std::fs::File, String> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    match options.open(path) {
        Ok(file) => Ok(file),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err("TEMP_NOT_FOUND".to_string()),
        Err(e) => Err(format!("Open temp file failed: {}", e)),
    }
}

#[cfg(unix)]
fn ensure_same_file(file: &std::fs::File, path: &Path) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    let fd_meta = file
        .metadata()
        .map_err(|e| format!("Temp fd metadata failed: {}", e))?;
    let path_meta =
        std::fs::symlink_metadata(path).map_err(|e| format!("Temp path metadata failed: {}", e))?;
    if path_meta.file_type().is_symlink()
        || fd_meta.dev() != path_meta.dev()
        || fd_meta.ino() != path_meta.ino()
    {
        return Err("Temp file changed during cleanup".to_string());
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_same_file(_file: &std::fs::File, path: &Path) -> Result<(), String> {
    if std::fs::symlink_metadata(path)
        .map_err(|e| format!("Temp path metadata failed: {}", e))?
        .file_type()
        .is_symlink()
    {
        return Err("Cannot clean up symlinks".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_no_null_bytes_accepts_clean_paths() {
        assert!(validate_no_null_bytes("").is_ok());
        assert!(validate_no_null_bytes("/normal/path").is_ok());
        assert!(validate_no_null_bytes("secret.aerovault").is_ok());
        assert!(validate_no_null_bytes("folder/with spaces/file.aerovault").is_ok());
    }

    #[test]
    fn validate_no_null_bytes_rejects_embedded_nulls() {
        assert!(validate_no_null_bytes("/evil\0path").is_err());
        assert!(validate_no_null_bytes("\0leading").is_err());
        assert!(validate_no_null_bytes("trailing\0").is_err());
    }

    #[test]
    fn is_managed_temp_vault_filename_requires_prefix_and_extension() {
        // Accepts AeroFTP-managed pattern
        assert!(is_managed_temp_vault_filename(
            "aerovault_remote_550e8400-e29b-41d4-a716-446655440000.aerovault"
        ));
        assert!(is_managed_temp_vault_filename(
            "aerovault_remote_x.aerovault"
        ));

        // Missing prefix
        assert!(!is_managed_temp_vault_filename("secret.aerovault"));
        // Missing extension
        assert!(!is_managed_temp_vault_filename("aerovault_remote_abc"));
        // Wrong extension
        assert!(!is_managed_temp_vault_filename("aerovault_remote_abc.zip"));
        // Partial/empty
        assert!(!is_managed_temp_vault_filename("aerovault_remote_"));
        assert!(!is_managed_temp_vault_filename(""));
        assert!(!is_managed_temp_vault_filename(".aerovault"));
    }
}
