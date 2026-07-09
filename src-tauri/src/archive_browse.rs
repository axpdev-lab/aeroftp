//! Archive browsing and selective extraction
//!
//! Provides listing and single-entry extraction for ZIP, 7z, TAR, and RAR archives.
//! Used by the ArchiveBrowser frontend component and AeroVault module.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use crate::archive_progress::{phase, ArchiveProgress, ProgressReader};
use secrecy::{ExposeSecret, SecretString};
use serde::Serialize;

/// M16: Validate archive entry paths to prevent path traversal attacks (ZipSlip).
/// Delegates to the single shared guard in `lib.rs` so the check never drifts. The
/// old local copy used a substring `contains("..")` test that also rejected legit
/// names like `a..b.txt`; the shared guard splits on `/` and `\` and matches the
/// `..` component precisely. (CLAUDE-AV-B1-03)
use crate::is_safe_archive_entry;

/// Metadata for a single entry inside an archive
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveEntry {
    pub name: String,
    pub size: u64,
    pub compressed_size: u64,
    pub is_dir: bool,
    pub is_encrypted: bool,
    pub modified: Option<String>,
}

// ─── ZIP ───────────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn list_zip(
    archive_path: String,
    password: Option<String>,
) -> Result<Vec<ArchiveEntry>, String> {
    use std::fs::File;
    use zip::ZipArchive;

    let file = File::open(&archive_path).map_err(|e| format!("Failed to open archive: {}", e))?;
    let mut archive =
        ZipArchive::new(file).map_err(|e| format!("Failed to read archive: {}", e))?;

    let secret_password: Option<SecretString> = password.map(SecretString::from);
    let mut entries = Vec::new();

    for i in 0..archive.len() {
        // Use by_index_raw to get metadata without decompressing
        let raw = archive
            .by_index_raw(i)
            .map_err(|e| format!("Failed to read entry {}: {}", i, e))?;

        let encrypted = raw.encrypted();
        let name = raw.name().to_string();
        let is_dir = name.ends_with('/');
        let size = raw.size();
        let compressed = raw.compressed_size();
        let modified = raw.last_modified().map(|dt| {
            format!(
                "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
                dt.year(),
                dt.month(),
                dt.day(),
                dt.hour(),
                dt.minute(),
                dt.second()
            )
        });

        entries.push(ArchiveEntry {
            name,
            size,
            compressed_size: compressed,
            is_dir,
            is_encrypted: encrypted,
            modified,
        });

        // Drop the borrow before next iteration
        drop(raw);
    }

    // If encrypted and password provided, verify it works by trying to read first file
    if entries.iter().any(|e| e.is_encrypted) {
        if let Some(ref pwd) = secret_password {
            let file =
                File::open(&archive_path).map_err(|e| format!("Failed to open archive: {}", e))?;
            let mut archive =
                ZipArchive::new(file).map_err(|e| format!("Failed to read archive: {}", e))?;
            // Try decrypting first non-dir entry to validate password
            for i in 0..archive.len() {
                let entry = archive
                    .by_index_decrypt(i, pwd.expose_secret().as_bytes())
                    .map_err(|e| format!("Invalid password or corrupt archive: {}", e))?;
                if !entry.name().ends_with('/') {
                    break;
                }
            }
        }
    }

    Ok(entries)
}

#[tauri::command]
pub async fn extract_zip_entry(
    archive_path: String,
    entry_name: String,
    output_path: String,
    password: Option<String>,
    app: tauri::AppHandle,
) -> Result<String, String> {
    extract_zip_entry_impl(archive_path, entry_name, output_path, password, Some(app)).await
}

