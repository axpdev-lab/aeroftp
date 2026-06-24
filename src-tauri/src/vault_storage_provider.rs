//! Read-only `StorageProvider` over an unlocked vault, for AeroMount Deliverable B
//! (#322, Ehud idea #1).
//!
//! Both the FUSE engine (`AeroFuseFs`, Linux/macOS) and the Windows WebDAV bridge
//! consume a [`StorageProvider`]. This module adapts a [`ReadableVault`] (the
//! Save-All seam: an unlocked Cryptomator vault or an opened `.aerovault` /
//! `.aerozip`) to that interface, so the EXISTING mount engine can surface a
//! decrypted vault as a browsable, READ-ONLY filesystem with no engine change.
//!
//! Posture (owner-gated, design 4.3 + 6):
//! - READ-ONLY: every mutating op returns `NotSupported`; the FUSE mount is also
//!   started with `read_only`/`MountOption::RO`, so write attempts fail twice.
//! - The whole tree is snapshotted once at construction (`ReadableVault::walk`),
//!   so listings never re-walk the backing store and parent directories are
//!   always present even if a backend lists only leaf files.
//! - Random reads: a seekable backend (Cryptomator, independent 32 KiB GCM
//!   chunks) serves `read_range` directly. A whole-file backend (`.aerovault`,
//!   the crate reads whole-file today) serves small files whole and caches large
//!   files once into a `0700` plaintext temp dir that auto-scrubs on drop, so a
//!   multi-GB file is decrypted at most once instead of per `read`.
//!
//! SECURITY: a mounted vault exposes decrypted bytes to any process running as
//! the user (mountpoint is `0700`, no `allow_other`). The temp cache holds
//! plaintext on disk for the mount's lifetime; it lives in an `O_EXCL`/`0700`
//! temp dir removed on unmount (provider drop / `disconnect`).

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::providers::types::{ProviderError, ProviderType, RemoteEntry};
use crate::providers::StorageProvider;
use crate::readable_vault::{ReadableEntry, ReadableVault};

/// Files larger than this are served from the first-access plaintext temp cache
/// on a whole-file (non-seekable) backend, instead of decrypting the whole file
/// on every `read`. Small files fall back to a cheap whole-file slice.
const TEMP_CACHE_THRESHOLD: u64 = 8 * 1024 * 1024;

/// Per-file metadata captured from the one-shot `walk`.
struct EntryMeta {
    is_dir: bool,
    size: u64,
    /// Container-private locator (Cryptomator parent `dir_id`; empty for
    /// `.aerovault`), echoed back into a `ReadableEntry` for `read`.
    handle: String,
}

pub struct VaultStorageProvider {
    name: String,
    /// The unlocked-vault backend behind a `Mutex` so the provider is `Sync`
    /// (the `.aerovault` crate handle is not). All backend-touching methods take
    /// `&mut self`, so they use `get_mut()` and never actually contend the lock.
    backend: Mutex<Box<dyn ReadableVault>>,
    seekable: bool,
    /// rel_path (no leading slash, `""` = root) -> metadata.
    entries: HashMap<String, EntryMeta>,
    /// parent rel_path -> direct children, prebuilt as `RemoteEntry` for `list`.
    children: HashMap<String, Vec<RemoteEntry>>,
    /// First-access plaintext temp cache for large files on a whole-file backend.
    cache_dir: Mutex<Option<tempfile::TempDir>>,
    file_cache: Mutex<HashMap<String, PathBuf>>,
}

/// Normalise a FUSE/WebDAV path (`/`, `/a`, `/a/b.txt`) to a vault rel_path
/// (`""`, `a`, `a/b.txt`).
fn to_rel(path: &str) -> String {
    path.trim_matches('/').to_string()
}

