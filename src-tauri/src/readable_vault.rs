//! Read-only abstraction over an UNLOCKED vault, plus the Save-All exporters.
//!
//! AeroMount "Save all..." (#322, Ehud idea #1): once a user has unlocked a
//! Cryptomator vault or opened an `.aerovault` / `.aerozip`, they can write the
//! whole decrypted tree out in one shot (to a folder, a `.zip`, or a `.aerozip`)
//! instead of pulling files one at a time. Both container types expose the same
//! conceptual operations (walk the tree, read a file's plaintext), so this module
//! defines ONE internal trait, [`ReadableVault`], with an adapter per container
//! type (`CryptomatorReadable` in `cryptomator.rs`, `VaultV3Readable` in
//! `aerovault_v3.rs`). The generic `.zip` exporter then covers both via the trait;
//! this seam is also what a future read-only mount (Deliverable B) reuses.
//!
//! SECURITY: every exporter writes PLAINTEXT to a caller-chosen location (the
//! user's explicit intent). The `.zip` target is built entry-by-entry so no
//! whole-tree plaintext temp dir is spilled, and entry paths are validated so a
//! hostile or corrupt container cannot escape the chosen destination. The
//! `.aerozip` target stages through a `TempDir` that auto-scrubs on drop (its
//! output is plaintext anyway).

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use std::io::Write;
use std::path::{Component, Path, PathBuf};

/// One node in a readable vault's tree, addressed by a forward-slash path
/// relative to the vault root.
pub struct ReadableEntry {
    /// Forward-slash path relative to the vault root (e.g. `a/b/file.txt`).
    pub rel_path: String,
    pub is_dir: bool,
    pub size: u64,
    /// Container-private file locator the adapter uses to read the file back
    /// without re-resolving `rel_path` (the Cryptomator adapter stores the
    /// parent `dir_id` here; the `.aerovault` adapter leaves it empty and uses
    /// `rel_path`).
    pub handle: String,
}

/// Outcome of a Save-All export. `skipped` carries one `path: reason` line per
/// entry that could not be exported (mirrors `CryptomatorIngestReport`) so a
/// single bad entry never aborts the whole export silently.
#[derive(serde::Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExportReport {
    pub files: u64,
    pub dirs: u64,
    pub skipped: Vec<String>,
}

/// Read-only view of an unlocked vault: walk the tree and stream a file's
/// plaintext. The single seam both Save-All and a future read-only mount share.
pub trait ReadableVault {
    /// Recursively enumerate every entry (directories and files), parents before
    /// children.
    fn walk(&self) -> Result<Vec<ReadableEntry>, String>;
    /// Stream a single file entry's plaintext into `sink`.
    fn read_file(&self, entry: &ReadableEntry, sink: &mut dyn Write) -> Result<(), String>;
}

/// Reject any vault entry path that is absolute or carries a `..` / `.` / root /
/// prefix component, so a hostile or corrupt container cannot make an export
/// escape the chosen destination.
fn validate_rel_path(rel_path: &str) -> Result<(), String> {
    if rel_path.is_empty() {
        return Err("empty entry path".to_string());
    }
    for comp in Path::new(rel_path).components() {
        if !matches!(comp, Component::Normal(_)) {
            return Err(format!("unsafe path component in '{rel_path}'"));
        }
    }
    Ok(())
}

/// Join a validated relative path under `dest` (containment-checked).
fn safe_join(dest: &Path, rel_path: &str) -> Result<PathBuf, String> {
    validate_rel_path(rel_path)?;
    Ok(dest.join(rel_path))
}

/// Read one file entry fully into memory (both backends already decrypt
/// whole-file), so the caller can write a complete archive member or skip the
/// entry on failure without leaving a partial one behind. The capacity hint is
/// capped so a bogus manifest size cannot pre-allocate an enormous buffer.
fn read_file_to_vec(vault: &dyn ReadableVault, entry: &ReadableEntry) -> Result<Vec<u8>, String> {
    let cap = entry.size.min(64 * 1024 * 1024) as usize;
    let mut buf: Vec<u8> = Vec::with_capacity(cap);
    vault.read_file(entry, &mut buf)?;
    Ok(buf)
}

/// Export every file in `vault` into `dest` (a folder), recreating the tree.
/// Streams each file straight to disk (memory-light). Per-entry failures are
/// recorded in the report and do not abort the export.
pub fn export_to_folder(
    vault: &dyn ReadableVault,
    dest: &Path,
    on_progress: &mut dyn FnMut(u64, u64),
) -> Result<ExportReport, String> {
    use std::fs;
    use std::io::BufWriter;

    let entries = vault.walk()?;
    let total_bytes: u64 = entries.iter().filter(|e| !e.is_dir).map(|e| e.size).sum();
    fs::create_dir_all(dest).map_err(|e| format!("Create destination: {e}"))?;

    let mut report = ExportReport::default();
    let mut done_bytes = 0u64;
    for entry in &entries {
        let out = match safe_join(dest, &entry.rel_path) {
            Ok(p) => p,
            Err(e) => {
                report.skipped.push(e);
                continue;
            }
        };
        if entry.is_dir {
            match fs::create_dir_all(&out) {
                Ok(_) => report.dirs += 1,
                Err(e) => report.skipped.push(format!("{}: {e}", entry.rel_path)),
            }
            continue;
        }
        let write_res = (|| -> Result<(), String> {
            if let Some(parent) = out.parent() {
                fs::create_dir_all(parent).map_err(|e| format!("create dir: {e}"))?;
            }
            let f = fs::File::create(&out).map_err(|e| format!("create: {e}"))?;
            let mut w = BufWriter::new(f);
            vault.read_file(entry, &mut w)?;
            w.flush().map_err(|e| format!("flush: {e}"))?;
            Ok(())
        })();
        match write_res {
            Ok(_) => report.files += 1,
            Err(e) => report.skipped.push(format!("{}: {e}", entry.rel_path)),
        }
        done_bytes = done_bytes.saturating_add(entry.size);
        on_progress(done_bytes, total_bytes);
    }
    Ok(report)
}