/// Implementation shared by the GUI command (passes `Some(app)` for the live progress
/// bar) and internal Rust callers such as the legacy v1 vault extract (`None`).
pub(crate) async fn extract_zip_entry_impl(
    archive_path: String,
    entry_name: String,
    output_path: String,
    password: Option<String>,
    app: Option<tauri::AppHandle>,
) -> Result<String, String> {
    use std::fs::{self, File};
    use std::io::Read;

    let secret_password: Option<SecretString> = password.map(SecretString::from);

    // M16: Validate entry name before extraction to prevent path traversal (ZipSlip)
    if !is_safe_archive_entry(&entry_name) {
        return Err(format!(
            "Unsafe archive entry path rejected: {}",
            entry_name
        ));
    }

    let file = File::open(&archive_path).map_err(|e| format!("Failed to open archive: {}", e))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| format!("Failed to read archive: {}", e))?;

    let mut entry = if let Some(ref pwd) = secret_password {
        archive
            .by_name_decrypt(&entry_name, pwd.expose_secret().as_bytes())
            .map_err(|e| format!("Entry not found: {}", e))?
    } else {
        archive
            .by_name(&entry_name)
            .map_err(|e| format!("Entry not found: {}", e))?
    };

    let out_path = std::path::Path::new(&output_path);
    if let Some(parent) = out_path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("Failed to create directory: {}", e))?;
    }

    // Atomic write via a uniquely-named temp file in the destination directory,
    // then persist onto the target. A fixed `.aerotmp` sibling (out_path with the
    // extension swapped) could collide with and clobber a real user file of that
    // name; a randomized temp name cannot, and NamedTempFile auto-removes on every
    // error path. (CLAUDE-AV-B1-09)
    let parent = out_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    let mut tmp = tempfile::Builder::new()
        .prefix(".aeroftp-extract-")
        .suffix(".aerotmp")
        .tempfile_in(parent)
        .map_err(|e| format!("Failed to create temp file: {}", e))?;
    // Bound extraction to the entry's declared uncompressed size (+1 to detect a
    // stream that expands past what it declared), defusing a deflate bomb that
    // would otherwise stream unbounded data to disk (CLAUDE-AV-015).
    let declared = entry.size();
    let mut progress = ArchiveProgress::for_optional_app(app, phase::EXTRACTING, declared);
    let written = {
        let mut limited = entry.by_ref().take(declared + 1);
        let mut counted = ProgressReader::new(&mut limited, &mut progress);
        std::io::copy(&mut counted, tmp.as_file_mut())
            .map_err(|e| format!("Failed to extract entry: {}", e))?
    };
    if written > declared {
        return Err(format!(
            "Archive entry '{}' expands past its declared size (compression bomb?)",
            entry_name
        ));
    }
    tmp.persist(out_path)
        .map_err(|e| format!("Failed to finalize extracted file: {}", e))?;
    progress.finish();

    Ok(output_path)
}

// ─── 7z ────────────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn list_7z(
    archive_path: String,
    password: Option<String>,
) -> Result<Vec<ArchiveEntry>, String> {
    use sevenz_rust2::{ArchiveReader, Password};
    use std::fs::File;
    use std::io::BufReader;

    let secret_password: Option<SecretString> = password.map(SecretString::from);

    let file = File::open(&archive_path).map_err(|e| format!("Failed to open archive: {}", e))?;
    let reader = BufReader::new(file);

    let pwd = secret_password
        .as_ref()
        .map(|p| Password::from(p.expose_secret()))
        .unwrap_or_else(Password::empty);

    // First try opening: if metadata itself is encrypted, we need the password
    let mut archive = match ArchiveReader::new(reader, pwd) {
        Ok(a) => a,
        Err(e) => {
            let err_str = format!("{:?}", e);
            if err_str.contains("password")
                || err_str.contains("Password")
                || err_str.contains("PasswordRequired")
            {
                return Err("PasswordRequired".to_string());
            }
            return Err(format!("Failed to read 7z archive: {}", e));
        }
    };

    // Check if content is encrypted by trying to read first entry without password
    let content_encrypted = if secret_password.is_none() {
        let mut encrypted = false;
        let has_files = archive.archive().files.iter().any(|f| f.has_stream());
        if has_files {
            let result = archive.for_each_entries(|_entry, reader| {
                let mut buf = [0u8; 1];
                if reader.read(&mut buf).is_err() {
                    encrypted = true;
                }
                Ok(false) // stop after first
            });
            if result.is_err() {
                encrypted = true;
            }
        }
        encrypted
    } else {
        false
    };

    if content_encrypted {
        return Err("PasswordRequired".to_string());
    }

    // Re-open to get clean state for listing (for_each_entries consumed the reader)
    let file2 =
        std::fs::File::open(&archive_path).map_err(|e| format!("Failed to open archive: {}", e))?;
    let reader2 = std::io::BufReader::new(file2);
    let pwd2 = secret_password
        .as_ref()
        .map(|p| Password::from(p.expose_secret()))
        .unwrap_or_else(Password::empty);
    let archive2 = ArchiveReader::new(reader2, pwd2)
        .map_err(|e| format!("Failed to read 7z archive: {}", e))?;

    let entries: Vec<ArchiveEntry> = archive2
        .archive()
        .files
        .iter()
        .map(|entry| {
            let name = entry.name().to_string();
            let is_dir = entry.is_directory();
            ArchiveEntry {
                name,
                size: entry.size(),
                compressed_size: 0,
                is_dir,
                is_encrypted: entry.has_stream() && secret_password.is_some(),
                modified: None,
            }
        })
        .collect();

    Ok(entries)
}

