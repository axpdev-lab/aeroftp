//! Local and remote directory scanning with excludes, depth caps, and entry
//! caps. Used by sync / check / reconcile and by the MCP tools that expose
//! the same operations.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use crate::provider_transfer_executor::ProviderListSessionModel;
use crate::providers::{ProviderError, StorageProvider};
use crate::transfer_dag::{
    DagObserver, ResourceRequest, TransferBudget, TransferResourceManager,
    TransferSessionPoolHandle,
};
use sha2::Digest;
use std::collections::{HashSet, VecDeque};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::JoinSet;

/// Soft cap on the number of entries returned from a single scan. Matches
/// the CLI cap so both front-ends behave identically.
const MAX_SCAN_ENTRIES: usize = 500_000;

/// Maximum directory depth when recursing the remote tree.
const DEFAULT_SCAN_DEPTH: usize = 100;

/// Minimum gap between periodic scan-progress notifications. Matches the
/// transfer progress throttle so a large tree streams a moving counter
/// instead of jumping straight to the final value (the O-3 regression of
/// the clone-pool scan path, which has no `AppHandle`).
const SCAN_PROGRESS_INTERVAL: std::time::Duration = std::time::Duration::from_millis(150);

/// A local file captured by `scan_local_tree`.
#[derive(Debug, Clone)]
pub struct LocalEntry {
    pub rel_path: String,
    pub size: u64,
    pub mtime: Option<String>,
    pub sha256: Option<String>,
}

/// A remote file captured by `scan_remote_tree`.
#[derive(Debug, Clone)]
pub struct RemoteEntry {
    pub rel_path: String,
    pub size: u64,
    pub mtime: Option<String>,
    /// Optional server-side hash. Populated when `ScanOptions::compute_remote_checksum`
    /// is set AND the provider implements `checksum()`. Algorithm is chosen
    /// by `pick_preferred_checksum` (SHA-256 preferred, then SHA-1, then MD5).
    pub checksum_alg: Option<String>,
    pub checksum_hex: Option<String>,
}

/// Shared scan tuning.
#[derive(Debug, Clone, Default)]
pub struct ScanOptions {
    /// Max recursion depth (None = default 100). 0 means root only.
    pub max_depth: Option<usize>,
    /// Glob patterns to exclude (compiled internally once).
    pub exclude_patterns: Vec<String>,
    /// If present, only entries whose rel_path is in this set pass the filter.
    pub files_from: Option<HashSet<String>>,
    /// Compute a streaming SHA-256 for each local file.
    pub compute_checksum: bool,
    /// Request server-side checksums for each remote file.
    ///
    /// Gated at call-time by `provider.supports_checksum()`: on unsupported
    /// providers the flag is silently ignored (comparison falls back to size).
    pub compute_remote_checksum: bool,
    /// Override the 500 000 entry cap (None = use the default).
    pub max_entries: Option<usize>,
    /// Paths that should always be skipped regardless of excludes.
    /// Used to skip the bisync snapshot file when syncing a tree.
    pub skip_filenames: Vec<String>,
}

fn compile_matchers(patterns: &[String]) -> Vec<globset::GlobMatcher> {
    patterns
        .iter()
        .filter_map(|pat| globset::Glob::new(pat).ok().map(|g| g.compile_matcher()))
        .collect()
}

fn matches_any(matchers: &[globset::GlobMatcher], rel: &str, name: &str) -> bool {
    matchers.iter().any(|m| m.is_match(rel) || m.is_match(name))
}

/// GAP-9f: derive a path relative to `root` from a provider-returned absolute
/// path. Returns `None` when `abs_path` does not sit under `root` (the caller
/// then abandons the fast-path and walks the BFS instead).
fn rel_from_abs(abs_path: &str, root: &str) -> Option<String> {
    let p = abs_path.trim_matches('/');
    let r = root.trim_matches('/');
    if r.is_empty() {
        return if p.is_empty() { None } else { Some(p.to_string()) };
    }
    if p == r {
        // The scan root itself, not a child entry.
        return None;
    }
    p.strip_prefix(&format!("{}/", r)).map(|s| s.to_string())
}