impl VaultStorageProvider {
    /// Build a provider over an unlocked-vault backend, snapshotting the whole
    /// tree once. Fails only if the initial `walk` fails.
    pub fn new(name: String, backend: Box<dyn ReadableVault>) -> Result<Self, String> {
        let seekable = backend.supports_seek();
        let walked = backend.walk()?;

        let mut entries: HashMap<String, EntryMeta> = HashMap::new();
        let mut children: HashMap<String, Vec<RemoteEntry>> = HashMap::new();
        children.entry(String::new()).or_default();

        // Create a directory (and every missing ancestor), idempotently.
        fn ensure_dir(
            rel: &str,
            entries: &mut HashMap<String, EntryMeta>,
            children: &mut HashMap<String, Vec<RemoteEntry>>,
        ) {
            if rel.is_empty() {
                return;
            }
            if matches!(entries.get(rel), Some(m) if m.is_dir) {
                return;
            }
            let (parent, name) = split_rel(rel);
            ensure_dir(parent, entries, children);
            entries.insert(
                rel.to_string(),
                EntryMeta {
                    is_dir: true,
                    size: 0,
                    handle: String::new(),
                },
            );
            children
                .entry(parent.to_string())
                .or_default()
                .push(RemoteEntry::directory(name.to_string(), format!("/{rel}")));
            children.entry(rel.to_string()).or_default();
        }

        for e in walked {
            let rel = e.rel_path.trim_matches('/').to_string();
            if rel.is_empty() {
                continue;
            }
            let (parent, name) = split_rel(&rel);
            ensure_dir(parent, &mut entries, &mut children);
            if e.is_dir {
                ensure_dir(&rel, &mut entries, &mut children);
                continue;
            }
            // A file path that collides with an already-seen entry is ignored
            // (first wins) so a malformed listing cannot double-list a name.
            if entries.contains_key(&rel) {
                continue;
            }
            entries.insert(
                rel.clone(),
                EntryMeta {
                    is_dir: false,
                    size: e.size,
                    handle: e.handle,
                },
            );
            children
                .entry(parent.to_string())
                .or_default()
                .push(RemoteEntry::file(
                    name.to_string(),
                    format!("/{rel}"),
                    e.size,
                ));
        }

        Ok(Self {
            name,
            backend: Mutex::new(backend),
            seekable,
            entries,
            children,
            cache_dir: Mutex::new(None),
            file_cache: Mutex::new(HashMap::new()),
        })
    }

    fn lookup(&self, rel: &str) -> Option<&EntryMeta> {
        self.entries.get(rel)
    }

    fn make_readable_entry(&self, rel: &str, meta: &EntryMeta) -> ReadableEntry {
        ReadableEntry {
            rel_path: rel.to_string(),
            is_dir: meta.is_dir,
            size: meta.size,
            handle: meta.handle.clone(),
        }
    }

    /// Ensure a whole-file (non-seekable) backend has the file decrypted once
    /// into the `0700` temp cache, returning the on-disk plaintext path.
    fn ensure_cached(
        &mut self,
        rel: &str,
        entry: &ReadableEntry,
    ) -> Result<PathBuf, ProviderError> {
        if let Some(p) = self.file_cache.lock().unwrap().get(rel) {
            return Ok(p.clone());
        }
        // Lazily create the 0700 temp dir on first large read.
        {
            let mut guard = self.cache_dir.lock().unwrap();
            if guard.is_none() {
                let dir = tempfile::Builder::new()
                    .prefix(".aeromount-cache-")
                    .tempdir()
                    .map_err(|e| ProviderError::TransferFailed(format!("temp cache dir: {e}")))?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(
                        dir.path(),
                        std::fs::Permissions::from_mode(0o700),
                    );
                }
                *guard = Some(dir);
            }
        }
        let dest = {
            let guard = self.cache_dir.lock().unwrap();
            let dir = guard.as_ref().unwrap();
            // Flatten the rel path into one cache filename (the dir is private).
            let safe: String = rel
                .chars()
                .map(|c| if c == '/' { '_' } else { c })
                .collect();
            dir.path().join(format!(
                "{}__{}",
                self.file_cache.lock().unwrap().len(),
                safe
            ))
        };
        // Decrypt the whole file into the cache file (0600).
        let res = (|| -> Result<(), String> {
            use std::io::Write as _;
            let f = std::fs::File::create(&dest).map_err(|e| format!("create cache: {e}"))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o600));
            }
            let mut w = std::io::BufWriter::new(f);
            self.backend.get_mut().unwrap().read_file(entry, &mut w)?;
            w.flush().map_err(|e| format!("flush cache: {e}"))?;
            Ok(())
        })();
        if let Err(e) = res {
            let _ = std::fs::remove_file(&dest);
            return Err(ProviderError::TransferFailed(e));
        }
        self.file_cache
            .lock()
            .unwrap()
            .insert(rel.to_string(), dest.clone());
        Ok(dest)
    }
}

/// Split a rel_path into `(parent, name)` (`"a/b/c" -> ("a/b", "c")`,
/// `"c" -> ("", "c")`).
fn split_rel(rel: &str) -> (&str, &str) {
    match rel.rfind('/') {
        Some(i) => (&rel[..i], &rel[i + 1..]),
        None => ("", rel),
    }
}