#[tauri::command]
pub async fn extract_7z_entry(
    archive_path: String,
    entry_name: String,
    output_path: String,
    password: Option<String>,
    app: tauri::AppHandle,
) -> Result<String, String> {
    use sevenz_rust2::{ArchiveReader, Password};
    use std::fs::{self, File};
    use std::io::BufReader;

    // M16: Validate entry name before extraction to prevent path traversal
    if !is_safe_archive_entry(&entry_name) {
        return Err(format!(
            "Unsafe archive entry path rejected: {}",
            entry_name
        ));
    }

    let secret_password: Option<SecretString> = password.map(SecretString::from);

    let file = File::open(&archive_path).map_err(|e| format!("Failed to open archive: {}", e))?;
    let reader = BufReader::new(file);

    let pwd = secret_password
        .as_ref()
        .map(|p| Password::from(p.expose_secret()))
        .unwrap_or_else(Password::empty);

    let mut archive =
        ArchiveReader::new(reader, pwd).map_err(|e| format!("Failed to read 7z archive: {}", e))?;

    let out_path = std::path::Path::new(&output_path);
    if let Some(parent) = out_path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("Failed to create directory: {}", e))?;
    }

    let mut found = false;
    let tmp_path = out_path.with_extension("aerotmp");
    archive
        .for_each_entries(|entry, reader| {
            if entry.name() == entry_name {
                found = true;
                // The entry's uncompressed size is the honest denominator; create the
                // emitter here where it is known, count bytes streamed to disk.
                let mut progress =
                    ArchiveProgress::for_app(app.clone(), phase::EXTRACTING, entry.size());
                let mut outfile = File::create(&tmp_path)?;
                {
                    let mut counted = ProgressReader::new(reader, &mut progress);
                    std::io::copy(&mut counted, &mut outfile)?;
                }
                progress.finish();
                // Stop iterating once our entry is extracted: continuing would keep
                // decoding the rest of the solid stream for nothing, and a later-entry
                // error would surface as a failure AFTER the bar already hit 100%.
                return Ok(false);
            }
            Ok(true)
        })
        .map_err(|e| {
            let _ = fs::remove_file(&tmp_path);
            format!("Failed to extract: {}", e)
        })?;

    if found {
        fs::rename(&tmp_path, out_path)
            .map_err(|e| format!("Failed to finalize extracted file: {}", e))?;
    } else {
        let _ = fs::remove_file(&tmp_path);
    }

    if !found {
        return Err(format!("Entry '{}' not found in archive", entry_name));
    }

    Ok(output_path)
}

// ─── TAR ───────────────────────────────────────────────────────────────────────

fn open_tar_reader(archive_path: &str) -> Result<Box<dyn std::io::Read>, String> {
    use std::fs::File;

    let file = File::open(archive_path).map_err(|e| format!("Failed to open archive: {}", e))?;
    let ext = archive_path.to_lowercase();

    if ext.ends_with(".tar.gz") || ext.ends_with(".tgz") {
        Ok(Box::new(flate2::read::GzDecoder::new(file)))
    } else if ext.ends_with(".tar.xz") || ext.ends_with(".txz") {
        Ok(Box::new(xz2::read::XzDecoder::new(file)))
    } else if ext.ends_with(".tar.bz2") || ext.ends_with(".tbz2") {
        Ok(Box::new(bzip2::read::BzDecoder::new(file)))
    } else {
        Ok(Box::new(file))
    }
}

#[tauri::command]
pub async fn list_tar(archive_path: String) -> Result<Vec<ArchiveEntry>, String> {
    let reader = open_tar_reader(&archive_path)?;
    let mut archive = tar::Archive::new(reader);

    let mut entries = Vec::new();
    for entry_result in archive
        .entries()
        .map_err(|e| format!("Failed to read tar: {}", e))?
    {
        let entry = entry_result.map_err(|e| format!("Failed to read entry: {}", e))?;
        let header = entry.header();

        let name = entry
            .path()
            .map_err(|e| format!("Invalid path: {}", e))?
            .to_string_lossy()
            .to_string();
        let is_dir = header.entry_type().is_dir();
        let size = header.size().unwrap_or(0);
        let modified = header.mtime().ok().map(|ts| {
            chrono::DateTime::from_timestamp(ts as i64, 0)
                .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S").to_string())
                .unwrap_or_default()
        });

        entries.push(ArchiveEntry {
            name,
            size,
            compressed_size: 0,
            is_dir,
            is_encrypted: false,
            modified,
        });
    }

    Ok(entries)
}