/// GAP-9f: convert a provider-native flat recursive listing
/// (`provider_list_recursive_fastpath`) into the scan's `RemoteEntry` rows,
/// applying the same exclude / skip / files_from / cap filters the BFS path
/// applies. Directories and symlinks are dropped (the scan result is
/// files-only, exactly like the BFS).
///
/// Returns `None` when any file entry's path cannot be made relative to
/// `root`: that means the flat listing and the root disagree, so the BFS is
/// the safe choice rather than a silently-incomplete tree.
fn adapt_fastpath_entries(
    entries: Vec<crate::providers::RemoteEntry>,
    root: &str,
    opts: &ScanOptions,
) -> Option<Vec<RemoteEntry>> {
    let matchers = compile_matchers(&opts.exclude_patterns);
    let cap = opts.max_entries.unwrap_or(MAX_SCAN_ENTRIES);
    let mut results = Vec::new();
    for entry in entries {
        if entry.is_dir || entry.is_symlink {
            continue;
        }
        let rel = match rel_from_abs(&entry.path, root) {
            Some(rel) if !rel.is_empty() => rel,
            _ => {
                tracing::info!(
                    "[scan_remote_tree] fast-path entry {} is not under root {}, falling back to BFS",
                    entry.path,
                    root
                );
                return None;
            }
        };
        if opts.skip_filenames.iter().any(|name| name == &entry.name) {
            continue;
        }
        if !matchers.is_empty() && matches_any(&matchers, &rel, &entry.name) {
            continue;
        }
        if let Some(ref set) = opts.files_from {
            if !set.contains(rel.as_str()) {
                continue;
            }
        }
        results.push(RemoteEntry {
            rel_path: rel,
            size: entry.size,
            mtime: entry.modified,
            checksum_alg: None,
            checksum_hex: None,
        });
        if results.len() >= cap {
            break;
        }
    }
    Some(results)
}

/// GAP-9f: attempt the provider-native single-shot recursive listing for the
/// compare scan. Returns `Some(rows)` only for S3, whose flat
/// `ListObjectsV2` carries full object keys; WebDAV's `Depth: infinity`
/// flattens nested paths (`structured_paths = false`) so it is left to the
/// BFS. `None` means "no fast-path, use the BFS".
async fn try_recursive_fastpath(
    provider: &mut Box<dyn StorageProvider>,
    remote_root: &str,
    opts: &ScanOptions,
) -> Option<Vec<RemoteEntry>> {
    let listing =
        crate::used_scan::provider_list_recursive_fastpath(provider, remote_root).await?;
    if !listing.structured_paths {
        return None;
    }
    adapt_fastpath_entries(listing.entries, remote_root, opts)
}

/// Walk the local directory tree and return files matching the filter.
/// Errors that hit non-root entries are silently dropped (same behaviour as
/// the CLI) so that partial scans still return useful data on messy trees.
pub fn scan_local_tree(root: &str, opts: &ScanOptions) -> Vec<LocalEntry> {
    let matchers = compile_matchers(&opts.exclude_patterns);
    let cap = opts.max_entries.unwrap_or(MAX_SCAN_ENTRIES);
    let depth = opts.max_depth.unwrap_or(DEFAULT_SCAN_DEPTH);

    let mut entries = Vec::new();
    for walk_entry in walkdir::WalkDir::new(root)
        .follow_links(false)
        .max_depth(depth)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entries.len() >= cap {
            break;
        }
        if !walk_entry.file_type().is_file() {
            continue;
        }
        let relative = walk_entry
            .path()
            .strip_prefix(root)
            .unwrap_or(walk_entry.path())
            .to_string_lossy()
            .replace('\\', "/");
        if relative.is_empty() {
            continue;
        }
        let fname = walk_entry.file_name().to_string_lossy().into_owned();
        if opts.skip_filenames.iter().any(|n| n == &fname) {
            continue;
        }
        if !matchers.is_empty() && matches_any(&matchers, &relative, &fname) {
            continue;
        }
        if let Some(ref set) = opts.files_from {
            if !set.contains(relative.as_str()) {
                continue;
            }
        }

        let meta = walk_entry.metadata().ok();
        let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        let mtime = meta.and_then(|m| {
            m.modified().ok().map(|t| {
                let dt: chrono::DateTime<chrono::Utc> = t.into();
                dt.format("%Y-%m-%dT%H:%M:%S").to_string()
            })
        });

        let sha256 = if opts.compute_checksum {
            compute_sha256(walk_entry.path()).ok()
        } else {
            None
        };

        entries.push(LocalEntry {
            rel_path: relative,
            size,
            mtime,
            sha256,
        });
    }
    entries
}

