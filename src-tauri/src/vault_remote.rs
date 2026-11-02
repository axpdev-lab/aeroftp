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

/// Pick the on-disk staging path for a remote vault download: a unique file
/// in the scratch folder (never the temporary folder, which is often a
/// RAM-sized tmpfs), with room for `size` bytes checked first. Extracted for
/// direct unit testing of the location gate.
fn remote_vault_staging_path(size: u64) -> Result<PathBuf, String> {
    let dir = crate::util::scratch::staging_dir(size)?;
    Ok(dir.join(format!(
        "aerovault_remote_{}.aerovault",
        uuid::Uuid::new_v4()
    )))
}

/// Canonical roots an AeroFTP-managed temp vault file may live in: the OS
/// temporary directory and the on-disk scratch folder `vault_v2_download_remote`
/// stages into. Both are legitimate origins for the upload-back and cleanup
/// commands. Resolved, never created and without a room check: a gate that
/// validates an existing file must not depend on the free space left.
fn managed_temp_vault_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(temp) = std::env::temp_dir().canonicalize() {
        roots.push(temp);
    }
    // A scratch folder that does not exist yet holds no vault file to
    // validate, so a failed canonicalize simply omits it.
    if let Ok(scratch) = crate::util::scratch::scratch_dir_path().canonicalize() {
        roots.push(scratch);
    }
    roots
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

    // Get the active provider and download
    let mut provider_guard = state.provider.lock().await;
    let provider = provider_guard
        .as_mut()
        .ok_or("No active connection. Connect to a server first.")?;

    let size = provider.size(&remote_path).await.unwrap_or(0);
    let local_path = remote_vault_staging_path(size)?;

    let local_str = local_path
        .to_str()
        .ok_or("Temp path contains invalid UTF-8")?;

    provider
        .download_checked(&remote_path, local_str, None, None)
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
/// vault file confined to the OS temp directory or the on-disk scratch folder.
/// Shared by the upload and cleanup commands so both enforce the same
/// confinement (CLAUDE-AV-021).
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
    if !managed_temp_vault_roots()
        .iter()
        .any(|root| canonical_path.starts_with(root))
    {
        return Err("Local vault file must be in an AeroFTP-managed temporary folder".into());
    }

    Ok(canonical_path)
}

/// Clean up a temporary vault file securely.
/// Validates: AeroFTP-managed folder confinement (temp directory or scratch
/// folder), filename pattern, no symlinks.
///
/// `async` because the secure-delete path is the opposite of cheap: it opens
/// the file, overwrites every byte of it in 1 MB chunks, calls `sync_all()`
/// (which waits on the physical device), and only then unlinks. The duration is
/// linear in the size of the vault file and ends in an fsync, so on a sync
/// command the whole window would sit frozen for it. `spawn_blocking` keeps the
/// zero-fill on the blocking pool; the validation order is untouched.
#[tauri::command]
pub async fn vault_v2_cleanup_temp(local_path: String) -> Result<(), String> {
    tokio::task::spawn_blocking(move || vault_v2_cleanup_temp_blocking(local_path))
        .await
        .unwrap_or_else(|err| Err(format!("Temp vault cleanup task failed: {err}")))
}

fn vault_v2_cleanup_temp_blocking(local_path: String) -> Result<(), String> {
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

    let canonical_path = path
        .canonicalize()
        .map_err(|e| format!("Path resolution failed: {}", e))?;
    if !managed_temp_vault_roots()
        .iter()
        .any(|root| canonical_path.starts_with(root))
    {
        return Err("Can only clean up AeroFTP temporary vault files".into());
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

    /// The download staging path lives in the on-disk scratch folder, never in
    /// the temporary folder (on many Linux systems a tmpfs sized from RAM, so
    /// a large vault staged there eats RAM and can fail).
    #[test]
    fn a_remote_vault_is_staged_on_disk_not_in_the_temporary_folder() {
        let path = remote_vault_staging_path(0).unwrap();
        if dirs::cache_dir().is_some() {
            assert!(
                !path.starts_with(std::env::temp_dir()),
                "{} must not be in the temporary folder",
                path.display()
            );
            assert!(
                path.ancestors()
                    .any(|a| a.file_name() == Some(std::ffi::OsStr::new("scratch"))),
                "{} must live under a scratch folder",
                path.display()
            );
        } else {
            // The documented fallback with no cache folder: scratch is
            // temp_dir()/aeroftp-scratch.
            assert!(
                path.starts_with(std::env::temp_dir().join("aeroftp-scratch")),
                "{} must live under the scratch fallback",
                path.display()
            );
        }
        assert!(is_managed_temp_vault_filename(
            path.file_name().unwrap().to_str().unwrap()
        ));
    }

    /// A vault staged in the scratch folder still passes the confinement gate
    /// the upload-back and cleanup commands share.
    #[test]
    fn a_vault_staged_in_the_scratch_folder_passes_the_managed_path_gate() {
        let path = remote_vault_staging_path(0).unwrap();
        std::fs::write(&path, b"vault").unwrap();
        assert!(validate_managed_temp_vault_path(&path).is_ok());
        std::fs::remove_file(&path).unwrap();
    }
}