#[tauri::command]
pub async fn extract_tar_entry(
    archive_path: String,
    entry_name: String,
    output_path: String,
    app: tauri::AppHandle,
) -> Result<String, String> {
    use std::fs::{self, File};

    // M16: Validate entry name before extraction to prevent path traversal
    if !is_safe_archive_entry(&entry_name) {
        return Err(format!(
            "Unsafe archive entry path rejected: {}",
            entry_name
        ));
    }

    let reader = open_tar_reader(&archive_path)?;
    let mut archive = tar::Archive::new(reader);

    let out_path = std::path::Path::new(&output_path);
    if let Some(parent) = out_path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("Failed to create directory: {}", e))?;
    }

    for entry_result in archive
        .entries()
        .map_err(|e| format!("Failed to read tar: {}", e))?
    {
        let mut entry = entry_result.map_err(|e| format!("Failed to read entry: {}", e))?;
        let path = entry
            .path()
            .map_err(|e| format!("Invalid path: {}", e))?
            .to_string_lossy()
            .to_string();

        if path == entry_name {
            // Atomic write: extract to .tmp then rename to prevent partial files
            let total = entry.header().size().unwrap_or(0);
            let mut progress = ArchiveProgress::for_app(app, phase::EXTRACTING, total);
            let tmp_path = out_path.with_extension("aerotmp");
            let mut outfile =
                File::create(&tmp_path).map_err(|e| format!("Failed to create file: {}", e))?;
            {
                let mut counted = ProgressReader::new(&mut entry, &mut progress);
                if let Err(e) = std::io::copy(&mut counted, &mut outfile) {
                    let _ = fs::remove_file(&tmp_path);
                    return Err(format!("Failed to extract: {}", e));
                }
            }
            drop(outfile);
            fs::rename(&tmp_path, out_path)
                .map_err(|e| format!("Failed to finalize extracted file: {}", e))?;
            progress.finish();
            return Ok(output_path);
        }
    }

    Err(format!("Entry '{}' not found in archive", entry_name))
}

// ─── RAR ───────────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn list_rar(archive_path: String) -> Result<Vec<ArchiveEntry>, String> {
    let archive = unrar::Archive::new(&archive_path)
        .open_for_listing()
        .map_err(|e| format!("Failed to open RAR archive: {}", e))?;

    let mut entries = Vec::new();
    for entry_result in archive {
        let entry = entry_result.map_err(|e| format!("Failed to read entry: {}", e))?;
        entries.push(ArchiveEntry {
            name: entry.filename.to_string_lossy().to_string(),
            size: entry.unpacked_size,
            compressed_size: 0, // RAR crate doesn't expose packed_size directly
            is_dir: entry.is_directory(),
            is_encrypted: entry.is_encrypted(),
            modified: None,
        });
    }

    Ok(entries)
}

#[tauri::command]
pub async fn extract_rar_entry(
    archive_path: String,
    entry_name: String,
    output_path: String,
    password: Option<String>,
    app: tauri::AppHandle,
) -> Result<String, String> {
    // M16: Validate entry name before extraction to prevent path traversal
    if !is_safe_archive_entry(&entry_name) {
        return Err(format!(
            "Unsafe archive entry path rejected: {}",
            entry_name
        ));
    }

    let secret_password: Option<SecretString> = password.map(SecretString::from);

    let out_dir = std::path::Path::new(&output_path)
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .to_string_lossy()
        .to_string();

    std::fs::create_dir_all(&out_dir).map_err(|e| format!("Failed to create directory: {}", e))?;

    let archive = if let Some(ref pwd) = secret_password {
        unrar::Archive::with_password(&archive_path, pwd.expose_secret().as_bytes())
    } else {
        unrar::Archive::new(&archive_path)
    };

    let mut archive = archive
        .open_for_processing()
        .map_err(|e| format!("Failed to open RAR archive: {}", e))?;

    while let Some(header) = archive
        .read_header()
        .map_err(|e| format!("Failed to read header: {}", e))?
    {
        let entry_path = header.entry().filename.to_string_lossy().to_string();
        if entry_path == entry_name {
            // RAR extracts in a single opaque call (no byte hook), so show an honest
            // indeterminate bar rather than a faked percentage (HANDOFF section 3.6).
            let total = header.entry().unpacked_size;
            let mut progress =
                ArchiveProgress::indeterminate_for_app(app, phase::EXTRACTING, total);
            header
                .extract_to(&output_path)
                .map_err(|e| format!("Failed to extract entry: {}", e))?;
            progress.finish();
            return Ok(output_path);
        } else {
            archive = header
                .skip()
                .map_err(|e| format!("Failed to skip entry: {}", e))?;
        }
    }

    Err(format!("Entry '{}' not found in archive", entry_name))
}