/// Recursively list the remote tree rooted at `remote_root`. Uses the
/// C1/C2-safe canonicalization: relative paths are built from the accumulated
/// `rel_prefix` + `entry.name`, never by stripping the provider-returned
/// absolute path (which varies by backend).
pub async fn scan_remote_tree(
    provider: &mut Box<dyn StorageProvider>,
    remote_root: &str,
    opts: &ScanOptions,
) -> Vec<RemoteEntry> {
    // GAP-9f: provider-native single-shot recursive listing fast-path. S3
    // collapses the per-directory BFS round-trips into one flat listing.
    // Skipped when checksums are requested (the flat listing carries no
    // per-file hash, so the BFS per-file `checksum()` is still required).
    if !opts.compute_remote_checksum {
        if let Some(results) = try_recursive_fastpath(provider, remote_root, opts).await {
            return results;
        }
    }

    let matchers = compile_matchers(&opts.exclude_patterns);
    let cap = opts.max_entries.unwrap_or(MAX_SCAN_ENTRIES);
    let depth = opts.max_depth.unwrap_or(DEFAULT_SCAN_DEPTH);
    // Server-side checksum is only worth requesting when both the caller
    // asked for it AND the provider advertises the capability. This lets
    // agents pass `compute_remote_checksum=true` unconditionally without
    // paying a per-file `NotSupported` round trip on protocols like FTP.
    let want_remote_checksum = opts.compute_remote_checksum && provider.supports_checksum();

    let mut results = Vec::new();
    let mut queue: Vec<(String, String, usize)> = vec![(remote_root.to_string(), String::new(), 0)];
    while let Some((abs_dir, rel_prefix, current_depth)) = queue.pop() {
        if current_depth >= depth {
            continue;
        }
        if results.len() >= cap {
            break;
        }
        match list_with_transport_retry(provider, &abs_dir).await {
            Ok(entries) => {
                // Collect files for this directory first so the per-file
                // checksum loop below can yield without re-entering `list`
                // on the same provider (some backends reuse a single
                // connection for list + stat + checksum and do not tolerate
                // interleaved calls).
                let mut pending_files: Vec<(String, String, crate::providers::RemoteEntry)> =
                    Vec::new();
                for entry in entries {
                    let entry_rel = if rel_prefix.is_empty() {
                        entry.name.clone()
                    } else {
                        format!("{}/{}", rel_prefix, entry.name)
                    };
                    if entry.is_dir {
                        queue.push((entry.path.clone(), entry_rel, current_depth + 1));
                        continue;
                    }
                    if opts.skip_filenames.iter().any(|n| n == &entry.name) {
                        continue;
                    }
                    if !matchers.is_empty() && matches_any(&matchers, &entry_rel, &entry.name) {
                        continue;
                    }
                    if let Some(ref set) = opts.files_from {
                        if !set.contains(entry_rel.as_str()) {
                            continue;
                        }
                    }
                    pending_files.push((entry_rel, entry.path.clone(), entry));
                }
                for (entry_rel, abs_path, provider_entry) in pending_files {
                    if results.len() >= cap {
                        break;
                    }
                    let (checksum_alg, checksum_hex) = if want_remote_checksum {
                        match provider.checksum(&abs_path).await {
                            Ok(map) => pick_preferred_checksum(&map),
                            Err(_) => (None, None),
                        }
                    } else {
                        (None, None)
                    };
                    results.push(RemoteEntry {
                        rel_path: entry_rel,
                        size: provider_entry.size,
                        mtime: provider_entry.modified,
                        checksum_alg,
                        checksum_hex,
                    });
                }
            }
            Err(e) => {
                eprintln!(
                    "[scan_remote_tree] warning: failed to list {}: {}",
                    abs_dir, e
                );
            }
        }
    }
    results
}

