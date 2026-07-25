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
    /// True when the figure is a lower bound for ANY reason. Kept as the
    /// single boolean callers can branch on; the fields below say WHY.
    pub truncated: bool,
    /// Directories the walk could not list at all (permission denied, or a
    /// transport error). Their contents are missing from `used_bytes`.
    ///
    /// Split out from `truncated` because the remedy is the opposite of the
    /// cap case: raising `max_depth` or `max_entries` cannot help here, and a
    /// message that blames the caps sends the user to the wrong knob. Observed
    /// on 2026-07-25 scanning a directory owned by another user, where the
    /// scan reported "capped (depth 100 / 500000 entries)" after reading
    /// exactly zero entries.
    pub unreadable_dirs: u64,
    /// True when a depth or entry cap stopped the walk. This is the case the
    /// caps message is actually about.
    pub hit_cap: bool,
    /// True when the caller cancelled the scan (Ctrl-C, UI abort).
    pub cancelled: bool,
    /// Which method produced the figure: "s3-list-recursive",
    /// "webdav-infinity" or "bfs". Surfaced for the tooltip/logs.
    pub method: &'static str,
}

/// Result of a provider-native single-shot recursive listing.
pub struct FastPathListing {
    /// Every entry under the scanned root, returned in one (paginated) call.
    pub entries: Vec<crate::providers::RemoteEntry>,
    /// Which provider fast-path produced this: `"s3-list-recursive"` or
    /// `"webdav-infinity"`. Surfaced as `UsedScan::method`.
    pub method: &'static str,
    /// True when every entry carries a full, structurally-correct path under
    /// the scan root, so a relative path can be derived by stripping the
    /// root prefix (S3 `list_recursive` returns full object keys).
    ///
    /// False when the provider's recursive listing flattens nested paths:
    /// `WebDavProvider::list_recursive` builds each entry's `name`/`path`
    /// from `base + displayname`, so a deep `a/b/c.txt` collapses to
    /// `root/c.txt`. That is harmless for a byte-sum (only `size`/`is_dir`
    /// are read) but unusable for a structured per-file compare — a
    /// `false` here tells `sync_core::scan` to fall back to the BFS.
    pub structured_paths: bool,
}

/// Try a provider-native single-shot recursive listing.
///
/// Returns `Some` when the provider exposes one and it succeeded:
/// - S3 → flat `ListObjectsV2` with no delimiter (`structured_paths = true`).
/// - WebDAV → `PROPFIND Depth: infinity` (`structured_paths = false`; see
///   `FastPathListing::structured_paths`), and only when the server actually
///   recursed — some servers answer 207 without recursing, yielding zero
///   files, in which case `None` is returned so the caller walks the BFS.
///
/// Returns `None` for every other provider, and on any fast-path error, so
/// the caller falls back to the provider-agnostic BFS. This is the single
/// shared specialization point: `used_scan` and `sync_core::scan` both call
/// it instead of duplicating the downcast logic.
pub async fn provider_list_recursive_fastpath(
    provider: &mut Box<dyn StorageProvider>,
    root: &str,
) -> Option<FastPathListing> {
    // --- S3 flat recursive listing -------------------------------------
    if let Some(s3) = provider.as_any_mut().downcast_mut::<S3Provider>() {
        return match s3.list_recursive(root).await {
            Ok(entries) => Some(FastPathListing {
                entries,
                method: "s3-list-recursive",
                structured_paths: true,
            }),
            Err(e) => {
                tracing::info!(
                    "[fastpath] S3 list_recursive failed ({}), falling back to BFS",
                    e
                );
                None
            }
        };
    }

    // --- WebDAV PROPFIND Depth: infinity -------------------------------
    if let Some(dav) = provider.as_any_mut().downcast_mut::<WebDavProvider>() {
        return match dav.list_recursive(root).await {
            Ok(entries) => {
                // Some servers (CloudMe, DriveHQ, jianguoyun) answer 207 to
                // Depth:infinity but do NOT recurse, returning only the
                // requested collection. Trust infinity ONLY when it found
                // files; otherwise fall back to the BFS.
                let files = entries.iter().filter(|e| !e.is_dir).count();
                if files > 0 {
                    Some(FastPathListing {
                        entries,
                        method: "webdav-infinity",
                        structured_paths: false,
                    })
                } else {
                    tracing::info!(
                        "[fastpath] WebDAV Depth:infinity returned no files (likely not recursed), falling back to BFS"
                    );
                    None
                }
            }
            Err(e) => {
                tracing::info!(
                    "[fastpath] WebDAV Depth:infinity unavailable ({}), falling back to BFS",
                    e
                );
                None
            }
        };
    }

    None
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
    // --- Provider-native single-shot recursive listing -----------------
    // S3 / WebDAV collapse the per-directory BFS round-trips. The shared
    // helper handles the downcast + the WebDAV not-recursed guard; any
    // failure falls through to the BFS so the figure stays correct.
    if let Some(fast) = provider_list_recursive_fastpath(provider, root).await {
        let mut used = 0u64;
        let mut files = 0u64;
        let mut dirs = 0u64;
        let mut truncated = false;
        let mut hit_cap = false;
        // The provider handed us a complete single-shot listing, so there is no
        // per-directory walk here: nothing can be unreadable and nothing polls
        // cancellation. Fixed at their empty values rather than tracked.
        let unreadable_dirs: u64 = 0;
        let cancelled = false;
        for e in fast.entries {
            if e.is_dir {
                dirs += 1;
                continue;
            }
            if files >= max_entries as u64 {
                truncated = true;
                hit_cap = true;
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
            unreadable_dirs,
            hit_cap,
            cancelled,
            method: fast.method,
        });
    }

    // --- Baseline: shared provider-agnostic BFS ------------------------
    bfs_used_bytes(
        provider,
        root,
        max_depth,
        max_entries,
        cancel,
        &mut on_progress,
    )
    .await
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
    let mut unreadable_dirs: u64 = 0;
    let mut hit_cap = false;
    let mut cancelled = false;
    // (absolute path, depth). LIFO is fine: we only sum, order is irrelevant.
    let mut queue: Vec<(String, usize)> = vec![(root.to_string(), 0)];

    while let Some((dir, depth)) = queue.pop() {
        if cancel.load(Ordering::Relaxed) {
            truncated = true;
            cancelled = true;
            break;
        }
        if depth >= max_depth || (files + dirs) >= max_entries as u64 {
            truncated = true;
            hit_cap = true;
            continue;
        }
        match provider.list(&dir).await {
            Ok(entries) => {
                for entry in entries {
                    // Skip symlinks: a symlink-to-dir is reported with both
                    // is_dir and is_symlink (sftp.rs), so following it allows
                    // cur->. / up->.. cycles to inflate the figure and burn
                    // the depth/entry budget. Matches the MCP scan and the
                    // sftp rmdir_recursive GAP-A02 precedent.
                    if entry.is_symlink {
                        continue;
                    }
                    // Cap is enforced inside the loop (not only between
                    // directories) so one hostile listing with millions of
                    // subdir entries cannot grow the queue past max_entries.
                    if (files + dirs) >= max_entries as u64 {
                        truncated = true;
                        hit_cap = true;
                        break;
                    }
                    if entry.is_dir {
                        dirs += 1;
                        queue.push((entry.path.clone(), depth + 1));
                        continue;
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
                unreadable_dirs += 1;
            }
        }
    }

    Ok(UsedScan {
        used_bytes: used,
        file_count: files,
        dir_count: dirs,
        truncated,
        unreadable_dirs,
        hit_cap,
        cancelled,
        method: "bfs",
    })
}