/// Export every file in `vault` into a single plaintext `.zip` at `dest`. Each
/// entry is buffered then written, so a read failure skips just that entry (no
/// partial zip member) and no plaintext temp dir is spilled.
pub fn export_to_zip(
    vault: &dyn ReadableVault,
    dest: &Path,
    on_progress: &mut dyn FnMut(u64, u64),
) -> Result<ExportReport, String> {
    use zip::write::SimpleFileOptions;
    use zip::ZipWriter;

    let entries = vault.walk()?;
    let total_bytes: u64 = entries.iter().filter(|e| !e.is_dir).map(|e| e.size).sum();
    let f = std::fs::File::create(dest).map_err(|e| format!("Create zip: {e}"))?;
    let mut zip = ZipWriter::new(std::io::BufWriter::new(f));
    let opts = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .compression_level(Some(6));

    let mut report = ExportReport::default();
    let mut done_bytes = 0u64;
    for entry in &entries {
        if let Err(e) = validate_rel_path(&entry.rel_path) {
            report.skipped.push(e);
            continue;
        }
        if entry.is_dir {
            match zip.add_directory(format!("{}/", entry.rel_path), opts) {
                Ok(_) => report.dirs += 1,
                Err(e) => report.skipped.push(format!("{}: {e}", entry.rel_path)),
            }
            continue;
        }
        match read_file_to_vec(vault, entry) {
            Ok(bytes) => {
                let w = (|| -> Result<(), String> {
                    zip.start_file(entry.rel_path.clone(), opts)
                        .map_err(|e| format!("zip start: {e}"))?;
                    zip.write_all(&bytes)
                        .map_err(|e| format!("zip write: {e}"))?;
                    Ok(())
                })();
                match w {
                    Ok(_) => report.files += 1,
                    Err(e) => report.skipped.push(format!("{}: {e}", entry.rel_path)),
                }
            }
            Err(e) => report.skipped.push(format!("{}: {e}", entry.rel_path)),
        }
        done_bytes = done_bytes.saturating_add(entry.size);
        on_progress(done_bytes, total_bytes);
    }
    zip.finish().map_err(|e| format!("Finalize zip: {e}"))?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    /// A tiny in-memory ReadableVault for exercising the generic exporters
    /// without a real crypto backend.
    struct FakeVault {
        entries: Vec<(String, bool, Vec<u8>)>, // (rel_path, is_dir, bytes)
    }
    impl ReadableVault for FakeVault {
        fn walk(&self) -> Result<Vec<ReadableEntry>, String> {
            Ok(self
                .entries
                .iter()
                .map(|(p, is_dir, bytes)| ReadableEntry {
                    rel_path: p.clone(),
                    is_dir: *is_dir,
                    size: bytes.len() as u64,
                    handle: String::new(),
                })
                .collect())
        }
        fn read_file(&self, entry: &ReadableEntry, sink: &mut dyn Write) -> Result<(), String> {
            let (_, _, bytes) = self
                .entries
                .iter()
                .find(|(p, _, _)| *p == entry.rel_path)
                .ok_or("not found")?;
            sink.write_all(bytes).map_err(|e| e.to_string())
        }
    }

    fn sample() -> FakeVault {
        FakeVault {
            entries: vec![
                ("a".to_string(), true, vec![]),
                ("a/hello.txt".to_string(), false, b"hello".to_vec()),
                ("top.bin".to_string(), false, (0u8..=255).collect()),
            ],
        }
    }

    #[test]
    fn folder_export_recreates_tree_byte_identical() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("out");
        let report = export_to_folder(&sample(), &dest, &mut |_, _| {}).unwrap();
        assert_eq!(report.files, 2);
        assert_eq!(report.dirs, 1);
        assert!(report.skipped.is_empty());
        assert_eq!(std::fs::read(dest.join("a/hello.txt")).unwrap(), b"hello");
        assert_eq!(
            std::fs::read(dest.join("top.bin")).unwrap(),
            (0u8..=255).collect::<Vec<u8>>()
        );
    }

    #[test]
    fn zip_export_contains_every_file_byte_identical() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("out.zip");
        let report = export_to_zip(&sample(), &dest, &mut |_, _| {}).unwrap();
        assert_eq!(report.files, 2);
        assert!(report.skipped.is_empty());

        let f = std::fs::File::open(&dest).unwrap();
        let mut archive = zip::ZipArchive::new(f).unwrap();
        let mut hello = String::new();
        archive
            .by_name("a/hello.txt")
            .unwrap()
            .read_to_string(&mut hello)
            .unwrap();
        assert_eq!(hello, "hello");
        let mut top = Vec::new();
        archive
            .by_name("top.bin")
            .unwrap()
            .read_to_end(&mut top)
            .unwrap();
        assert_eq!(top, (0u8..=255).collect::<Vec<u8>>());
    }

    #[test]
    fn unsafe_paths_are_skipped_not_written() {
        let evil = FakeVault {
            entries: vec![("../escape.txt".to_string(), false, b"x".to_vec())],
        };
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("out");
        let report = export_to_folder(&evil, &dest, &mut |_, _| {}).unwrap();
        assert_eq!(report.files, 0);
        assert_eq!(report.skipped.len(), 1);
        assert!(!tmp.path().join("escape.txt").exists());
    }
}