/// Scan a remote tree through the GUI provider holder, consuming explicit
/// checker/list leases. Clone-backed providers can list independent directories
/// concurrently; locked legacy providers keep the old one-list-at-a-time path.
pub async fn scan_remote_tree_with_provider_lock(
    provider: Arc<Mutex<Option<Box<dyn StorageProvider>>>>,
    remote_root: &str,
    opts: &ScanOptions,
    list_model: &ProviderListSessionModel,
    cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
    observer: Option<&dyn DagObserver>,
) -> Vec<RemoteEntry> {
    // GAP-9f: provider-native single-shot recursive listing fast-path,
    // tried once before either BFS branch (clone-pool or locked). S3's flat
    // ListObjectsV2 returns the whole subtree in one paginated call.
    // Skipped when checksums are requested or the scan is already cancelled.
    if !opts.compute_remote_checksum && !scan_cancelled(&cancel) {
        let fast = {
            let mut guard = provider.lock().await;
            match guard.as_mut() {
                Some(p) => try_recursive_fastpath(p, remote_root, opts).await,
                None => None,
            }
        };
        if let Some(results) = fast {
            if let Some(obs) = observer {
                obs.on_scan_progress(results.len(), 0);
            }
            return results;
        }
    }

    if !list_model.is_clone_pool() {
        return scan_remote_tree_locked(provider, remote_root, opts, list_model, cancel, observer)
            .await;
    }

    let cap = opts.max_entries.unwrap_or(MAX_SCAN_ENTRIES);
    let depth = opts.max_depth.unwrap_or(DEFAULT_SCAN_DEPTH);
    let max_workers = list_model.max_leases().max(1);
    let resource_manager = Arc::new(TransferResourceManager::new(TransferBudget {
        checker_slots: max_workers as u16,
        ..TransferBudget::from_file_slots(1)
    }));
    let session_pool = Arc::new(list_model.session_pool("provider-list"));
    let want_remote_checksum = {
        let provider_lock = provider.lock().await;
        opts.compute_remote_checksum
            && provider_lock
                .as_ref()
                .map(|provider| provider.supports_checksum())
                .unwrap_or(false)
    };

    let mut results = Vec::new();
    let mut queue = VecDeque::from([RemoteScanDir {
        abs_dir: remote_root.to_string(),
        rel_prefix: String::new(),
        depth: 0,
    }]);
    let mut join_set = JoinSet::new();
    let mut in_flight = 0usize;
    let mut last_progress = std::time::Instant::now();

    while (!queue.is_empty() || in_flight > 0) && results.len() < cap {
        if scan_cancelled(&cancel) {
            break;
        }
        while in_flight < max_workers {
            if scan_cancelled(&cancel) {
                break;
            }
            let Some(dir) = queue.pop_front() else {
                break;
            };
            if dir.depth >= depth {
                continue;
            }
            spawn_remote_scan_task(
                &mut join_set,
                provider.clone(),
                resource_manager.clone(),
                session_pool.clone(),
                dir,
                opts.clone(),
                want_remote_checksum,
                cancel.clone(),
            );
            in_flight += 1;
        }

        if in_flight == 0 {
            break;
        }

        match join_set.join_next().await {
            Some(Ok(Ok(batch))) => {
                in_flight = in_flight.saturating_sub(1);
                for file in batch.files {
                    if results.len() >= cap {
                        break;
                    }
                    results.push(file);
                }
                for dir in batch.dirs {
                    if results.len() >= cap {
                        break;
                    }
                    queue.push_back(dir);
                }
            }
            Some(Ok(Err(error))) => {
                in_flight = in_flight.saturating_sub(1);
                eprintln!("[scan_remote_tree] warning: {}", error);
            }
            Some(Err(error)) => {
                in_flight = in_flight.saturating_sub(1);
                eprintln!("[scan_remote_tree] warning: scan task failed: {}", error);
            }
            None => break,
        }

        if let Some(obs) = observer {
            if last_progress.elapsed() >= SCAN_PROGRESS_INTERVAL {
                obs.on_scan_progress(results.len(), in_flight);
                last_progress = std::time::Instant::now();
            }
        }
    }

    if let Some(obs) = observer {
        obs.on_scan_progress(results.len(), 0);
    }
    results
}