#[async_trait]
impl StorageProvider for VaultStorageProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn provider_type(&self) -> ProviderType {
        ProviderType::AeroVaultMount
    }

    fn display_name(&self) -> String {
        self.name.clone()
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        // The vault is already unlocked at construction; nothing to dial.
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        // Scrub the plaintext temp cache eagerly (also dropped with the provider).
        self.file_cache.lock().unwrap().clear();
        *self.cache_dir.lock().unwrap() = None;
        Ok(())
    }

    fn is_connected(&self) -> bool {
        true
    }

    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let rel = to_rel(path);
        // Root and known directories list their prebuilt children.
        if rel.is_empty() || matches!(self.lookup(&rel), Some(m) if m.is_dir) {
            return Ok(self.children.get(&rel).cloned().unwrap_or_default());
        }
        Err(ProviderError::NotFound(path.to_string()))
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> {
        Ok("/".to_string())
    }

    async fn cd(&mut self, _path: &str) -> Result<(), ProviderError> {
        Ok(())
    }

    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        Ok(())
    }

    async fn download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let rel = to_rel(remote_path);
        let meta = self
            .lookup(&rel)
            .filter(|m| !m.is_dir)
            .ok_or_else(|| ProviderError::NotFound(remote_path.to_string()))?;
        let size = meta.size;
        let entry = self.make_readable_entry(&rel, meta);
        use std::io::Write as _;
        let f = std::fs::File::create(local_path)
            .map_err(|e| ProviderError::TransferFailed(format!("create: {e}")))?;
        let mut w = std::io::BufWriter::new(f);
        self.backend
            .get_mut()
            .unwrap()
            .read_file(&entry, &mut w)
            .map_err(ProviderError::TransferFailed)?;
        w.flush()
            .map_err(|e| ProviderError::TransferFailed(format!("flush: {e}")))?;
        if let Some(cb) = on_progress {
            cb(size, size);
        }
        Ok(())
    }

    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        let rel = to_rel(remote_path);
        let meta = self
            .lookup(&rel)
            .filter(|m| !m.is_dir)
            .ok_or_else(|| ProviderError::NotFound(remote_path.to_string()))?;
        let entry = ReadableEntry {
            rel_path: rel.clone(),
            is_dir: false,
            size: meta.size,
            handle: meta.handle.clone(),
        };
        let cap = meta.size.min(64 * 1024 * 1024) as usize;
        let mut buf = Vec::with_capacity(cap);
        self.backend
            .get_mut()
            .unwrap()
            .read_file(&entry, &mut buf)
            .map_err(ProviderError::TransferFailed)?;
        Ok(buf)
    }

    async fn upload(
        &mut self,
        _local_path: &str,
        _remote_path: &str,
        _on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        Err(read_only())
    }

    async fn mkdir(&mut self, _path: &str) -> Result<(), ProviderError> {
        Err(read_only())
    }

    async fn delete(&mut self, _path: &str) -> Result<(), ProviderError> {
        Err(read_only())
    }

    async fn rmdir(&mut self, _path: &str) -> Result<(), ProviderError> {
        Err(read_only())
    }

    async fn rmdir_recursive(&mut self, _path: &str) -> Result<(), ProviderError> {
        Err(read_only())
    }

    async fn rename(&mut self, _from: &str, _to: &str) -> Result<(), ProviderError> {
        Err(read_only())
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        let rel = to_rel(path);
        if rel.is_empty() {
            return Ok(RemoteEntry::directory("/".to_string(), "/".to_string()));
        }
        let (_, name) = split_rel(&rel);
        match self.lookup(&rel) {
            Some(m) if m.is_dir => Ok(RemoteEntry::directory(name.to_string(), format!("/{rel}"))),
            Some(m) => Ok(RemoteEntry::file(
                name.to_string(),
                format!("/{rel}"),
                m.size,
            )),
            None => Err(ProviderError::NotFound(path.to_string())),
        }
    }

    async fn size(&mut self, path: &str) -> Result<u64, ProviderError> {
        let rel = to_rel(path);
        self.lookup(&rel)
            .map(|m| m.size)
            .ok_or_else(|| ProviderError::NotFound(path.to_string()))
    }

    async fn exists(&mut self, path: &str) -> Result<bool, ProviderError> {
        let rel = to_rel(path);
        Ok(rel.is_empty() || self.lookup(&rel).is_some())
    }

    async fn keep_alive(&mut self) -> Result<(), ProviderError> {
        Ok(())
    }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        Ok("AeroMount: unlocked vault (read-only)".to_string())
    }

    fn supports_delta_sync(&self) -> bool {
        // Advertise range reads so the FUSE engine uses `read_range` (our
        // seekable / temp-cache path) instead of the whole-file fallback.
        true
    }

    async fn read_range(
        &mut self,
        path: &str,
        offset: u64,
        len: u64,
    ) -> Result<Vec<u8>, ProviderError> {
        let rel = to_rel(path);
        let meta = self
            .lookup(&rel)
            .filter(|m| !m.is_dir)
            .ok_or_else(|| ProviderError::NotFound(path.to_string()))?;
        let entry = ReadableEntry {
            rel_path: rel.clone(),
            is_dir: false,
            size: meta.size,
            handle: meta.handle.clone(),
        };
        let want = len.min(u32::MAX as u64) as u32;

        if self.seekable {
            return self
                .backend
                .get_mut()
                .unwrap()
                .read_at(&entry, offset, want)
                .map_err(ProviderError::TransferFailed);
        }

        // Whole-file backend: small files slice directly; large files go through
        // the first-access plaintext temp cache so they decrypt at most once.
        if meta.size <= TEMP_CACHE_THRESHOLD {
            return self
                .backend
                .get_mut()
                .unwrap()
                .read_at(&entry, offset, want)
                .map_err(ProviderError::TransferFailed);
        }

        let cached = self.ensure_cached(&rel, &entry)?;
        read_window_from_file(&cached, offset, want)
    }
}

