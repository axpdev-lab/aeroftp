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
    /// True when a caller-requested depth bound excluded branches deeper
    /// than that depth. The figure is then a **lower bound** of the full
    /// tree, but a complete answer to the depth that was asked, so
    /// `truncated` and `hit_cap` stay false.
    ///
    /// On the fast path this is a deduction from the relative name
    /// (counting `/` after stripping the root), not a fact of a walk: a
    /// key with a surprising prefix can disagree with the BFS depth. A
    /// flattened listing (WebDAV `Depth: infinity`) is never filtered this
    /// way; the scan falls through to the BFS so the bound is walked.
    pub depth_limited: bool,
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
    /// True when the provider stopped its own listing before the end of the
    /// tree (S3 stops paginating at its entry cap even with a continuation
    /// token). What it never listed has no path to name, so a consumer that
    /// bounds a run has to refuse it rather than treat the answer as whole.
    pub truncated: bool,
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
            Ok((entries, truncated)) => Some(FastPathListing {
                entries,
                method: "s3-list-recursive",
                structured_paths: true,
                truncated,
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
                        // PROPFIND infinity is answered whole or not at all:
                        // this provider sets no cap of its own.
                        truncated: false,
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

/// Relative path of `abs_path` under `root`. Same strip as
/// `sync_core::scan::rel_from_abs`: trim `/`, drop the root prefix, keep
/// the rest. `None` when `abs_path` is the root itself or sits outside it.
///
/// Depth is then `rel.split('/').count()`. That count is a deduction from
/// the name, not a fact of a walk.
pub(crate) fn rel_from_abs(abs_path: &str, root: &str) -> Option<String> {
    let p = abs_path.trim_matches('/');
    let r = root.trim_matches('/');
    if r.is_empty() {
        return if p.is_empty() {
            None
        } else {
            Some(p.to_string())
        };
    }
    if p == r {
        return None;
    }
    p.strip_prefix(&format!("{}/", r)).map(|s| s.to_string())
}

fn relative_component_count(abs_path: &str, root: &str) -> Option<usize> {
    rel_from_abs(abs_path, root)
        .filter(|rel| !rel.is_empty())
        .map(|rel| rel.split('/').count())
}

/// Reduce a provider-native flat listing to a used-storage figure.
///
/// Shared by `scan_used_bytes` and the GUI `provider_scan_used` command so
/// the two cannot diverge on the entry cap, the cancel flag, or a listing
/// the provider itself cut. This is the reduction #801 wrote for the fast
/// path: directories count against the cap with files, and a cancel raised
/// while the listing ran is read on the way out, keeping the sums.
///
/// `max_depth` is applied only when it is `Some` (the caller asked) and the
/// listing carries structured paths. The parachute (`None`) does not filter
/// a list the provider already delivered. A requested depth that is
/// respected is not a truncation: `truncated` and `hit_cap` stay as the
/// provider and the entry cap left them, and `depth_limited` records that
/// the figure is a lower bound of the full tree.
pub(crate) fn reduce_fastpath_listing(
    listing: FastPathListing,
    root: &str,
    max_depth: Option<usize>,
    max_entries: usize,
    cancel: &AtomicBool,
) -> UsedScan {
    let apply_depth = max_depth.filter(|_| listing.structured_paths);
    let method = listing.method;
    let mut used = 0u64;
    let mut files = 0u64;
    let mut dirs = 0u64;
    // A provider that stopped its own listing early makes this figure a
    // lower bound, exactly as the entry cap below does.
    let mut truncated = listing.truncated;
    let mut hit_cap = listing.truncated;
    let mut depth_limited = false;
    // The provider handed us a complete single-shot listing, so there is no
    // per-directory walk here and nothing can be unreadable.
    let unreadable_dirs: u64 = 0;
    for e in listing.entries {
        if let Some(limit) = apply_depth {
            if let Some(n) = relative_component_count(&e.path, root) {
                if n > limit {
                    depth_limited = true;
                    continue;
                }
            }
        }
        // Directories count against the cap alongside files, as they do in
        // `bfs_used_bytes`, so the two ways of walking the same tree cut at
        // the same place. Counting files alone let a listing made mostly of
        // directory markers (which the recursive listing keeps since #796)
        // run past `max_entries` without ever reaching this check.
        if (files + dirs) >= max_entries as u64 {
            truncated = true;
            hit_cap = true;
            break;
        }
        if e.is_dir {
            dirs += 1;
            continue;
        }
        used = used.saturating_add(e.size);
        files += 1;
    }
    // One call, but not an instant one: a cancel raised while the listing
    // ran is a cancel this scan has to honour. The BFS polls the flag
    // between directories; this path has no walk to poll inside, so it
    // polls on the way out. The figure keeps what was already summed, as
    // the BFS does when it breaks out of its queue: `df --scan` and `size`
    // print this number, so a stopped scan has to answer a lower bound and
    // not a zero. A cancel is not a cap, so `hit_cap` stays as the
    // provider left it.
    let cancelled = cancel.load(Ordering::Relaxed);
    if cancelled {
        truncated = true;
    }
    UsedScan {
        used_bytes: used,
        file_count: files,
        dir_count: dirs,
        truncated,
        unreadable_dirs,
        hit_cap,
        cancelled,
        depth_limited,
        method,
    }
}

/// Text marker for `size` when the figure is a lower bound. Cancelled and
/// truncated are two different states and must not share a word.
pub fn size_bound_marker(scan: &UsedScan) -> &'static str {
    if scan.cancelled {
        " (CANCELLED)"
    } else if scan.truncated {
        " (TRUNCATED)"
    } else {
        ""
    }
}

/// Recursively sum the bytes used under `root`.
///
/// `max_depth` is `Some` when the caller asked for a depth, `None` when
/// only the project parachute applies. `max_entries` is the entry cap.
/// `cancel` is polled between directories so the GUI cancel button (and
/// double Ctrl+C in the CLI) takes effect promptly. `on_progress(files,
/// bytes)` is called as the figure grows so the caller can render a spinner.
pub async fn scan_used_bytes(
    provider: &mut Box<dyn StorageProvider>,
    root: &str,
    max_depth: Option<usize>,
    max_entries: usize,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(u64, u64),
) -> Result<UsedScan, ProviderError> {
    // --- Provider-native single-shot recursive listing -----------------
    // S3 / WebDAV collapse the per-directory BFS round-trips. The shared
    // helper handles the downcast + the WebDAV not-recursed guard; any
    // failure falls through to the BFS so the figure stays correct.
    if let Some(fast) = provider_list_recursive_fastpath(provider, root).await {
        // A requested depth on a flattened listing cannot be applied by
        // counting `/` in the name: WebDAV infinity collapses `a/b/c.txt`
        // to `root/c.txt`. Fall through so the BFS walks the bound.
        let skip_fast = max_depth.is_some() && !fast.structured_paths;
        if !skip_fast {
            let scan = reduce_fastpath_listing(fast, root, max_depth, max_entries, cancel);
            on_progress(scan.file_count, scan.used_bytes);
            return Ok(scan);
        }
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
    max_depth: Option<usize>,
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
    let mut depth_limited = false;
    // None = the caller asked for the whole tree; the parachute is the
    // walk's safety net and hitting it is a truncation. Some = the caller
    // asked for this many levels; stopping there is a complete answer.
    let depth_limit = max_depth.unwrap_or(crate::sync_core::scan::DEFAULT_SCAN_DEPTH);
    let depth_requested = max_depth.is_some();
    // (absolute path, depth). LIFO is fine: we only sum, order is irrelevant.
    let mut queue: Vec<(String, usize)> = vec![(root.to_string(), 0)];

    while let Some((dir, depth)) = queue.pop() {
        if cancel.load(Ordering::Relaxed) {
            truncated = true;
            cancelled = true;
            break;
        }
        if depth >= depth_limit {
            if depth_requested {
                // Complete answer to the depth that was asked. The figure
                // excludes what sits under this directory, so it is a lower
                // bound of the full tree, declared by `depth_limited`.
                depth_limited = true;
            } else {
                truncated = true;
                hit_cap = true;
            }
            continue;
        }
        if (files + dirs) >= max_entries as u64 {
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

    // A cancel raised while the last directory was listed never sees
    // another turn of this loop: the queue is empty and we would leave
    // cancelled=false. The fast path already polls on the way out (#801);
    // this walk has to do the same, keeping the sums already in hand.
    let cancelled = cancelled || cancel.load(Ordering::Relaxed);
    if cancelled {
        truncated = true;
    }

    Ok(UsedScan {
        used_bytes: used,
        file_count: files,
        dir_count: dirs,
        truncated,
        unreadable_dirs,
        hit_cap,
        cancelled,
        depth_limited,
        method: "bfs",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::types::S3Config;

    /// An S3 provider connected to a fake endpoint that answers every request
    /// with `body`, so `scan_used_bytes` takes the single-shot fast path. The
    /// handle returned is the server's: abort it at the end of the test.
    async fn s3_fast_path(
        body: &'static str,
    ) -> (Box<dyn StorageProvider>, tokio::task::JoinHandle<()>) {
        let app = axum::Router::new().fallback(axum::routing::any(
            move |_req: axum::extract::Request| async move {
                axum::http::Response::new(axum::body::Body::from(body))
            },
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let mut provider = S3Provider::new(S3Config {
            endpoint: Some(format!("http://{addr}")),
            region: "us-east-1".to_string(),
            access_key_id: "key".to_string(),
            secret_access_key: secrecy::SecretString::from("secret".to_string()),
            session_token: None,
            role_arn: None,
            role_external_id: None,
            role_session_name: None,
            role_duration_seconds: None,
            role_mfa_serial: None,
            role_mfa_token_code: None,
            bucket: "test-bucket".to_string(),
            prefix: None,
            path_style: true,
            storage_class: None,
            sse_mode: None,
            sse_kms_key_id: None,
            verify_cert: true,
            allow_cleartext_endpoint: true,
        })
        .expect("build the S3 provider");
        provider
            .connect()
            .await
            .expect("connect to the fake endpoint");
        (Box::new(provider), server)
    }

    /// Directory markers count against the entry cap, as they do in the BFS,
    /// so both ways of walking a tree cut at the same place. Counting only
    /// files against the cap let a listing of markers run past `max_entries`.
    #[tokio::test]
    async fn fast_path_counts_directories_against_the_entry_cap() {
        let (mut provider, server) = s3_fast_path(
            "<ListBucketResult><Contents><Key>d1/</Key><Size>0</Size></Contents><Contents><Key>d2/</Key><Size>0</Size></Contents><Contents><Key>d3/</Key><Size>0</Size></Contents><Contents><Key>f.txt</Key><Size>7</Size></Contents></ListBucketResult>",
        )
        .await;
        let cancel = AtomicBool::new(false);

        let scan = scan_used_bytes(&mut provider, "/", None, 2, &cancel, |_, _| {})
            .await
            .expect("the scan answers");
        server.abort();

        assert_eq!(
            scan.file_count + scan.dir_count,
            2,
            "the cap counts files and directories together: {scan:?}"
        );
        assert!(scan.truncated, "a cut figure is a lower bound: {scan:?}");
        assert!(scan.hit_cap, "the cap is what cut it: {scan:?}");
    }

    /// A cancel raised while the single listing runs is honoured on the way out
    /// of the await, and the figure keeps what was already summed, as the BFS
    /// does when it breaks out of its queue. A stopped scan is a partial
    /// figure, not a zero one: `df --scan` and `size` print it.
    #[tokio::test]
    async fn fast_path_honours_a_cancel_and_keeps_the_partial_figure() {
        let (mut provider, server) = s3_fast_path(
            "<ListBucketResult><Contents><Key>a.txt</Key><Size>3</Size></Contents><Contents><Key>b.txt</Key><Size>4</Size></Contents></ListBucketResult>",
        )
        .await;
        let cancel = AtomicBool::new(true);

        let scan = scan_used_bytes(&mut provider, "/", None, 500_000, &cancel, |_, _| {})
            .await
            .expect("the scan answers");
        server.abort();

        assert!(scan.cancelled, "the cancel is reported: {scan:?}");
        assert!(
            scan.truncated,
            "a cancelled figure is a lower bound: {scan:?}"
        );
        assert!(!scan.hit_cap, "a cancel is not a cap: {scan:?}");
        assert_eq!(
            (scan.file_count, scan.used_bytes),
            (2, 7),
            "the sums already in hand are reported, not zeros: {scan:?}"
        );
    }

    /// Through the real S3 fast path, a requested depth of 1 keeps only the
    /// top-level file. Before the fix the flat listing was summed whole.
    #[tokio::test]
    async fn s3_fast_path_applies_a_requested_depth() {
        let (mut provider, server) = s3_fast_path(
            "<ListBucketResult><Contents><Key>a.txt</Key><Size>1</Size></Contents><Contents><Key>dir/b.txt</Key><Size>2</Size></Contents><Contents><Key>dir/sub/c.txt</Key><Size>4</Size></Contents></ListBucketResult>",
        )
        .await;
        let cancel = AtomicBool::new(false);
        let scan = scan_used_bytes(&mut provider, "/", Some(1), 500_000, &cancel, |_, _| {})
            .await
            .expect("the scan answers");
        server.abort();
        assert_eq!(
            (scan.used_bytes, scan.file_count),
            (1, 1),
            "depth 1 keeps only a.txt: {scan:?}"
        );
        assert!(scan.depth_limited, "deeper keys were excluded: {scan:?}");
        assert!(
            !scan.truncated && !scan.hit_cap,
            "a requested depth is not a truncation: {scan:?}"
        );
        assert_eq!(scan.method, "s3-list-recursive");
    }

    fn file(name: &str, path: &str, size: u64) -> crate::providers::RemoteEntry {
        crate::providers::RemoteEntry::file(name.to_string(), path.to_string(), size)
    }

    fn dir(name: &str, path: &str) -> crate::providers::RemoteEntry {
        crate::providers::RemoteEntry::directory(name.to_string(), path.to_string())
    }

    /// Nested tree used to compare the two walks:
    /// `/a.txt` (1), `/dir/b.txt` (2), `/dir/sub/c.txt` (4).
    fn nested_listing() -> FastPathListing {
        FastPathListing {
            entries: vec![
                file("a.txt", "/a.txt", 1),
                dir("dir", "/dir"),
                file("b.txt", "/dir/b.txt", 2),
                dir("sub", "/dir/sub"),
                file("c.txt", "/dir/sub/c.txt", 4),
            ],
            method: "s3-list-recursive",
            structured_paths: true,
            truncated: false,
        }
    }

    fn nested_tree_provider() -> Box<dyn StorageProvider> {
        let mut dirs = std::collections::HashMap::new();
        dirs.insert(
            "/".to_string(),
            vec![file("a.txt", "/a.txt", 1), dir("dir", "/dir")],
        );
        dirs.insert(
            "/dir".to_string(),
            vec![file("b.txt", "/dir/b.txt", 2), dir("sub", "/dir/sub")],
        );
        dirs.insert(
            "/dir/sub".to_string(),
            vec![file("c.txt", "/dir/sub/c.txt", 4)],
        );
        Box::new(tree_provider(dirs))
    }

    fn tree_provider(
        dirs: std::collections::HashMap<String, Vec<crate::providers::RemoteEntry>>,
    ) -> TreeProvider {
        TreeProvider {
            dirs,
            cancel: None,
            raise_cancel_on: None,
        }
    }

    /// In-memory tree for the BFS: `list` answers from a map.
    /// `raise_cancel_on` stores `cancel` when that path is listed, so a test
    /// can raise the flag while the last directory is in flight.
    struct TreeProvider {
        dirs: std::collections::HashMap<String, Vec<crate::providers::RemoteEntry>>,
        cancel: Option<std::sync::Arc<AtomicBool>>,
        raise_cancel_on: Option<String>,
    }

    #[async_trait::async_trait]
    impl StorageProvider for TreeProvider {
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
        fn provider_type(&self) -> crate::providers::ProviderType {
            crate::providers::ProviderType::Sftp
        }
        fn display_name(&self) -> String {
            "tree".to_string()
        }
        async fn connect(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn disconnect(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        fn is_connected(&self) -> bool {
            true
        }
        async fn list(
            &mut self,
            path: &str,
        ) -> Result<Vec<crate::providers::RemoteEntry>, ProviderError> {
            if self.raise_cancel_on.as_deref() == Some(path) {
                if let Some(flag) = &self.cancel {
                    flag.store(true, Ordering::Relaxed);
                }
            }
            self.dirs
                .get(path)
                .cloned()
                .ok_or_else(|| ProviderError::NotFound(path.to_string()))
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
            _remote_path: &str,
            _local_path: &str,
            _progress: Option<Box<dyn Fn(u64, u64) + Send>>,
        ) -> Result<(), ProviderError> {
            Err(ProviderError::NotSupported("download".to_string()))
        }
        async fn download_to_bytes(
            &mut self,
            _remote_path: &str,
        ) -> Result<Vec<u8>, ProviderError> {
            Err(ProviderError::NotSupported("download_to_bytes".to_string()))
        }
        async fn upload(
            &mut self,
            _local_path: &str,
            _remote_path: &str,
            _progress: Option<Box<dyn Fn(u64, u64) + Send>>,
        ) -> Result<(), ProviderError> {
            Err(ProviderError::NotSupported("upload".to_string()))
        }
        async fn mkdir(&mut self, _path: &str) -> Result<(), ProviderError> {
            Err(ProviderError::NotSupported("mkdir".to_string()))
        }
        async fn delete(&mut self, _path: &str) -> Result<(), ProviderError> {
            Err(ProviderError::NotSupported("delete".to_string()))
        }
        async fn rmdir(&mut self, _path: &str) -> Result<(), ProviderError> {
            Err(ProviderError::NotSupported("rmdir".to_string()))
        }
        async fn rmdir_recursive(&mut self, _path: &str) -> Result<(), ProviderError> {
            Err(ProviderError::NotSupported("rmdir_recursive".to_string()))
        }
        async fn rename(&mut self, _from: &str, _to: &str) -> Result<(), ProviderError> {
            Err(ProviderError::NotSupported("rename".to_string()))
        }
        async fn stat(
            &mut self,
            path: &str,
        ) -> Result<crate::providers::RemoteEntry, ProviderError> {
            Err(ProviderError::NotFound(path.to_string()))
        }
        async fn size(&mut self, _path: &str) -> Result<u64, ProviderError> {
            Ok(0)
        }
        async fn exists(&mut self, path: &str) -> Result<bool, ProviderError> {
            Ok(self.dirs.contains_key(path))
        }
        async fn keep_alive(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn server_info(&mut self) -> Result<String, ProviderError> {
            Ok("tree".to_string())
        }
    }

    /// Depth is a deduction from the relative name: count `/` after stripping
    /// the root. A flattened or oddly-prefixed path can disagree with a walk.
    #[test]
    fn relative_component_count_is_a_name_deduction() {
        assert_eq!(relative_component_count("/a.txt", "/"), Some(1));
        assert_eq!(relative_component_count("/dir/b.txt", "/"), Some(2));
        assert_eq!(relative_component_count("/dir/sub/c.txt", "/"), Some(3));
        assert_eq!(
            relative_component_count("/srv/data/a/b.txt", "/srv/data"),
            Some(2)
        );
        assert_eq!(relative_component_count("/srv/data", "/srv/data"), None);
    }

    /// A requested depth of 2 keeps `/a.txt` and `/dir/b.txt` and drops
    /// `/dir/sub/c.txt`. The figure is a lower bound of the full tree
    /// (`depth_limited`), not a truncation: the answer to the depth that
    /// was asked is complete.
    #[test]
    fn requested_depth_on_the_fast_path_is_a_lower_bound_not_a_truncation() {
        let cancel = AtomicBool::new(false);
        let scan = reduce_fastpath_listing(nested_listing(), "/", Some(2), 500_000, &cancel);
        assert_eq!(
            (scan.used_bytes, scan.file_count, scan.dir_count),
            (3, 2, 2),
            "depth 2 keeps a.txt + dir/b.txt and the two dirs above the cut: {scan:?}"
        );
        assert!(
            scan.depth_limited,
            "excluding deeper branches is a lower bound of the full tree: {scan:?}"
        );
        assert!(
            !scan.truncated && !scan.hit_cap,
            "a requested depth that was respected is not a truncation: {scan:?}"
        );
    }

    /// The parachute does not filter a list the provider already delivered.
    /// A three-component key is counted in full.
    #[test]
    fn parachute_does_not_filter_the_flat_listing() {
        let cancel = AtomicBool::new(false);
        let scan = reduce_fastpath_listing(nested_listing(), "/", None, 500_000, &cancel);
        assert_eq!(
            (scan.used_bytes, scan.file_count, scan.dir_count),
            (7, 3, 2),
            "the whole listing is summed: {scan:?}"
        );
        assert!(
            !scan.depth_limited && !scan.truncated && !scan.hit_cap,
            "no bound was asked and the provider did not cut: {scan:?}"
        );
    }

    /// Fast path and BFS answer the same tree under the same requested depth.
    /// Before the fix the fast path summed the whole listing.
    #[tokio::test]
    async fn requested_depth_matches_on_fast_path_and_bfs() {
        let cancel = AtomicBool::new(false);
        let fast = reduce_fastpath_listing(nested_listing(), "/", Some(1), 500_000, &cancel);
        let mut bfs_provider = nested_tree_provider();
        let bfs = scan_used_bytes(&mut bfs_provider, "/", Some(1), 500_000, &cancel, |_, _| {})
            .await
            .expect("the BFS answers");

        assert_eq!(
            (fast.used_bytes, fast.file_count),
            (1, 1),
            "fast path at depth 1 keeps only a.txt: {fast:?}"
        );
        assert_eq!(
            (bfs.used_bytes, bfs.file_count),
            (fast.used_bytes, fast.file_count),
            "BFS must answer the same tree: fast={fast:?} bfs={bfs:?}"
        );
        assert!(
            !fast.truncated && !fast.hit_cap && !bfs.truncated && !bfs.hit_cap,
            "requested depth is not a truncation: fast={fast:?} bfs={bfs:?}"
        );
        assert_eq!(bfs.method, "bfs");
    }

    /// A cancel raised while the last directory is listed never sees another
    /// turn of the queue: the loop ends normally and only the exit poll
    /// reports it. Raising the flag before the scan would be answered by the
    /// head-of-loop check, which already worked. The sums already in hand
    /// are kept. These flags are what `shouldPersistUsedScan` refuses.
    #[tokio::test]
    async fn a_cancel_raised_while_listing_the_last_directory_is_reported() {
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let mut dirs = std::collections::HashMap::new();
        dirs.insert(
            "/".to_string(),
            vec![file("a.txt", "/a.txt", 3), file("b.txt", "/b.txt", 4)],
        );
        let mut provider: Box<dyn StorageProvider> = Box::new(TreeProvider {
            dirs,
            cancel: Some(cancel.clone()),
            raise_cancel_on: Some("/".to_string()),
        });
        let scan = scan_used_bytes(
            &mut provider,
            "/",
            None,
            500_000,
            cancel.as_ref(),
            |_, _| {},
        )
        .await
        .expect("the scan answers");

        assert!(
            scan.cancelled,
            "the exit poll must see a cancel that arrived on the last list: {scan:?}"
        );
        assert!(
            scan.truncated,
            "a cancelled figure is a lower bound: {scan:?}"
        );
        assert_eq!(
            (scan.file_count, scan.used_bytes),
            (2, 7),
            "the sums already in hand are kept: {scan:?}"
        );
        assert!(!scan.hit_cap, "a cancel is not a cap: {scan:?}");
        // shouldPersistUsedScan is !truncated && !cancelled. Either flag
        // false would have written this incomplete figure onto the profile.
        assert!(
            scan.truncated && scan.cancelled,
            "the persist gate must refuse this scan: {scan:?}"
        );
    }

    /// Hitting the parachute on the BFS is a truncation. The flat listing
    /// under the same parachute is not filtered, so a deep file is counted.
    #[tokio::test]
    async fn parachute_truncates_the_bfs_and_leaves_the_flat_listing_whole() {
        let cancel = AtomicBool::new(false);
        let parachute = crate::sync_core::scan::DEFAULT_SCAN_DEPTH;

        let mut dirs = std::collections::HashMap::new();
        let mut path = String::new();
        for i in 0..=parachute {
            let here = if path.is_empty() {
                "/".to_string()
            } else {
                path.clone()
            };
            let child_name = format!("d{i}");
            let child_path = if here == "/" {
                format!("/{child_name}")
            } else {
                format!("{here}/{child_name}")
            };
            dirs.insert(here, vec![dir(&child_name, &child_path)]);
            path = child_path;
        }
        dirs.insert(
            path.clone(),
            vec![file("deep.txt", &format!("{path}/deep.txt"), 9)],
        );
        let mut provider: Box<dyn StorageProvider> = Box::new(tree_provider(dirs));
        let bfs = scan_used_bytes(&mut provider, "/", None, 500_000, &cancel, |_, _| {})
            .await
            .expect("the BFS answers");
        assert!(
            bfs.truncated && bfs.hit_cap,
            "the parachute cut the walk: {bfs:?}"
        );
        assert_eq!(
            bfs.used_bytes, 0,
            "the file sits past the parachute: {bfs:?}"
        );

        let mut entries = Vec::new();
        let mut key = String::new();
        for i in 0..=parachute {
            key = if key.is_empty() {
                format!("/d{i}")
            } else {
                format!("{key}/d{i}")
            };
            entries.push(dir(&format!("d{i}"), &key));
        }
        entries.push(file("deep.txt", &format!("{key}/deep.txt"), 9));
        let fast = reduce_fastpath_listing(
            FastPathListing {
                entries,
                method: "s3-list-recursive",
                structured_paths: true,
                truncated: false,
            },
            "/",
            None,
            500_000,
            &cancel,
        );
        assert_eq!(
            fast.used_bytes, 9,
            "the parachute does not throw away a list the provider already delivered: {fast:?}"
        );
        assert!(!fast.truncated && !fast.hit_cap && !fast.depth_limited);
    }

    /// A listing the provider itself cut is a lower bound, even with no
    /// depth asked. The GUI persist path keys off this flag.
    #[test]
    fn a_provider_cut_listing_is_truncated() {
        let cancel = AtomicBool::new(false);
        let mut listing = nested_listing();
        listing.truncated = true;
        let scan = reduce_fastpath_listing(listing, "/", None, 500_000, &cancel);
        assert!(scan.truncated && scan.hit_cap, "{scan:?}");
        assert!(!scan.cancelled);
    }

    /// Cancelled and truncated must not share a word in `size` output.
    #[test]
    fn size_bound_marker_names_cancel_and_truncation_apart() {
        let cancel = AtomicBool::new(false);
        let clean = reduce_fastpath_listing(nested_listing(), "/", None, 500_000, &cancel);
        assert_eq!(size_bound_marker(&clean), "");

        let mut listing = nested_listing();
        listing.truncated = true;
        let truncated = reduce_fastpath_listing(listing, "/", None, 500_000, &cancel);
        assert_eq!(size_bound_marker(&truncated), " (TRUNCATED)");

        let cancelled_flag = AtomicBool::new(true);
        let cancelled =
            reduce_fastpath_listing(nested_listing(), "/", None, 500_000, &cancelled_flag);
        assert_eq!(size_bound_marker(&cancelled), " (CANCELLED)");
        assert!(cancelled.truncated, "a cancel is still a lower bound");
    }

    /// A flattened listing does not apply a requested depth by counting
    /// `/` in the name: every nested file would look like one component.
    #[test]
    fn flattened_listing_does_not_filter_by_name() {
        let cancel = AtomicBool::new(false);
        let listing = FastPathListing {
            entries: vec![file("c.txt", "/c.txt", 4)],
            method: "webdav-infinity",
            structured_paths: false,
            truncated: false,
        };
        let scan = reduce_fastpath_listing(listing, "/", Some(1), 500_000, &cancel);
        assert_eq!(
            scan.used_bytes, 4,
            "a flattened path is not dropped as if it were depth 1: {scan:?}"
        );
        assert!(!scan.depth_limited);
    }
}