async fn scan_remote_tree_locked(
    provider: Arc<Mutex<Option<Box<dyn StorageProvider>>>>,
    remote_root: &str,
    opts: &ScanOptions,
    list_model: &ProviderListSessionModel,
    cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
    observer: Option<&dyn DagObserver>,
) -> Vec<RemoteEntry> {
    let cap = opts.max_entries.unwrap_or(MAX_SCAN_ENTRIES);
    let depth = opts.max_depth.unwrap_or(DEFAULT_SCAN_DEPTH);
    let want_remote_checksum = {
        let provider_lock = provider.lock().await;
        opts.compute_remote_checksum
            && provider_lock
                .as_ref()
                .map(|provider| provider.supports_checksum())
                .unwrap_or(false)
    };

    let mut results = Vec::new();
    let resource_manager = TransferResourceManager::new(TransferBudget {
        checker_slots: 1,
        ..TransferBudget::from_file_slots(1)
    });
    let session_pool = list_model.session_pool("provider-list");
    let mut queue = VecDeque::from([RemoteScanDir {
        abs_dir: remote_root.to_string(),
        rel_prefix: String::new(),
        depth: 0,
    }]);
    while let Some(dir) = queue.pop_front() {
        if scan_cancelled(&cancel) {
            break;
        }
        if dir.depth >= depth || results.len() >= cap {
            continue;
        }

        let Ok(_checker_lease) = resource_manager.acquire(ResourceRequest::checker()).await else {
            eprintln!("[scan_remote_tree] warning: failed to acquire checker slot");
            break;
        };
        let Ok(session_lease) = session_pool.acquire().await else {
            eprintln!("[scan_remote_tree] warning: failed to acquire list lease");
            break;
        };
        let batch = {
            let mut provider_lock = provider.lock().await;
            let Some(provider) = provider_lock.as_mut() else {
                eprintln!("[scan_remote_tree] warning: provider disconnected");
                break;
            };
            scan_remote_dir(provider, &dir, opts, want_remote_checksum, &cancel).await
        };
        drop(session_lease);

        match batch {
            Ok(batch) => {
                for file in batch.files {
                    if results.len() >= cap {
                        break;
                    }
                    results.push(file);
                }
                for dir in batch.dirs {
                    queue.push_back(dir);
                }
            }
            Err(error) => {
                eprintln!(
                    "[scan_remote_tree] warning: failed to list {}: {}",
                    dir.abs_dir, error
                );
            }
        }

        // One list at a time: emit per directory, matching the cadence the
        // pre-clone-pool path produced (no throttle needed here).
        if let Some(obs) = observer {
            obs.on_scan_progress(results.len(), usize::from(!queue.is_empty()));
        }
    }

    if let Some(obs) = observer {
        obs.on_scan_progress(results.len(), 0);
    }
    results
}

#[derive(Debug, Clone)]
struct RemoteScanDir {
    abs_dir: String,
    rel_prefix: String,
    depth: usize,
}

struct RemoteScanBatch {
    files: Vec<RemoteEntry>,
    dirs: Vec<RemoteScanDir>,
}

#[allow(clippy::too_many_arguments)]
fn spawn_remote_scan_task(
    join_set: &mut JoinSet<Result<RemoteScanBatch, String>>,
    provider: Arc<Mutex<Option<Box<dyn StorageProvider>>>>,
    resource_manager: Arc<TransferResourceManager>,
    session_pool: Arc<TransferSessionPoolHandle>,
    dir: RemoteScanDir,
    opts: ScanOptions,
    want_remote_checksum: bool,
    cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
) {
    join_set.spawn(async move {
        let _checker_lease = resource_manager
            .acquire(ResourceRequest::checker())
            .await
            .map_err(|error| format!("failed to acquire checker slot: {}", error))?;
        let session_lease = session_pool
            .acquire()
            .await
            .map_err(|error| format!("failed to acquire list lease: {}", error))?;
        if scan_cancelled(&cancel) {
            drop(session_lease);
            return Ok(RemoteScanBatch {
                files: Vec::new(),
                dirs: Vec::new(),
            });
        }
        let mut worker = {
            let provider_lock = provider.lock().await;
            provider_lock
                .as_ref()
                .ok_or_else(|| "provider disconnected".to_string())?
                .clone_for_list()
                .map_err(|error| error.to_string())?
        };
        let result = scan_remote_dir(&mut worker, &dir, &opts, want_remote_checksum, &cancel)
            .await
            .map_err(|error| format!("failed to list {}: {}", dir.abs_dir, error));
        drop(session_lease);
        result
    });
}