/// Read up to `len` bytes from `path` at `offset` (EOF-clamped), for the temp
/// cache path.
fn read_window_from_file(path: &PathBuf, offset: u64, len: u32) -> Result<Vec<u8>, ProviderError> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path)
        .map_err(|e| ProviderError::TransferFailed(format!("open cache: {e}")))?;
    let file_len = f
        .metadata()
        .map_err(|e| ProviderError::TransferFailed(format!("stat cache: {e}")))?
        .len();
    if offset >= file_len {
        return Ok(Vec::new());
    }
    f.seek(SeekFrom::Start(offset))
        .map_err(|e| ProviderError::TransferFailed(format!("seek cache: {e}")))?;
    let want = (file_len - offset).min(len as u64) as usize;
    let mut buf = vec![0u8; want];
    f.read_exact(&mut buf)
        .map_err(|e| ProviderError::TransferFailed(format!("read cache: {e}")))?;
    Ok(buf)
}

fn read_only() -> ProviderError {
    ProviderError::NotSupported("read-only mount".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    struct FakeVault {
        entries: Vec<(String, bool, Vec<u8>)>,
        seekable: bool,
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
        fn supports_seek(&self) -> bool {
            self.seekable
        }
    }

    fn provider(seekable: bool) -> VaultStorageProvider {
        let v = FakeVault {
            entries: vec![
                ("a".into(), true, vec![]),
                ("a/hello.txt".into(), false, b"hello world".to_vec()),
                ("top.bin".into(), false, (0u8..=255).collect()),
            ],
            seekable,
        };
        VaultStorageProvider::new("test".into(), Box::new(v)).unwrap()
    }

    #[tokio::test]
    async fn lists_root_and_subdir() {
        let mut p = provider(true);
        let root = p.list("/").await.unwrap();
        let names: Vec<_> = root.iter().map(|e| e.name.clone()).collect();
        assert!(names.contains(&"a".to_string()));
        assert!(names.contains(&"top.bin".to_string()));
        let sub = p.list("/a").await.unwrap();
        assert_eq!(sub.len(), 1);
        assert_eq!(sub[0].name, "hello.txt");
        assert!(!sub[0].is_dir);
    }

    #[tokio::test]
    async fn stat_and_size() {
        let mut p = provider(true);
        let st = p.stat("/a/hello.txt").await.unwrap();
        assert_eq!(st.size, 11);
        assert!(!st.is_dir);
        assert_eq!(p.size("/top.bin").await.unwrap(), 256);
        assert!(p.stat("/a").await.unwrap().is_dir);
    }

    #[tokio::test]
    async fn read_range_window_seekable() {
        let mut p = provider(true);
        let win = p.read_range("/a/hello.txt", 6, 5).await.unwrap();
        assert_eq!(win, b"world");
        // Past EOF -> empty.
        assert!(p
            .read_range("/a/hello.txt", 100, 5)
            .await
            .unwrap()
            .is_empty());
        // Clamped at EOF.
        let tail = p.read_range("/top.bin", 254, 10).await.unwrap();
        assert_eq!(tail, vec![254u8, 255u8]);
    }

    #[tokio::test]
    async fn read_range_window_wholefile() {
        // Non-seekable backend uses the default read_at (whole-file slice) for
        // these small files; same observable result.
        let mut p = provider(false);
        let win = p.read_range("/a/hello.txt", 0, 5).await.unwrap();
        assert_eq!(win, b"hello");
    }

    #[tokio::test]
    async fn writes_are_rejected() {
        let mut p = provider(true);
        assert!(p.mkdir("/x").await.is_err());
        assert!(p.delete("/top.bin").await.is_err());
        assert!(p.upload("/tmp/x", "/y", None).await.is_err());
    }
}
