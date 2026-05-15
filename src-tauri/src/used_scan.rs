// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Explicit recursive "used storage" scan (item 4b).
//!
//! Completes item 4a for backends with no quota-total API (raw
//! FTP/FTPS/SFTP, most S3/WebDAV) and generic cloud: it sums the byte size
//! of every file under a root so the manual total (or an API total) can be
//! turned into a real used/total/% figure.
//!
//! Method (agreed design, see docs/dev/DESIGN-2026-05-15_item4b-used-scan.md):
//! a single shared BFS is the universal baseline because every backend's
//! `list()` already returns the size inline (SFTP readdir attrs, FTP MLSD
//! `size=`, WebDAV `getcontentlength`, S3 `Size`, cloud APIs) so no
//! per-file `stat()` is ever needed. Two narrow specializations collapse
//! the per-directory round-trips where the protocol offers a single-shot
//! recursive listing: S3 `list_recursive` (flat ListObjectsV2) and WebDAV
//! `PROPFIND Depth: infinity` (with automatic BFS fallback when the server
//! rejects or limits it). Everything else uses the shared BFS.
//!
//! NEVER called automatically on connect: the caller (CLI `df --scan`, GUI
//! "Calculate used storage" action) triggers it explicitly.

use crate::providers::{ProviderError, S3Provider, StorageProvider, WebDavProvider};
use std::sync::atomic::{AtomicBool, Ordering};

/// Outcome of a used-storage scan.
#[derive(Debug, Clone)]
pub struct UsedScan {
    /// Sum of file sizes under the scanned root, in bytes.
    pub used_bytes: u64,
    /// Number of files counted.
    pub file_count: u64,
    /// Number of directories traversed.
    pub dir_count: u64,
    /// True when a cap (depth/entries) or cancellation stopped the scan
    /// early, so `used_bytes` is a lower bound.
    pub truncated: bool,
    /// Which method produced the figure: "s3-list-recursive",
    /// "webdav-infinity" or "bfs". Surfaced for the tooltip/logs.
    pub method: &'static str,
}

/// Recursively sum the bytes used under `root`.
///
/// `max_depth` / `max_entries` reuse the project-wide scan caps. `cancel`
/// is polled between directories so the GUI cancel button (and double
/// Ctrl+C in the CLI) takes effect promptly. `on_progress(files, bytes)`
/// is called as the figure grows so the caller can render a spinner.
pub async fn scan_used_bytes(
    provider: &mut Box<dyn StorageProvider>,
    root: &str,
    max_depth: usize,
    max_entries: usize,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(u64, u64),
) -> Result<UsedScan, ProviderError> {
    // --- Specialization 1: S3 flat recursive listing -------------------
    if let Some(s3) = provider.as_any_mut().downcast_mut::<S3Provider>() {
        let entries = s3.list_recursive(root).await?;
        let mut used = 0u64;
        let mut files = 0u64;
        let mut dirs = 0u64;
        let mut truncated = false;
        for e in entries {
            if e.is_dir {
                dirs += 1;
                continue;
            }
            if files >= max_entries as u64 {
                truncated = true;
                break;
            }
            used = used.saturating_add(e.size);
            files += 1;
        }
        on_progress(files, used);
        return Ok(UsedScan {
            used_bytes: used,
            file_count: files,
            dir_count: dirs,
            truncated,
            method: "s3-list-recursive",
        });
    }

    // --- Specialization 2: WebDAV PROPFIND Depth: infinity -------------
    // Attempt the single-request recursive listing; on any error (server
    // forbids/limits infinity) fall through to the shared BFS so the
    // figure stays correct.
    if let Some(dav) = provider.as_any_mut().downcast_mut::<WebDavProvider>() {
        match dav.list_recursive(root).await {
            Ok(entries) => {
                let mut used = 0u64;
                let mut files = 0u64;
                let mut dirs = 0u64;
                let mut truncated = false;
                for e in entries {
                    if e.is_dir {
                        dirs += 1;
                        continue;
                    }
                    if files >= max_entries as u64 {
                        truncated = true;
                        break;
                    }
                    used = used.saturating_add(e.size);
                    files += 1;
                }
                // Some servers (CloudMe, DriveHQ, jianguoyun) answer 207 to
                // Depth:infinity but do NOT recurse: they return only the
                // requested collection (and maybe its immediate
                // subdirectories), so files==0 even though the tree has
                // files. Trust infinity ONLY when it actually found files;
                // otherwise fall back to the BFS, which walks explicitly
                // and returns the true figure. A genuinely file-less tree
                // also yields 0 via BFS, so the fallback is harmless.
                if files > 0 {
                    on_progress(files, used);
                    return Ok(UsedScan {
                        used_bytes: used,
                        file_count: files,
                        dir_count: dirs,
                        truncated,
                        method: "webdav-infinity",
                    });
                }
                tracing::info!(
                    "[used_scan] WebDAV Depth:infinity returned no entries (likely not recursed), falling back to BFS"
                );
            }
            Err(e) => {
                tracing::info!(
                    "[used_scan] WebDAV Depth:infinity unavailable ({}), falling back to BFS",
                    e
                );
            }
        }
    }

    // --- Baseline: shared provider-agnostic BFS ------------------------
    bfs_used_bytes(provider, root, max_depth, max_entries, cancel, &mut on_progress).await
}

/// Provider-agnostic breadth-first sum. One `list()` per directory; size
/// comes inline with each entry for every backend, so this never issues a
/// per-file metadata call.
async fn bfs_used_bytes(
    provider: &mut Box<dyn StorageProvider>,
    root: &str,
    max_depth: usize,
    max_entries: usize,
    cancel: &AtomicBool,
    on_progress: &mut impl FnMut(u64, u64),
) -> Result<UsedScan, ProviderError> {
    let mut used = 0u64;
    let mut files = 0u64;
    let mut dirs = 0u64;
    let mut truncated = false;
    // (absolute path, depth). LIFO is fine: we only sum, order is irrelevant.
    let mut queue: Vec<(String, usize)> = vec![(root.to_string(), 0)];

    while let Some((dir, depth)) = queue.pop() {
        if cancel.load(Ordering::Relaxed) {
            truncated = true;
            break;
        }
        if depth >= max_depth || (files + dirs) >= max_entries as u64 {
            truncated = true;
            continue;
        }
        match provider.list(&dir).await {
            Ok(entries) => {
                for entry in entries {
                    if entry.is_dir {
                        dirs += 1;
                        queue.push((entry.path.clone(), depth + 1));
                        continue;
                    }
                    if files >= max_entries as u64 {
                        truncated = true;
                        break;
                    }
                    used = used.saturating_add(entry.size);
                    files += 1;
                }
                on_progress(files, used);
            }
            Err(e) => {
                // A single unreadable directory should not abort the whole
                // figure: log and keep going (the result is a lower bound).
                tracing::warn!("[used_scan] failed to list {}: {}", dir, e);
                truncated = true;
            }
        }
    }

    Ok(UsedScan {
        used_bytes: used,
        file_count: files,
        dir_count: dirs,
        truncated,
        method: "bfs",
    })
}