/// Returns true when an optional cancel flag has been raised by the UI.
fn scan_cancelled(cancel: &Option<Arc<std::sync::atomic::AtomicBool>>) -> bool {
    cancel
        .as_ref()
        .map(|flag| flag.load(std::sync::atomic::Ordering::Relaxed))
        .unwrap_or(false)
}

async fn scan_remote_dir(
    provider: &mut Box<dyn StorageProvider>,
    dir: &RemoteScanDir,
    opts: &ScanOptions,
    want_remote_checksum: bool,
    cancel: &Option<Arc<std::sync::atomic::AtomicBool>>,
) -> Result<RemoteScanBatch, ProviderError> {
    let matchers = compile_matchers(&opts.exclude_patterns);
    let entries = list_with_transport_retry(provider, &dir.abs_dir).await?;
    let mut files = Vec::new();
    let mut dirs = Vec::new();

    for entry in entries {
        if scan_cancelled(cancel) {
            break;
        }
        let entry_rel = if dir.rel_prefix.is_empty() {
            entry.name.clone()
        } else {
            format!("{}/{}", dir.rel_prefix, entry.name)
        };
        if entry.is_dir {
            dirs.push(RemoteScanDir {
                abs_dir: entry.path.clone(),
                rel_prefix: entry_rel,
                depth: dir.depth + 1,
            });
            continue;
        }
        if opts.skip_filenames.iter().any(|name| name == &entry.name) {
            continue;
        }
        if !matchers.is_empty() && matches_any(&matchers, &entry_rel, &entry.name) {
            continue;
        }
        if let Some(ref set) = opts.files_from {
            if !set.contains(entry_rel.as_str()) {
                continue;
            }
        }
        let (checksum_alg, checksum_hex) = if want_remote_checksum {
            match provider.checksum(&entry.path).await {
                Ok(map) => pick_preferred_checksum(&map),
                Err(_) => (None, None),
            }
        } else {
            (None, None)
        };
        files.push(RemoteEntry {
            rel_path: entry_rel,
            size: entry.size,
            mtime: entry.modified,
            checksum_alg,
            checksum_hex,
        });
    }

    Ok(RemoteScanBatch { files, dirs })
}

/// Run `provider.list(dir)` with one automatic reconnect on transport-level
/// failures.
///
/// Motivation: FTP control channels can land in a half-open state after a
/// previous upload that failed mid-way (e.g. `553 No such file`). The
/// subsequent `list` call then returns *"Data connection is already open"*
/// or similar: symptoms of a dead session, not a missing directory. Left
/// alone, `scan_remote_tree` was silently treating the failure as an empty
/// listing, which let sync duplicate files that actually existed. We now
/// detect transport-level errors, `disconnect()` + `connect()` the provider,
/// and retry `list()` exactly once. Business-level errors (`NotFound`,
/// `PermissionDenied`, `NotSupported`) bypass the reconnect.
async fn list_with_transport_retry(
    provider: &mut Box<dyn StorageProvider>,
    abs_dir: &str,
) -> Result<Vec<crate::providers::RemoteEntry>, ProviderError> {
    match provider.list(abs_dir).await {
        Ok(v) => Ok(v),
        Err(e) if is_transport_level(&e) => {
            eprintln!(
                "[scan_remote_tree] transport error on {}: {}: reconnecting",
                abs_dir, e
            );
            // Best-effort tear-down; we already know the session is dirty.
            let _ = provider.disconnect().await;
            provider.connect().await?;
            provider.list(abs_dir).await
        }
        Err(e) => Err(e),
    }
}

