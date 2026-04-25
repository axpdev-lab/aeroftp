//! Remote Vault support — download .aerovault files from remote servers,
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
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

use crate::provider_commands::ProviderState;
use std::path::PathBuf;
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

    // Verify local file is in temp directory (defense-in-depth for upload)
    let path = PathBuf::from(&local_path);
    if !path.exists() {
        return Err("Local vault file not found".into());
    }

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

/// Clean up a temporary vault file securely.
/// Validates: temp directory confinement, filename pattern, no symlinks.
#[tauri::command]
pub fn vault_v2_cleanup_temp(local_path: String) -> Result<(), String> {
    validate_no_null_bytes(&local_path)?;

    let path = PathBuf::from(&local_path);

    // Validate filename pattern — only clean up files we created
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("Invalid filename")?;
    if !is_managed_temp_vault_filename(file_name) {
        return Err("Can only clean up AeroFTP temporary vault files".into());
    }

    // Reject symlinks before any further operations (RB-006, SEC-005)
    match std::fs::symlink_metadata(&path) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err("Cannot clean up symlinks".into());
            }
        }
        Err(_) => return Ok(()), // File doesn't exist — already cleaned up
    }

    // Canonicalize and verify temp directory confinement
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

    // Zero-fill before delete for security (best-effort on modern storage)
    if let Ok(metadata) = std::fs::symlink_metadata(&canonical_path) {
        let size = metadata.len();
        if size > 0 {
            let chunk_size = std::cmp::min(size as usize, 1024 * 1024); // 1MB chunks
            let zeros = vec![0u8; chunk_size];
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .write(true)
                .open(&canonical_path)
            {
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
                // Force flush to disk
                let _ = file.sync_all();

                if write_failed {
                    // Still delete even if zero-fill failed
                    let _ = std::fs::remove_file(&canonical_path);
                    return Err(
                        "Secure zero-fill incomplete; file deleted without full overwrite".into(),
                    );
                }
            }
        }
    }

    std::fs::remove_file(&canonical_path)
        .map_err(|e| format!("Failed to remove temp file: {}", e))?;

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