/// Same classifier shape used by the MCP pool: keep the two in sync.
///
/// Any provider-level failure that leaves the underlying TCP/TLS/SSH session
/// unusable qualifies. Pattern matching errs on the side of retrying: worst
/// case we reconnect once on a spurious match; the correct case would have
/// been a silently-skipped directory.
fn is_transport_level(e: &ProviderError) -> bool {
    match e {
        ProviderError::NotConnected
        | ProviderError::ConnectionFailed(_)
        | ProviderError::ConnectionLost(_)
        | ProviderError::Timeout
        | ProviderError::NetworkError(_)
        | ProviderError::IoError(_) => true,
        ProviderError::TransferFailed(msg)
        | ProviderError::ServerError(msg)
        | ProviderError::Other(msg)
        | ProviderError::Unknown(msg) => {
            let lower = msg.to_ascii_lowercase();
            const PATTERNS: &[&str] = &[
                "data connection is already open",
                "data connection already open",
                "connection is already open",
                "broken pipe",
                "pipe closed",
                "connection reset",
                "connection closed",
                "connection aborted",
                "connection refused",
                "not connected",
                "eof from server",
                "unexpected eof",
                "channel closed",
                "session closed",
                "socket closed",
                "stream closed",
                "bad file descriptor",
            ];
            PATTERNS.iter().any(|p| lower.contains(p))
        }
        _ => false,
    }
}

/// Pick a preferred hash from a provider checksum map. Returns `(algo, hex)`.
///
/// Order: SHA-256 → SHA-1 → MD5 → first available entry. Keys are matched
/// case-insensitively against known aliases since providers spell them
/// inconsistently (`"sha-256"`, `"SHA256"`, `"sha256Hex"`, etc.).
fn pick_preferred_checksum(
    checksums: &std::collections::HashMap<String, String>,
) -> (Option<String>, Option<String>) {
    if checksums.is_empty() {
        return (None, None);
    }
    let preferred = [
        ("sha-256", "sha256"),
        ("sha256", "sha256"),
        ("sha_256", "sha256"),
        ("sha-1", "sha1"),
        ("sha1", "sha1"),
        ("md5", "md5"),
    ];
    let lower_map: std::collections::HashMap<String, &String> = checksums
        .iter()
        .map(|(k, v)| (k.to_ascii_lowercase(), v))
        .collect();
    for (key, canonical) in preferred {
        if let Some(val) = lower_map.get(key) {
            return (Some(canonical.to_string()), Some((*val).clone()));
        }
    }
    // Fallback: any key. Stable ordering is not required: the consumer
    // compares by both algo label and value.
    checksums
        .iter()
        .next()
        .map(|(k, v)| (Some(k.clone()), Some(v.clone())))
        .unwrap_or((None, None))
}

fn compute_sha256(path: &Path) -> std::io::Result<String> {
    use std::fs::File;
    use std::io::Read;

    let mut file = File::open(path)?;
    let mut hasher = sha2::Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn scan_local_tree_returns_files_with_relative_paths() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        fs::write(root.join("a.txt"), b"hello").unwrap();
        fs::create_dir(root.join("sub")).unwrap();
        fs::write(root.join("sub/b.txt"), b"world").unwrap();

        let entries = scan_local_tree(root.to_str().unwrap(), &ScanOptions::default());
        let paths: Vec<String> = entries.iter().map(|e| e.rel_path.clone()).collect();
        assert!(paths.contains(&"a.txt".to_string()));
        assert!(paths.contains(&"sub/b.txt".to_string()));
    }

    #[test]
    fn scan_local_tree_honours_excludes() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        fs::write(root.join("keep.log"), b"x").unwrap();
        fs::write(root.join("skip.tmp"), b"x").unwrap();

        let opts = ScanOptions {
            exclude_patterns: vec!["*.tmp".to_string()],
            ..Default::default()
        };
        let entries = scan_local_tree(root.to_str().unwrap(), &opts);
        let paths: Vec<String> = entries.iter().map(|e| e.rel_path.clone()).collect();
        assert_eq!(paths, vec!["keep.log"]);
    }

    #[test]
    fn scan_local_tree_computes_sha256_when_requested() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        fs::write(root.join("hello.txt"), b"hello").unwrap();

        let opts = ScanOptions {
            compute_checksum: true,
            ..Default::default()
        };
        let entries = scan_local_tree(root.to_str().unwrap(), &opts);
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].sha256.as_deref(),
            Some("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824")
        );
    }

    #[test]
    fn scan_local_tree_respects_files_from_filter() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        fs::write(root.join("a.txt"), b"a").unwrap();
        fs::write(root.join("b.txt"), b"b").unwrap();

        let mut files_from = HashSet::new();
        files_from.insert("a.txt".to_string());

        let opts = ScanOptions {
            files_from: Some(files_from),
            ..Default::default()
        };
        let entries = scan_local_tree(root.to_str().unwrap(), &opts);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].rel_path, "a.txt");
    }

    // --- GAP-9f: provider-native recursive listing fast-path ----------

    use crate::providers::RemoteEntry as ProviderEntry;

    fn provider_file(name: &str, path: &str, size: u64) -> ProviderEntry {
        ProviderEntry::file(name.to_string(), path.to_string(), size)
    }

    #[test]
    fn rel_from_abs_strips_the_root_prefix() {
        assert_eq!(
            rel_from_abs("/srv/data/a/b.txt", "/srv/data"),
            Some("a/b.txt".to_string())
        );
        // Trailing slashes on either side are tolerated.
        assert_eq!(
            rel_from_abs("/srv/data/a/b.txt", "/srv/data/"),
            Some("a/b.txt".to_string())
        );
        // A bucket-root scan keeps the whole key.
        assert_eq!(
            rel_from_abs("/photos/x.jpg", "/"),
            Some("photos/x.jpg".to_string())
        );
    }

    #[test]
    fn rel_from_abs_rejects_paths_outside_the_root() {
        assert_eq!(rel_from_abs("/other/x.txt", "/srv/data"), None);
        // The root itself is not a child entry.
        assert_eq!(rel_from_abs("/srv/data", "/srv/data"), None);
    }

    #[test]
    fn adapt_fastpath_entries_derives_nested_relative_paths() {
        let entries = vec![
            provider_file("top.txt", "/root/top.txt", 10),
            provider_file("c.txt", "/root/a/b/c.txt", 20),
            ProviderEntry::directory("a".to_string(), "/root/a".to_string()),
        ];
        let rows = adapt_fastpath_entries(entries, "/root", &ScanOptions::default()).unwrap();
        let mut paths: Vec<String> = rows.iter().map(|r| r.rel_path.clone()).collect();
        paths.sort();
        // The directory entry is dropped; both files keep their nesting.
        assert_eq!(paths, vec!["a/b/c.txt".to_string(), "top.txt".to_string()]);
    }

    #[test]
    fn adapt_fastpath_entries_honours_exclude_and_files_from_and_cap() {
        let entries = vec![
            provider_file("keep.txt", "/root/keep.txt", 1),
            provider_file("skip.tmp", "/root/skip.tmp", 1),
            provider_file("nested.txt", "/root/sub/nested.txt", 1),
        ];
        let excluded = adapt_fastpath_entries(
            entries.clone(),
            "/root",
            &ScanOptions {
                exclude_patterns: vec!["*.tmp".to_string()],
                ..Default::default()
            },
        )
        .unwrap();
        assert!(excluded.iter().all(|r| !r.rel_path.ends_with(".tmp")));

        let mut files_from = HashSet::new();
        files_from.insert("sub/nested.txt".to_string());
        let filtered = adapt_fastpath_entries(
            entries.clone(),
            "/root",
            &ScanOptions {
                files_from: Some(files_from),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].rel_path, "sub/nested.txt");

        let capped = adapt_fastpath_entries(
            entries,
            "/root",
            &ScanOptions {
                max_entries: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(capped.len(), 1);
    }

    #[test]
    fn adapt_fastpath_entries_bails_when_an_entry_is_outside_the_root() {
        let entries = vec![
            provider_file("ok.txt", "/root/ok.txt", 1),
            provider_file("stray.txt", "/elsewhere/stray.txt", 1),
        ];
        // A key that does not sit under the scan root means the flat listing
        // and the root disagree: the whole fast-path is abandoned.
        assert!(adapt_fastpath_entries(entries, "/root", &ScanOptions::default()).is_none());
    }

    #[test]
    fn adapt_fastpath_entries_drops_symlinks() {
        let mut link = provider_file("link.txt", "/root/link.txt", 0);
        link.is_symlink = true;
        let rows = adapt_fastpath_entries(
            vec![link, provider_file("real.txt", "/root/real.txt", 5)],
            "/root",
            &ScanOptions::default(),
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].rel_path, "real.txt");
    }
}
