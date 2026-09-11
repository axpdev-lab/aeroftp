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
use std::collections::{HashMap, HashSet, VecDeque};
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
    /// Directories listed at once on a provider whose list pool allows it
    /// (`--checkers`); `None` means [`DEFAULT_SCAN_CHECKERS`]. A single-session
    /// provider walks one directory at a time whatever the value.
    pub checkers: Option<usize>,
    /// Force the portable BFS scanner even when a provider advertises a flat
    /// recursive list. AeroSync Compare uses this under an active crypt
    /// overlay so encrypted path segments are gathered in the same shape as
    /// the regular provider browser.
    pub disable_recursive_fastpath: bool,
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
        return if p.is_empty() {
            None
        } else {
            Some(p.to_string())
        };
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
/// applies. Directories are dropped (the scan result is files-only, exactly
/// like the BFS) and symlinks are returned as skipped links.
///
/// Returns `None` when any file entry's path cannot be made relative to
/// `root`: that means the flat listing and the root disagree, so the BFS is
/// the safe choice rather than a silently-incomplete tree.
fn adapt_fastpath_entries(
    entries: Vec<crate::providers::RemoteEntry>,
    root: &str,
    opts: &ScanOptions,
) -> Option<(Vec<RemoteEntry>, Vec<SkippedLink>)> {
    let matchers = compile_matchers(&opts.exclude_patterns);
    let cap = opts.max_entries.unwrap_or(MAX_SCAN_ENTRIES);
    let mut results = Vec::new();
    let mut skipped_links = Vec::new();
    for entry in entries {
        if entry.is_symlink {
            // Listed and not followed, as the BFS treats a link to a directory,
            // and reported so a sync leaves the link's path alone.
            if let Some(rel) = rel_from_abs(&entry.path, root).filter(|rel| !rel.is_empty()) {
                tracing::warn!(
                    "[scan_remote_tree] fast-path skipping symlink {} -> {}: not followed",
                    rel,
                    entry.link_target.as_deref().unwrap_or("?")
                );
                skipped_links.push(SkippedLink {
                    rel_path: rel,
                    link_target: entry.link_target.clone(),
                });
                if results.len() + skipped_links.len() >= cap {
                    break;
                }
            }
            continue;
        }
        if entry.is_dir {
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
        // SEC: same traversal guard as the BFS path. A flat listing whose object
        // key resolves to a `..`-bearing relative path must not reach the
        // download sink; drop the single offending entry rather than the whole
        // fast-path (the rest of the listing is still usable).
        if let Err(reason) = crate::sync::validate_relative_path(&rel) {
            tracing::warn!(
                "[scan_remote_tree] fast-path skipping entry {} under {}: {}",
                entry.path,
                root,
                reason
            );
            continue;
        }
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
        if results.len() + skipped_links.len() >= cap {
            break;
        }
    }
    Some((results, skipped_links))
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
) -> Option<(Vec<RemoteEntry>, Vec<SkippedLink>)> {
    let listing = crate::used_scan::provider_list_recursive_fastpath(provider, remote_root).await?;
    if !listing.structured_paths {
        return None;
    }
    adapt_fastpath_entries(listing.entries, remote_root, opts)
}

/// Completeness signal for a tree scan.
///
/// A scan is INCOMPLETE when a directory listing failed (`list_errors > 0`) or
/// it was cut short at the entry cap (`truncated`). This matters for
/// delete-propagation: a file "missing" from an *incomplete* scan may simply be
/// un-listed rather than actually deleted on that side, so mirroring that
/// "missing" into a destructive delete is unsafe. Callers that delete orphans
/// must gate on [`ScanCompleteness::is_complete`]. CLAUDE-AV-B3-01.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScanCompleteness {
    /// Number of directory listings that failed during the scan.
    pub list_errors: usize,
    /// True if the scan stopped early at [`MAX_SCAN_ENTRIES`] / `max_entries`.
    pub truncated: bool,
}

impl ScanCompleteness {
    /// The scan saw the whole tree: no listing errors and no cap truncation.
    pub fn is_complete(&self) -> bool {
        self.list_errors == 0 && !self.truncated
    }
}

/// A symbolic link a scan listed and did not follow.
///
/// No walker follows a link to a directory (GAP-A02): syncing through one
/// would copy the target's tree under another name, and a link to `..` never
/// terminates. Whatever sits behind a link is therefore absent from that
/// side's scan, which is not the same as deleted.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SkippedLink {
    /// The link itself, relative to the scan root.
    pub rel_path: String,
    /// What the link points to, when it could be read.
    pub link_target: Option<String>,
}

/// A path a scan could not see, so what sits at or under it is unknown.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct UnseenPath {
    /// The path, relative to the scan root.
    pub rel_path: String,
    /// Why it was not seen: `depth_limit`, `list_error` or `unreadable`.
    pub reason: &'static str,
}

/// What a tree scan skipped or could not see, beyond the entries it returned.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanBoundaries {
    /// Symbolic links the scan listed and did not follow.
    pub links: Vec<SkippedLink>,
    /// Paths the scan could not see: a directory at the depth limit or one that
    /// failed to list, an entry whose metadata could not be read.
    pub unseen: Vec<UnseenPath>,
    /// Set when the scan missed a part of the tree it cannot name (it was
    /// cancelled, cut off at the entry cap, or lost its session), so no run can
    /// be bounded around it.
    pub unbounded: Option<&'static str>,
}

impl ScanBoundaries {
    /// How many boundaries the scan recorded; they count against its entry cap.
    fn len(&self) -> usize {
        self.links.len() + self.unseen.len()
    }

    /// Record a path the scan did not see. The scan root is not recorded: an
    /// unlisted root is the incomplete scan the completeness counters already
    /// report, and there is no path to bound a run around.
    fn unseen(&mut self, rel_path: &str, reason: &'static str) {
        if !rel_path.is_empty() {
            tracing::warn!("[scan] not seen: {} ({})", rel_path, reason);
            self.unseen.push(UnseenPath {
                rel_path: rel_path.to_string(),
                reason,
            });
        }
    }
}

/// The part of a sync its scans did not see, left alone on both sides: every
/// skipped link and unseen path, with everything under them.
///
/// A scan does not see behind a link, below its depth limit, into a directory
/// that failed to list or past metadata it could not read, so the other side's
/// files at those paths read as present on that side only. A download with
/// orphan deletes then deletes the local copies of files still on the remote,
/// an upload deletes the remote files behind a local link, and any copy may
/// write through a link into its target. The scans have the facts for none of
/// those decisions, so the run stays out of those subtrees; and when a scan
/// missed a part of the tree it cannot name, the run is refused instead.
#[derive(Debug, Default)]
pub struct ScanBound {
    links: Vec<SkippedLink>,
    unseen: Vec<UnseenPath>,
    paths: HashSet<String>,
    ancestors: HashSet<String>,
    refusal: Option<String>,
}

/// What a local path prefix is on disk, cached by [`ScanBound::for_sync`].
#[derive(Clone, Copy)]
enum LocalPrefix {
    Link,
    Present,
    Absent,
    Unreadable,
}

impl ScanBound {
    /// Bound a sync by what its two scans did not see: the remote links and
    /// unseen paths, the local unseen paths, and the local links above any path
    /// only the remote holds.
    ///
    /// Local walkers skip links too, but a local link matters only where the
    /// remote holds something under it, and every such path is absent from the
    /// local entries: checking the ancestors of those paths on disk finds exactly
    /// the links that would be misread, whichever walker (or watch cycle)
    /// produced the local list. An ancestor whose metadata cannot be read, for a
    /// reason other than absence, is left out the same way.
    pub fn for_sync<'a>(
        local_root: &str,
        local_paths: impl IntoIterator<Item = &'a str>,
        remote_paths: impl IntoIterator<Item = &'a str>,
        local: &ScanBoundaries,
        remote: ScanBoundaries,
    ) -> Self {
        let mut bound = Self::default();
        if let Some(reason) = remote.unbounded {
            bound.refusal = Some(format!(
                "the remote scan did not see the whole tree ({reason})"
            ));
        } else if let Some(reason) = local.unbounded {
            bound.refusal = Some(format!(
                "the local scan did not see the whole tree ({reason})"
            ));
        }
        for link in remote.links {
            bound.add_link(link);
        }
        for path in remote
            .unseen
            .into_iter()
            .chain(local.unseen.iter().cloned())
        {
            bound.add_unseen(path);
        }
        let local_set: HashSet<&str> = local_paths.into_iter().collect();
        let root = Path::new(local_root);
        let mut on_disk: HashMap<String, LocalPrefix> = HashMap::new();
        for rel in remote_paths {
            if local_set.contains(rel) || bound.covers(rel) {
                continue;
            }
            let prefix_ends = rel
                .match_indices('/')
                .map(|(end, _)| end)
                .chain(std::iter::once(rel.len()));
            for end in prefix_ends {
                let prefix = &rel[..end];
                let state = match on_disk.get(prefix) {
                    Some(state) => *state,
                    None => {
                        let path = root.join(prefix);
                        let state = match std::fs::symlink_metadata(&path) {
                            Ok(meta) if meta.file_type().is_symlink() => {
                                let link_target = std::fs::read_link(&path)
                                    .ok()
                                    .map(|target| target.to_string_lossy().into_owned());
                                tracing::warn!(
                                    "[sync] skipping local symlink {} -> {}: not followed, its subtree is left out on both sides",
                                    prefix,
                                    link_target.as_deref().unwrap_or("?")
                                );
                                bound.add_link(SkippedLink {
                                    rel_path: prefix.to_string(),
                                    link_target,
                                });
                                LocalPrefix::Link
                            }
                            Ok(_) => LocalPrefix::Present,
                            Err(error)
                                if matches!(
                                    error.kind(),
                                    std::io::ErrorKind::NotFound
                                        | std::io::ErrorKind::NotADirectory
                                ) =>
                            {
                                LocalPrefix::Absent
                            }
                            Err(error) => {
                                tracing::warn!(
                                    "[sync] cannot read local {}: {}; its subtree is left out on both sides",
                                    prefix,
                                    error
                                );
                                bound.add_unseen(UnseenPath {
                                    rel_path: prefix.to_string(),
                                    reason: "unreadable",
                                });
                                LocalPrefix::Unreadable
                            }
                        };
                        on_disk.insert(prefix.to_string(), state);
                        state
                    }
                };
                if !matches!(state, LocalPrefix::Present) {
                    break;
                }
            }
        }
        bound
    }

    /// [`ScanBound::for_sync`] over the entries of a sync scan, applied to them:
    /// every entry the bound covers is dropped on both sides.
    pub fn apply(
        local_root: &str,
        locals: &mut Vec<LocalEntry>,
        remotes: &mut Vec<RemoteEntry>,
        local: &ScanBoundaries,
        remote: ScanBoundaries,
    ) -> Self {
        let bound = Self::for_sync(
            local_root,
            locals.iter().map(|entry| entry.rel_path.as_str()),
            remotes.iter().map(|entry| entry.rel_path.as_str()),
            local,
            remote,
        );
        if !bound.is_empty() {
            locals.retain(|entry| !bound.covers(&entry.rel_path));
            remotes.retain(|entry| !bound.covers(&entry.rel_path));
        }
        bound
    }

    /// Whether `rel` is a bounded path or sits under one.
    pub fn covers(&self, rel: &str) -> bool {
        if self.paths.is_empty() {
            return false;
        }
        self.paths.contains(rel)
            || rel
                .match_indices('/')
                .any(|(end, _)| self.paths.contains(&rel[..end]))
    }

    /// Whether a bounded path sits under the directory `rel`: such a directory
    /// cannot be copied or deleted as a whole without reaching into the bound.
    pub fn holds_bounded_paths(&self, rel: &str) -> bool {
        self.ancestors.contains(rel)
    }

    /// Nothing bounds the run.
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// Why the run must not be planned at all: a scan missed a part of the tree
    /// it cannot name.
    pub fn refusal(&self) -> Option<&str> {
        self.refusal.as_deref()
    }

    /// The links the bound was built from, remote first, in the order found.
    pub fn links(&self) -> &[SkippedLink] {
        &self.links
    }

    /// The unseen paths that bound the run, remote first.
    pub fn unseen(&self) -> &[UnseenPath] {
        &self.unseen
    }

    /// Every link a run names: the links the bound left out, then the local
    /// links a walk skipped that nothing on the remote sits under.
    pub fn reported_links(&self, local: &ScanBoundaries) -> Vec<SkippedLink> {
        let mut links = self.links.clone();
        links.extend(
            local
                .links
                .iter()
                .filter(|link| !self.paths.contains(&link.rel_path))
                .cloned(),
        );
        links
    }

    fn add_link(&mut self, link: SkippedLink) {
        if self.insert_path(&link.rel_path) {
            self.links.push(link);
        }
    }

    fn add_unseen(&mut self, path: UnseenPath) {
        if self.insert_path(&path.rel_path) {
            self.unseen.push(path);
        }
    }

    fn insert_path(&mut self, rel: &str) -> bool {
        if !self.paths.insert(rel.to_string()) {
            return false;
        }
        for (end, _) in rel.match_indices('/') {
            self.ancestors.insert(rel[..end].to_string());
        }
        true
    }
}

/// Walk the local directory tree and return files matching the filter; see
/// [`scan_local_tree_checked`] for what the walk could not see.
pub fn scan_local_tree(root: &str, opts: &ScanOptions) -> Vec<LocalEntry> {
    scan_local_tree_checked(root, opts).0
}

/// Like [`scan_local_tree`] but also reports [`ScanCompleteness`] so an
/// orphan-delete caller can refuse to delete off an incomplete scan.
/// CLAUDE-AV-B3-01.
///
/// The third element names what the walk did not see: the symlinks to
/// directories it did not follow (reported only: whether a sync leaves a path
/// alone is [`ScanBound`]'s decision), the directories at the depth limit, and
/// the entries it could not read.
pub fn scan_local_tree_checked(
    root: &str,
    opts: &ScanOptions,
) -> (Vec<LocalEntry>, ScanCompleteness, ScanBoundaries) {
    let matchers = compile_matchers(&opts.exclude_patterns);
    let cap = opts.max_entries.unwrap_or(MAX_SCAN_ENTRIES);
    let depth = opts.max_depth.unwrap_or(DEFAULT_SCAN_DEPTH);
    let relative_of = |path: &Path| {
        path.strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/")
    };

    let mut entries = Vec::new();
    let mut completeness = ScanCompleteness::default();
    let mut boundaries = ScanBoundaries::default();
    for result in walkdir::WalkDir::new(root)
        .follow_links(false)
        .max_depth(depth)
        .into_iter()
    {
        let walk_entry = match result {
            Ok(e) => e,
            Err(error) => {
                // A directory we could not read (the root itself when the mount
                // is gone, or any subtree) makes the scan incomplete: files
                // under it are invisible. Count it so delete propagation can
                // refuse, and bound a subtree so no copy reaches into it.
                // CLAUDE-AV-B3-01.
                completeness.list_errors += 1;
                if error.depth() > 0 {
                    if let Some(path) = error.path() {
                        boundaries.unseen(&relative_of(path), "unreadable");
                    }
                }
                continue;
            }
        };
        if entries.len() + boundaries.len() >= cap {
            completeness.truncated = true;
            boundaries.unbounded.get_or_insert("entry_cap");
            break;
        }
        if walk_entry.path_is_symlink() && walk_entry.depth() > 0 {
            // Not followed. A link to a directory is named, so the run can say
            // which subtree it did not load; a link to a file is passed over.
            match std::fs::metadata(walk_entry.path()) {
                Ok(target) if target.is_dir() => {
                    let rel_path = relative_of(walk_entry.path());
                    let link_target = std::fs::read_link(walk_entry.path())
                        .ok()
                        .map(|target| target.to_string_lossy().into_owned());
                    tracing::warn!(
                        "[scan_local_tree] skipping symlink {} -> {}: not followed",
                        rel_path,
                        link_target.as_deref().unwrap_or("?")
                    );
                    boundaries.links.push(SkippedLink {
                        rel_path,
                        link_target,
                    });
                }
                Ok(_) => {}
                // A dangling link points at nothing to load.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => {
                    // Listed but not resolved: it sits in a directory that can
                    // be read and not traversed, so what it points at is unknown.
                    completeness.list_errors += 1;
                    boundaries.unseen(&relative_of(walk_entry.path()), "unreadable");
                }
            }
            continue;
        }
        if walk_entry.depth() == depth && walk_entry.file_type().is_dir() {
            // Listed at the depth limit and not descended into: what it holds is
            // unknown, like the contents of a directory that failed to list.
            completeness.truncated = true;
            if depth == 0 {
                boundaries.unbounded.get_or_insert("depth_limit");
            } else {
                boundaries.unseen(&relative_of(walk_entry.path()), "depth_limit");
            }
            continue;
        }
        if !walk_entry.file_type().is_file() {
            continue;
        }
        let relative = relative_of(walk_entry.path());
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

        let meta = match walk_entry.metadata() {
            Ok(meta) => Some(meta),
            Err(_) => {
                // Listed but not stat'ed: a file inside a directory that can be
                // read and not traversed (0400) keeps the type its directory
                // entry reports while every stat fails. Its size and mtime were
                // never seen, so the scan is incomplete; the entry stays, so it
                // does not read as absent, and its path is bounded, so nothing is
                // copied over it on the strength of a size it never had.
                completeness.list_errors += 1;
                boundaries.unseen(&relative, "unreadable");
                None
            }
        };
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
    (entries, completeness, boundaries)
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
    scan_remote_tree_checked(provider, remote_root, opts)
        .await
        .0
}

/// Directories listed at once when [`ScanOptions::checkers`] is `None`.
pub const DEFAULT_SCAN_CHECKERS: usize = 8;

/// Like [`scan_remote_tree`] but also reports [`ScanCompleteness`]. A failed
/// directory listing is COUNTED (not swallowed into a silently-empty tree) and
/// cap truncation is flagged, so an orphan-delete caller can refuse to mirror a
/// "missing" file that is really just un-listed. CLAUDE-AV-B3-01.
///
/// This is the walker of the sync core, the DAG sync, the MCP tools and the
/// CLI. It runs on the provider's list pool: a provider that lists on
/// independent clones lists up to `checkers` directories at once on warm
/// workers, a single-session provider walks one directory at a time. It used
/// to be a second, serial walker next to the pooled one the GUI scan used,
/// which is why a `sync` over 5000 files on SFTP spent three times longer than
/// rclone in the scan alone. The caller's provider is parked behind a
/// fail-closed placeholder while the pool owns it and handed back before this
/// returns.
///
/// The third element names what the walk skipped or could not see; see
/// [`ScanBound`] for what a sync does with it.
pub async fn scan_remote_tree_checked(
    provider: &mut Box<dyn StorageProvider>,
    remote_root: &str,
    opts: &ScanOptions,
) -> (Vec<RemoteEntry>, ScanCompleteness, ScanBoundaries) {
    let parked: Box<dyn StorageProvider> =
        Box::new(crate::crypt_overlay_provider::DetachedProvider);
    let real = std::mem::replace(provider, parked);
    let holder: Arc<Mutex<Option<Box<dyn StorageProvider>>>> = Arc::new(Mutex::new(Some(real)));
    let checkers = opts.checkers.unwrap_or(DEFAULT_SCAN_CHECKERS).max(1);
    let list_model =
        crate::provider_transfer_executor::resolve_provider_list_session_model(&holder, checkers)
            .await;
    let out = scan_remote_tree_with_provider_lock_checked(
        Arc::clone(&holder),
        remote_root,
        opts,
        &list_model,
        None,
        None,
    )
    .await;
    *provider = holder
        .lock()
        .await
        .take()
        .expect("the pool scanner hands the provider back");
    out
}

/// Scan a remote tree through the GUI provider holder, consuming explicit
/// checker/list leases. Clone-backed providers can list independent directories
/// concurrently; locked legacy providers keep the old one-list-at-a-time path.
/// Also reports [`ScanCompleteness`], so a caller that turns "absent from this
/// listing" into a delete can refuse to act on a tree it did not fully see.
/// CLAUDE-AV-B3-13: every branch below used to drop its listing failures to an
/// `eprintln!` and answer with a plain `Vec`, which is indistinguishable from a
/// tree that really is missing those files.
pub async fn scan_remote_tree_with_provider_lock_checked(
    provider: Arc<Mutex<Option<Box<dyn StorageProvider>>>>,
    remote_root: &str,
    opts: &ScanOptions,
    list_model: &ProviderListSessionModel,
    cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
    observer: Option<&dyn DagObserver>,
) -> (Vec<RemoteEntry>, ScanCompleteness, ScanBoundaries) {
    let mut completeness = ScanCompleteness::default();

    // GAP-9f: provider-native single-shot recursive listing fast-path,
    // tried once before either BFS branch (clone-pool or locked). S3's flat
    // ListObjectsV2 returns the whole subtree in one paginated call.
    // Skipped when checksums are requested or the scan is already cancelled.
    if !opts.compute_remote_checksum && !opts.disable_recursive_fastpath && !scan_cancelled(&cancel)
    {
        let fast = {
            let mut guard = provider.lock().await;
            match guard.as_mut() {
                Some(p) => try_recursive_fastpath(p, remote_root, opts).await,
                None => None,
            }
        };
        if let Some((results, links)) = fast {
            if let Some(obs) = observer {
                obs.on_scan_progress(results.len(), 0);
            }
            // `Some` means the provider returned a full recursive listing in one
            // call, and it degrades to `None` on any error, so there are no
            // per-directory failures to count here. The cap still bites though:
            // `adapt_fastpath_entries` stops filling at `max_entries`, counting
            // the skipped links with the entries, and a listing cut off at the
            // cap is a truncated tree like any other.
            let mut boundaries = ScanBoundaries {
                links,
                ..ScanBoundaries::default()
            };
            if results.len() + boundaries.len() >= opts.max_entries.unwrap_or(MAX_SCAN_ENTRIES) {
                completeness.truncated = true;
                boundaries.unbounded = Some("entry_cap");
            }
            return (results, completeness, boundaries);
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
    // Warm scan workers: a clone that finished a directory cleanly and opted
    // into reuse is parked here and picked up by the next task, so a
    // connection-backed worker (SFTP) dials once per lease, not once per
    // directory. HTTP clones do not opt in and keep their per-directory clone.
    let warm_workers = WarmScanWorkers::default();
    let want_remote_checksum = {
        let provider_lock = provider.lock().await;
        opts.compute_remote_checksum
            && provider_lock
                .as_ref()
                .map(|provider| provider.supports_checksum())
                .unwrap_or(false)
    };

    let mut results = Vec::new();
    let mut boundaries = ScanBoundaries::default();
    let mut queue = VecDeque::from([RemoteScanDir {
        abs_dir: remote_root.to_string(),
        rel_prefix: String::new(),
        depth: 0,
    }]);
    let mut join_set = JoinSet::new();
    let mut in_flight = 0usize;
    let mut last_progress = std::time::Instant::now();

    while (!queue.is_empty() || in_flight > 0) && results.len() + boundaries.len() < cap {
        if scan_cancelled(&cancel) {
            // CLAUDE-AV-B3-13: a cancelled walk stopped mid-tree, at no
            // particular directory, so nothing it missed has a name.
            completeness.truncated = true;
            boundaries.unbounded.get_or_insert("cancelled");
            break;
        }
        while in_flight < max_workers {
            if scan_cancelled(&cancel) {
                completeness.truncated = true;
                boundaries.unbounded.get_or_insert("cancelled");
                break;
            }
            let Some(dir) = queue.pop_front() else {
                break;
            };
            if dir.depth >= depth {
                // CLAUDE-AV-B3-13: everything under this directory is below the
                // depth limit and therefore invisible to the scan.
                completeness.truncated = true;
                bound_unlisted_below_depth(&mut boundaries, &dir);
                continue;
            }
            spawn_remote_scan_task(
                &mut join_set,
                provider.clone(),
                resource_manager.clone(),
                session_pool.clone(),
                warm_workers.clone(),
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
                absorb_skipped_links(
                    &mut boundaries,
                    &mut completeness,
                    batch.skipped_links,
                    cap.saturating_sub(results.len()),
                );
                for file in batch.files {
                    if results.len() + boundaries.len() >= cap {
                        completeness.truncated = true;
                        break;
                    }
                    results.push(file);
                }
                for dir in batch.dirs {
                    if results.len() + boundaries.len() >= cap {
                        // Dropping a queued directory drops its whole subtree.
                        completeness.truncated = true;
                        break;
                    }
                    queue.push_back(dir);
                }
            }
            Some(Ok(Err(failure))) => {
                in_flight = in_flight.saturating_sub(1);
                // CLAUDE-AV-B3-13: this directory did not list. Its files are
                // simply absent from `results`, which reads exactly like a
                // deletion downstream, so count it instead of only warning, and
                // bound it so no copy reaches into what it might hold.
                completeness.list_errors += 1;
                boundaries.unseen(&failure.rel_prefix, "list_error");
                eprintln!("[scan_remote_tree] warning: {}", failure.message);
            }
            Some(Err(error)) => {
                in_flight = in_flight.saturating_sub(1);
                completeness.list_errors += 1;
                // The task that failed took the name of its directory with it.
                boundaries.unbounded.get_or_insert("scan_task_failed");
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

    // The loop also exits when the cap is hit, which is a truncated tree whose
    // missing part has no name.
    if results.len() + boundaries.len() >= cap {
        completeness.truncated = true;
        boundaries.unbounded.get_or_insert("entry_cap");
    }

    if let Some(obs) = observer {
        obs.on_scan_progress(results.len(), 0);
    }
    (results, completeness, boundaries)
}

async fn scan_remote_tree_locked(
    provider: Arc<Mutex<Option<Box<dyn StorageProvider>>>>,
    remote_root: &str,
    opts: &ScanOptions,
    list_model: &ProviderListSessionModel,
    cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
    observer: Option<&dyn DagObserver>,
) -> (Vec<RemoteEntry>, ScanCompleteness, ScanBoundaries) {
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
    let mut boundaries = ScanBoundaries::default();
    let mut completeness = ScanCompleteness::default();
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
            completeness.truncated = true;
            boundaries.unbounded.get_or_insert("cancelled");
            break;
        }
        // CLAUDE-AV-B3-13: both of these skip a directory we were asked to walk,
        // so the tree we report is smaller than the tree that exists.
        if dir.depth >= depth {
            completeness.truncated = true;
            bound_unlisted_below_depth(&mut boundaries, &dir);
            continue;
        }
        if results.len() + boundaries.len() >= cap {
            completeness.truncated = true;
            boundaries.unbounded.get_or_insert("entry_cap");
            break;
        }

        // CLAUDE-AV-B3-13: each of the three aborts below abandons the queue
        // mid-walk. They used to leave only a stderr warning behind; what they
        // abandon has no single name, so no run can be bounded around it.
        let Ok(_checker_lease) = resource_manager.acquire(ResourceRequest::checker()).await else {
            eprintln!("[scan_remote_tree] warning: failed to acquire checker slot");
            completeness.truncated = true;
            boundaries.unbounded.get_or_insert("scan_session_lost");
            break;
        };
        let Ok(session_lease) = session_pool.acquire().await else {
            eprintln!("[scan_remote_tree] warning: failed to acquire list lease");
            completeness.truncated = true;
            boundaries.unbounded.get_or_insert("scan_session_lost");
            break;
        };
        let batch = {
            let mut provider_lock = provider.lock().await;
            let Some(provider) = provider_lock.as_mut() else {
                eprintln!("[scan_remote_tree] warning: provider disconnected");
                completeness.truncated = true;
                boundaries.unbounded.get_or_insert("scan_session_lost");
                break;
            };
            scan_remote_dir(provider, &dir, opts, want_remote_checksum, &cancel).await
        };
        drop(session_lease);

        match batch {
            Ok(batch) => {
                absorb_skipped_links(
                    &mut boundaries,
                    &mut completeness,
                    batch.skipped_links,
                    cap.saturating_sub(results.len()),
                );
                for file in batch.files {
                    if results.len() + boundaries.len() >= cap {
                        completeness.truncated = true;
                        boundaries.unbounded.get_or_insert("entry_cap");
                        break;
                    }
                    results.push(file);
                }
                for dir in batch.dirs {
                    queue.push_back(dir);
                }
            }
            Err(error) => {
                // CLAUDE-AV-B3-13: an unlisted directory hides every file under
                // it, which downstream reads as "these files were deleted".
                completeness.list_errors += 1;
                boundaries.unseen(&dir.rel_prefix, "list_error");
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
    (results, completeness, boundaries)
}

/// Bound a directory the walk reached at its depth limit and did not list. The
/// root has no path to bound around, so a limit that stops at the root leaves
/// the whole tree unseen.
fn bound_unlisted_below_depth(boundaries: &mut ScanBoundaries, dir: &RemoteScanDir) {
    if dir.rel_prefix.is_empty() {
        boundaries.unbounded.get_or_insert("depth_limit");
    } else {
        boundaries.unseen(&dir.rel_prefix, "depth_limit");
    }
}

/// Record a batch's skipped links within the room the entry cap leaves them: a
/// directory holding many links must not grow the list without bound, and links
/// that no longer fit make the scan truncated, with a gap it cannot name.
fn absorb_skipped_links(
    boundaries: &mut ScanBoundaries,
    completeness: &mut ScanCompleteness,
    links: Vec<SkippedLink>,
    room: usize,
) {
    let room = room.saturating_sub(boundaries.len());
    if links.len() > room {
        completeness.truncated = true;
        boundaries.unbounded.get_or_insert("entry_cap");
    }
    boundaries.links.extend(links.into_iter().take(room));
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
    skipped_links: Vec<SkippedLink>,
}

/// A directory a scan task could not list, and why.
struct RemoteScanFailure {
    rel_prefix: String,
    message: String,
}

#[allow(clippy::too_many_arguments)]
fn spawn_remote_scan_task(
    join_set: &mut JoinSet<Result<RemoteScanBatch, RemoteScanFailure>>,
    provider: Arc<Mutex<Option<Box<dyn StorageProvider>>>>,
    resource_manager: Arc<TransferResourceManager>,
    session_pool: Arc<TransferSessionPoolHandle>,
    warm_workers: WarmScanWorkers,
    dir: RemoteScanDir,
    opts: ScanOptions,
    want_remote_checksum: bool,
    cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
) {
    join_set.spawn(async move {
        let failure = |message: String| RemoteScanFailure {
            rel_prefix: dir.rel_prefix.clone(),
            message,
        };
        let _checker_lease = resource_manager
            .acquire(ResourceRequest::checker())
            .await
            .map_err(|error| failure(format!("failed to acquire checker slot: {}", error)))?;
        let session_lease = session_pool
            .acquire()
            .await
            .map_err(|error| failure(format!("failed to acquire list lease: {}", error)))?;
        if scan_cancelled(&cancel) {
            drop(session_lease);
            return Ok(RemoteScanBatch {
                files: Vec::new(),
                dirs: Vec::new(),
                skipped_links: Vec::new(),
            });
        }
        let mut worker = match warm_workers.take().await {
            Some(worker) => worker,
            None => {
                let provider_lock = provider.lock().await;
                provider_lock
                    .as_ref()
                    .ok_or_else(|| failure("provider disconnected".to_string()))?
                    .clone_for_list()
                    .map_err(|error| failure(error.to_string()))?
            }
        };
        let result = scan_remote_dir(&mut worker, &dir, &opts, want_remote_checksum, &cancel)
            .await
            .map_err(|error| failure(format!("failed to list {}: {}", dir.abs_dir, error)));
        // Parked before the lease is released, so the waiter that wakes on
        // the freed permit finds the warm worker ready to pop.
        warm_workers.park(worker, result.is_ok()).await;
        drop(session_lease);
        result
    });
}

/// Warm scan workers shared by the tasks of one remote walk (the scan twin of
/// the transfer executor's warm worker pool). A worker is parked only when it
/// opted into reuse and its directory listed cleanly; a failed listing may
/// leave the session in an unknown state, so that worker is dropped instead.
/// The pool is bounded by the list-session leases: never more than
/// `max_leases` workers exist at once.
#[derive(Clone, Default)]
struct WarmScanWorkers {
    workers: Arc<Mutex<Vec<Box<dyn StorageProvider>>>>,
}

impl WarmScanWorkers {
    async fn take(&self) -> Option<Box<dyn StorageProvider>> {
        self.workers.lock().await.pop()
    }

    async fn park(&self, worker: Box<dyn StorageProvider>, listed_ok: bool) {
        if scan_worker_is_reusable(listed_ok, worker.supports_transfer_worker_reuse()) {
            self.workers.lock().await.push(worker);
        }
    }
}

/// The reuse gate of [`WarmScanWorkers`]: only a clean listing on a worker
/// that opted in is recycled.
fn scan_worker_is_reusable(listed_ok: bool, opted_in: bool) -> bool {
    listed_ok && opted_in
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
    let mut skipped_links = Vec::new();

    for entry in entries {
        if scan_cancelled(cancel) {
            break;
        }
        let entry_rel = if dir.rel_prefix.is_empty() {
            entry.name.clone()
        } else {
            format!("{}/{}", dir.rel_prefix, entry.name)
        };
        // SEC: the entry name is provider-controlled. A malicious or MITM'd
        // listing entry named `..` (or `../../etc/...`) would otherwise let a
        // Download/Both sync write outside the local target root. Reject any
        // traversing name before it becomes a rel_path (skipping a bad dir also
        // keeps it out of the rel_prefix of its children). This is the one
        // walker every scan path uses, so the guard lives here once.
        if let Err(reason) = crate::sync::validate_relative_path(&entry_rel) {
            tracing::warn!(
                "[scan_remote_tree] skipping remote entry {:?} under {}: {}",
                entry.name,
                dir.abs_dir,
                reason
            );
            continue;
        }
        if entry.is_dir {
            // A symlink to a directory is never walked (GAP-A02): syncing
            // through one would duplicate the target's tree, and a link to
            // `..` never terminates. The CLI walker already refused it; the
            // shared walker, which the GUI and now the CLI use, did not.
            if entry.is_walkable_dir() {
                dirs.push(RemoteScanDir {
                    abs_dir: entry.path.clone(),
                    rel_prefix: entry_rel,
                    depth: dir.depth + 1,
                });
            } else {
                // Reported, so a sync leaves the link's path alone on both
                // sides instead of reading the absence behind it as a delete.
                tracing::warn!(
                    "[scan_remote_tree] skipping symlink {} -> {}: not followed",
                    entry_rel,
                    entry.link_target.as_deref().unwrap_or("?")
                );
                skipped_links.push(SkippedLink {
                    rel_path: entry_rel,
                    link_target: entry.link_target.clone(),
                });
            }
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

    Ok(RemoteScanBatch {
        files,
        dirs,
        skipped_links,
    })
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
pub(crate) mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    /// CLAUDE-AV-B3-13: the provider-backed remote walk must report that it did
    /// not see the whole tree. Every abort below used to leave a stderr warning
    /// and return a plain `Vec`, which downstream cannot tell apart from a
    /// remote where those files genuinely no longer exist: running a preset
    /// remote-to-local then deletes the local copies.
    ///
    /// Both branches are driven with a DISCONNECTED provider, which needs no
    /// mock: it is itself one of the real failure paths.
    #[tokio::test]
    async fn locked_walk_on_a_disconnected_provider_is_incomplete_not_an_empty_remote() {
        let provider: Arc<Mutex<Option<Box<dyn StorageProvider>>>> = Arc::new(Mutex::new(None));
        let model = ProviderListSessionModel::LockedSingle {
            provider_type: None,
        };

        let (entries, scan, _) = scan_remote_tree_with_provider_lock_checked(
            provider,
            "/remote",
            &ScanOptions::default(),
            &model,
            None,
            None,
        )
        .await;

        assert!(entries.is_empty());
        assert!(
            !scan.is_complete(),
            "a walk that never listed anything is not an empty remote"
        );
        assert!(scan.truncated, "the walk abandoned its queue mid-tree");
    }

    #[test]
    fn a_scan_worker_is_recycled_only_after_a_clean_listing_and_only_if_it_opted_in() {
        assert!(scan_worker_is_reusable(true, true));
        assert!(
            !scan_worker_is_reusable(false, true),
            "a failed listing drops the worker"
        );
        assert!(
            !scan_worker_is_reusable(true, false),
            "HTTP clones keep their per-directory clone"
        );
        assert!(!scan_worker_is_reusable(false, false));
    }

    #[tokio::test]
    async fn clone_pool_walk_counts_a_failed_directory_listing() {
        let provider: Arc<Mutex<Option<Box<dyn StorageProvider>>>> = Arc::new(Mutex::new(None));
        let model = ProviderListSessionModel::HttpClonePool {
            provider_type: crate::providers::ProviderType::S3,
            max_leases: 2,
        };

        let (entries, scan, _) = scan_remote_tree_with_provider_lock_checked(
            provider,
            "/remote",
            &ScanOptions::default(),
            &model,
            None,
            None,
        )
        .await;

        assert!(entries.is_empty());
        assert!(scan.list_errors > 0, "the failed listing must be counted");
        assert!(!scan.is_complete());
    }

    #[tokio::test]
    async fn a_cancelled_remote_walk_is_incomplete() {
        let provider: Arc<Mutex<Option<Box<dyn StorageProvider>>>> = Arc::new(Mutex::new(None));
        let model = ProviderListSessionModel::LockedSingle {
            provider_type: None,
        };
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(true));

        let (_entries, scan, _) = scan_remote_tree_with_provider_lock_checked(
            provider,
            "/remote",
            &ScanOptions::default(),
            &model,
            Some(cancel),
            None,
        )
        .await;

        assert!(!scan.is_complete(), "a cancelled walk stopped early");
        assert!(scan.truncated);
    }

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

    /// The local walk does not follow a symlink, and names the ones that point
    /// at a directory: that subtree was not loaded. A link to a file is passed
    /// over without a name, as before.
    #[cfg(unix)]
    #[test]
    fn scan_local_tree_names_a_symlinked_directory_it_does_not_follow() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir(root.join("real")).unwrap();
        fs::write(root.join("real/x.txt"), b"x").unwrap();
        std::os::unix::fs::symlink("real", root.join("link")).unwrap();
        std::os::unix::fs::symlink("real/x.txt", root.join("file-link.txt")).unwrap();
        let (entries, completeness, boundaries) =
            scan_local_tree_checked(root.to_str().unwrap(), &ScanOptions::default());
        let paths: Vec<_> = entries.iter().map(|e| e.rel_path.as_str()).collect();
        assert_eq!(paths, vec!["real/x.txt"]);
        assert!(completeness.is_complete());
        assert_eq!(
            boundaries.links,
            vec![SkippedLink {
                rel_path: "link".to_string(),
                link_target: Some("real".to_string()),
            }]
        );
    }

    /// A directory that is readable but not traversable (0400) lists its names,
    /// and the directory entry even says which are files, but every stat inside
    /// it fails. The file is kept, so it does not read as absent, and the scan is
    /// incomplete, because its size and mtime were never seen. Distinct from an
    /// unreadable directory (000), which fails to list at all.
    #[cfg(unix)]
    #[test]
    fn scan_local_tree_is_incomplete_when_it_cannot_stat_a_listed_file() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        fs::write(root.join("a.txt"), b"a").unwrap();
        let locked = root.join("locked");
        fs::create_dir(&locked).unwrap();
        fs::write(locked.join("x.txt"), b"x").unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o400)).unwrap();
        let stat_blocked = fs::metadata(locked.join("x.txt")).is_err();
        let (entries, completeness, _) =
            scan_local_tree_checked(root.to_str().unwrap(), &ScanOptions::default());
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).unwrap();
        if !stat_blocked {
            // Running as root, or on a filesystem that ignores the mode: the
            // stat succeeds and there is nothing to observe.
            return;
        }
        let mut paths: Vec<_> = entries.iter().map(|e| e.rel_path.as_str()).collect();
        paths.sort();
        assert_eq!(
            paths,
            vec!["a.txt", "locked/x.txt"],
            "the listed file is kept"
        );
        assert!(
            !completeness.is_complete(),
            "a listed file that could not be stat'ed makes the scan incomplete"
        );
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
        let (rows, _) = adapt_fastpath_entries(entries, "/root", &ScanOptions::default()).unwrap();
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
        let (excluded, _) = adapt_fastpath_entries(
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
        let (filtered, _) = adapt_fastpath_entries(
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

        let (capped, _) = adapt_fastpath_entries(
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
        link.link_target = Some("target.txt".to_string());
        let (rows, links) = adapt_fastpath_entries(
            vec![link, provider_file("real.txt", "/root/real.txt", 5)],
            "/root",
            &ScanOptions::default(),
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].rel_path, "real.txt");
        // Dropped from the rows, but reported, so a sync can leave it alone.
        assert_eq!(
            links,
            vec![SkippedLink {
                rel_path: "link.txt".to_string(),
                link_target: Some("target.txt".to_string()),
            }]
        );
    }

    /// An in-memory tree for walking tests: `list` answers from a map, every
    /// other operation is unsupported.
    struct TreeProvider {
        dirs: std::collections::HashMap<String, Vec<crate::providers::RemoteEntry>>,
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

    /// GAP-A02 on the shared walker: a symlink that points at a directory is
    /// listed as an entry of its parent but its target is never walked. A
    /// tree that links to its own root would otherwise never finish.
    #[tokio::test]
    async fn a_symlinked_directory_is_listed_but_never_walked() {
        use crate::providers::RemoteEntry;
        let mut loop_link = RemoteEntry::directory("loop".to_string(), "/root".to_string());
        loop_link.is_symlink = true;
        let mut dirs = std::collections::HashMap::new();
        dirs.insert(
            "/root".to_string(),
            vec![
                RemoteEntry::file("a.txt".to_string(), "/root/a.txt".to_string(), 1),
                RemoteEntry::directory("sub".to_string(), "/root/sub".to_string()),
                loop_link,
            ],
        );
        dirs.insert(
            "/root/sub".to_string(),
            vec![RemoteEntry::file(
                "b.txt".to_string(),
                "/root/sub/b.txt".to_string(),
                2,
            )],
        );
        let mut provider: Box<dyn StorageProvider> = Box::new(TreeProvider { dirs });
        let opts = ScanOptions {
            disable_recursive_fastpath: true,
            ..ScanOptions::default()
        };
        let (rows, completeness, boundaries) = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            scan_remote_tree_checked(&mut provider, "/root", &opts),
        )
        .await
        .expect("a walk that follows the loop never finishes");
        let mut paths: Vec<_> = rows.iter().map(|r| r.rel_path.clone()).collect();
        paths.sort();
        assert_eq!(paths, vec!["a.txt", "sub/b.txt"]);
        assert!(completeness.is_complete());
        // Not walked, and reported, so a sync leaves its path alone.
        assert_eq!(
            boundaries.links,
            vec![SkippedLink {
                rel_path: "loop".to_string(),
                link_target: None,
            }]
        );
    }

    fn link(rel: &str) -> SkippedLink {
        SkippedLink {
            rel_path: rel.to_string(),
            link_target: None,
        }
    }

    /// The bound is the link and everything under it, by path component: a
    /// sibling that merely starts with the same characters is not under it.
    #[test]
    fn link_bound_covers_the_link_and_its_subtree_and_nothing_else() {
        let bound = ScanBound::for_sync(
            "/nonexistent-local-root",
            std::iter::empty(),
            std::iter::empty(),
            &ScanBoundaries::default(),
            ScanBoundaries {
                links: vec![link("a/link")],
                ..ScanBoundaries::default()
            },
        );
        assert!(bound.covers("a/link"));
        assert!(bound.covers("a/link/x.txt"));
        assert!(bound.covers("a/link/deep/y.txt"));
        assert!(!bound.covers("a/link2/x.txt"));
        assert!(!bound.covers("a/linkx.txt"));
        assert!(!bound.covers("a"));
        assert!(!bound.covers("b/a/link/x.txt"));
    }

    /// The local side of the bound: a path only the remote holds, sitting under
    /// a local symlink the local walk skipped, reveals that link on disk. A
    /// remote-only path with no link above it adds nothing.
    #[cfg(unix)]
    #[test]
    fn link_bound_finds_a_local_link_above_a_path_only_the_remote_holds() {
        let local = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(local.path().join("real")).unwrap();
        std::fs::write(local.path().join("real/x.txt"), b"x").unwrap();
        std::os::unix::fs::symlink("real", local.path().join("link")).unwrap();
        let bound = ScanBound::for_sync(
            local.path().to_str().unwrap(),
            ["real/x.txt"],
            ["real/x.txt", "link/x.txt", "other/y.txt"],
            &ScanBoundaries::default(),
            ScanBoundaries::default(),
        );
        assert_eq!(
            bound.links(),
            &[SkippedLink {
                rel_path: "link".to_string(),
                link_target: Some("real".to_string()),
            }]
        );
        assert!(bound.covers("link/x.txt"));
        assert!(!bound.covers("real/x.txt"));
        assert!(!bound.covers("other/y.txt"));
    }

    /// Applied to a sync scan, the bound drops what it covers on both sides and
    /// leaves every other entry alone.
    #[test]
    fn link_bound_apply_drops_covered_entries_on_both_sides() {
        let local_entry = |rel: &str| LocalEntry {
            rel_path: rel.to_string(),
            size: 1,
            mtime: None,
            sha256: None,
        };
        let remote_entry = |rel: &str| RemoteEntry {
            rel_path: rel.to_string(),
            size: 1,
            mtime: None,
            checksum_alg: None,
            checksum_hex: None,
        };
        let mut locals = vec![local_entry("a.txt"), local_entry("link/x.txt")];
        let mut remotes = vec![remote_entry("a.txt"), remote_entry("link")];
        let bound = ScanBound::apply(
            "/nonexistent-local-root",
            &mut locals,
            &mut remotes,
            &ScanBoundaries::default(),
            ScanBoundaries {
                links: vec![link("link")],
                ..ScanBoundaries::default()
            },
        );
        assert_eq!(bound.links().len(), 1);
        let local_paths: Vec<_> = locals.iter().map(|e| e.rel_path.as_str()).collect();
        let remote_paths: Vec<_> = remotes.iter().map(|e| e.rel_path.as_str()).collect();
        assert_eq!(local_paths, vec!["a.txt"]);
        assert_eq!(remote_paths, vec!["a.txt"]);
    }

    /// The local side of the bound when the local tree cannot be read: a link
    /// inside a directory that is readable but not traversable (0400) fails its
    /// lstat with a permission error, not with absence. Its path is left out on
    /// both sides instead of being taken as absent, which left the remote files
    /// under it free to be planned for deletion.
    #[cfg(unix)]
    #[test]
    fn link_bound_protects_a_local_prefix_it_cannot_read() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir(root.join("real")).unwrap();
        let locked = root.join("locked");
        fs::create_dir(&locked).unwrap();
        std::os::unix::fs::symlink("../real", locked.join("link")).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o400)).unwrap();
        let blocked = fs::symlink_metadata(locked.join("link")).is_err();
        let bound = ScanBound::for_sync(
            root.to_str().unwrap(),
            std::iter::empty(),
            ["locked/link/x.txt"],
            &ScanBoundaries::default(),
            ScanBoundaries::default(),
        );
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).unwrap();
        if !blocked {
            // Running as root, or on a filesystem that ignores the mode.
            return;
        }
        assert!(
            bound.covers("locked/link/x.txt"),
            "a prefix that cannot be read is left out, not taken as absent"
        );
        assert_eq!(
            bound.unseen(),
            &[UnseenPath {
                rel_path: "locked/link".to_string(),
                reason: "unreadable",
            }]
        );
    }

    /// A symlink the local walk lists but cannot resolve, because it sits in a
    /// 0400 directory: what it points at is unknown, so the scan is incomplete.
    #[cfg(unix)]
    #[test]
    fn scan_local_tree_is_incomplete_when_it_cannot_resolve_a_listed_link() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir(root.join("real")).unwrap();
        let locked = root.join("locked");
        fs::create_dir(&locked).unwrap();
        std::os::unix::fs::symlink("../real", locked.join("link")).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o400)).unwrap();
        let blocked = fs::metadata(locked.join("link")).is_err();
        let (_, completeness, _) =
            scan_local_tree_checked(root.to_str().unwrap(), &ScanOptions::default());
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).unwrap();
        if !blocked {
            return;
        }
        assert!(
            !completeness.is_complete(),
            "a listed link that could not be resolved makes the scan incomplete"
        );
    }

    /// Skipped links count against the scan's entry cap like entries do: a
    /// directory holding many links and few files must not grow the link list
    /// without bound, and a scan cut off at the cap says so.
    #[tokio::test]
    async fn a_scan_counts_skipped_links_against_its_entry_cap() {
        use crate::providers::RemoteEntry;
        let mut listing = vec![RemoteEntry::file(
            "a.txt".to_string(),
            "/root/a.txt".to_string(),
            1,
        )];
        for i in 0..5 {
            let mut link = RemoteEntry::directory(format!("link{i}"), format!("/root/link{i}"));
            link.is_symlink = true;
            listing.push(link);
        }
        let mut provider: Box<dyn StorageProvider> = Box::new(TreeProvider {
            dirs: std::collections::HashMap::from([("/root".to_string(), listing)]),
        });
        let opts = ScanOptions {
            disable_recursive_fastpath: true,
            max_entries: Some(3),
            ..ScanOptions::default()
        };
        let (_rows, completeness, boundaries) =
            scan_remote_tree_checked(&mut provider, "/root", &opts).await;
        assert!(
            !completeness.is_complete(),
            "a scan cut off at the cap is not complete"
        );
        assert!(
            boundaries.links.len() <= 3,
            "the links stay within the cap, got {}",
            boundaries.links.len()
        );
        assert_eq!(
            boundaries.unbounded,
            Some("entry_cap"),
            "the part cut off has no name, so a sync must refuse rather than bound it"
        );
    }

    /// A directory the walk reaches at its depth limit is not listed; the scan
    /// names it, so a sync can leave what it might hold alone on both sides.
    #[tokio::test]
    async fn a_directory_at_the_depth_limit_is_reported_unseen() {
        use crate::providers::RemoteEntry;
        let mut provider: Box<dyn StorageProvider> = Box::new(TreeProvider {
            dirs: std::collections::HashMap::from([
                (
                    "/root".to_string(),
                    vec![
                        RemoteEntry::file("a.txt".to_string(), "/root/a.txt".to_string(), 1),
                        RemoteEntry::directory("d1".to_string(), "/root/d1".to_string()),
                    ],
                ),
                (
                    "/root/d1".to_string(),
                    vec![RemoteEntry::file(
                        "x.txt".to_string(),
                        "/root/d1/x.txt".to_string(),
                        1,
                    )],
                ),
            ]),
        });
        let opts = ScanOptions {
            disable_recursive_fastpath: true,
            max_depth: Some(1),
            ..ScanOptions::default()
        };
        let (rows, completeness, boundaries) =
            scan_remote_tree_checked(&mut provider, "/root", &opts).await;
        let paths: Vec<_> = rows.iter().map(|r| r.rel_path.as_str()).collect();
        assert_eq!(paths, vec!["a.txt"]);
        assert!(completeness.truncated);
        assert_eq!(
            boundaries.unseen,
            vec![UnseenPath {
                rel_path: "d1".to_string(),
                reason: "depth_limit",
            }]
        );
        assert_eq!(
            boundaries.unbounded, None,
            "a named gap can be bounded around"
        );
    }

    /// The bound knows the directories above every bounded path, and only those.
    #[test]
    fn scan_bound_knows_the_directories_above_a_bounded_path() {
        let bound = ScanBound::for_sync(
            "/nonexistent-local-root",
            std::iter::empty(),
            std::iter::empty(),
            &ScanBoundaries::default(),
            ScanBoundaries {
                links: vec![link("a/b/link")],
                ..ScanBoundaries::default()
            },
        );
        assert!(bound.holds_bounded_paths("a"));
        assert!(bound.holds_bounded_paths("a/b"));
        assert!(
            !bound.holds_bounded_paths("a/b/link"),
            "the bounded path itself is covered, not above"
        );
        assert!(!bound.holds_bounded_paths("c"));
    }

    /// A scan that missed a part of the tree it cannot name leaves nothing to
    /// bound around: the bound refuses the run and says which side and why.
    #[test]
    fn scan_bound_refuses_a_run_a_scan_could_not_bound() {
        let bound = ScanBound::for_sync(
            "/nonexistent-local-root",
            std::iter::empty(),
            std::iter::empty(),
            &ScanBoundaries::default(),
            ScanBoundaries {
                unbounded: Some("cancelled"),
                ..ScanBoundaries::default()
            },
        );
        let refusal = bound.refusal().expect("an unnamed gap refuses the run");
        assert!(
            refusal.contains("remote") && refusal.contains("cancelled"),
            "{refusal}"
        );
        let bounded = ScanBound::for_sync(
            "/nonexistent-local-root",
            std::iter::empty(),
            std::iter::empty(),
            &ScanBoundaries::default(),
            ScanBoundaries::default(),
        );
        assert_eq!(bounded.refusal(), None);
    }

    /// An in-memory tree that lists on independent clones and counts how many
    /// directory listings are in flight at once. Each sub-directory listing
    /// waits at a rendezvous until `parties` are open together, so a walk
    /// finishes only if the walker really lists that many directories at once.
    pub(crate) struct PoolTreeProvider {
        pub(crate) dirs: std::collections::HashMap<String, Vec<crate::providers::RemoteEntry>>,
        /// What the mock claims to be; the default is SFTP.
        pub(crate) provider_type: crate::providers::ProviderType,
        /// Streams the last `set_multi_thread_download` armed (0 = never called).
        pub(crate) armed_streams: Arc<std::sync::atomic::AtomicUsize>,
        pub(crate) ceiling: u16,
        pub(crate) in_flight: Arc<std::sync::atomic::AtomicUsize>,
        pub(crate) peak: Arc<std::sync::atomic::AtomicUsize>,
        pub(crate) rendezvous: Arc<tokio::sync::Barrier>,
    }

    impl PoolTreeProvider {
        /// A root with `width` sub-directories of one file each; the rendezvous
        /// opens when `width` sub-directory listings are in flight together.
        pub(crate) fn fan(width: usize, ceiling: u16) -> Self {
            use crate::providers::RemoteEntry;
            let mut dirs = std::collections::HashMap::new();
            dirs.insert(
                "/root".to_string(),
                (0..width)
                    .map(|i| RemoteEntry::directory(format!("d{i}"), format!("/root/d{i}")))
                    .collect(),
            );
            for i in 0..width {
                dirs.insert(
                    format!("/root/d{i}"),
                    vec![RemoteEntry::file(
                        "f.txt".to_string(),
                        format!("/root/d{i}/f.txt"),
                        1,
                    )],
                );
            }
            Self {
                dirs,
                provider_type: crate::providers::ProviderType::Sftp,
                armed_streams: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                ceiling,
                in_flight: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                peak: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                rendezvous: Arc::new(tokio::sync::Barrier::new(width)),
            }
        }
    }

    #[async_trait::async_trait]
    impl StorageProvider for PoolTreeProvider {
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
        fn provider_type(&self) -> crate::providers::ProviderType {
            self.provider_type
        }
        fn display_name(&self) -> String {
            "pool-tree".to_string()
        }
        fn set_multi_thread_download(&mut self, streams: usize, _cutoff_bytes: u64) {
            self.armed_streams
                .store(streams, std::sync::atomic::Ordering::SeqCst);
        }
        fn list_executor_kind(&self) -> crate::providers::ProviderListExecutorKind {
            crate::providers::ProviderListExecutorKind::HttpClonePool
        }
        fn list_executor_max_sessions(&self) -> u16 {
            self.ceiling
        }
        fn clone_for_list(&self) -> Result<Box<dyn StorageProvider>, ProviderError> {
            Ok(Box::new(Self {
                dirs: self.dirs.clone(),
                provider_type: self.provider_type,
                armed_streams: Arc::clone(&self.armed_streams),
                ceiling: self.ceiling,
                in_flight: Arc::clone(&self.in_flight),
                peak: Arc::clone(&self.peak),
                rendezvous: Arc::clone(&self.rendezvous),
            }))
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
            use std::sync::atomic::Ordering;
            let entries = self
                .dirs
                .get(path)
                .cloned()
                .ok_or_else(|| ProviderError::NotFound(path.to_string()))?;
            if path != "/root" {
                let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                self.peak.fetch_max(now, Ordering::SeqCst);
                self.rendezvous.wait().await;
                self.in_flight.fetch_sub(1, Ordering::SeqCst);
            }
            Ok(entries)
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
            Ok(())
        }
        async fn delete(&mut self, _path: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn rmdir(&mut self, _path: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn rmdir_recursive(&mut self, _path: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn rename(&mut self, _from: &str, _to: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn stat(
            &mut self,
            path: &str,
        ) -> Result<crate::providers::RemoteEntry, ProviderError> {
            Err(ProviderError::NotFound(path.to_string()))
        }
        async fn size(&mut self, _path: &str) -> Result<u64, ProviderError> {
            Ok(1)
        }
        async fn exists(&mut self, path: &str) -> Result<bool, ProviderError> {
            Ok(self.dirs.contains_key(path))
        }
        async fn keep_alive(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn server_info(&mut self) -> Result<String, ProviderError> {
            Ok("pool-tree".to_string())
        }
    }

    /// A listing entry named `..` (provider-controlled) must never become a
    /// row: with the serial walker gone, the guard has to live in the shared
    /// per-directory walk that both the locked and the pooled branch use.
    #[tokio::test]
    async fn a_traversing_remote_name_never_becomes_a_row() {
        use crate::providers::RemoteEntry;
        let mut dirs = std::collections::HashMap::new();
        dirs.insert(
            "/root".to_string(),
            vec![
                RemoteEntry::file("ok.txt".to_string(), "/root/ok.txt".to_string(), 1),
                RemoteEntry::file(
                    "../evil.txt".to_string(),
                    "/root/../evil.txt".to_string(),
                    1,
                ),
                RemoteEntry::directory("..".to_string(), "/".to_string()),
            ],
        );
        let mut provider: Box<dyn StorageProvider> = Box::new(TreeProvider { dirs });
        let opts = ScanOptions {
            disable_recursive_fastpath: true,
            ..ScanOptions::default()
        };
        let (rows, completeness, _) = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            scan_remote_tree_checked(&mut provider, "/root", &opts),
        )
        .await
        .expect("a walk that follows `..` never finishes");
        let paths: Vec<_> = rows.iter().map(|r| r.rel_path.as_str()).collect();
        assert_eq!(paths, vec!["ok.txt"], "the traversing entries are dropped");
        assert!(
            completeness.is_complete(),
            "a dropped hostile name is not a listing failure, as on main before"
        );
    }

    /// The walker every sync path calls lists `checkers` directories at once
    /// on a pool-backed provider, and hands the caller's provider back.
    #[tokio::test]
    async fn the_shared_walker_lists_checkers_directories_at_once_and_returns_the_provider() {
        let counting = PoolTreeProvider::fan(8, 8);
        let peak = Arc::clone(&counting.peak);
        let mut provider: Box<dyn StorageProvider> = Box::new(counting);
        let opts = ScanOptions {
            disable_recursive_fastpath: true,
            checkers: Some(8),
            ..ScanOptions::default()
        };
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            scan_remote_tree_checked(&mut provider, "/root", &opts),
        )
        .await;
        let delivered = peak.load(std::sync::atomic::Ordering::SeqCst);
        let (rows, completeness, _) = outcome.unwrap_or_else(|_| {
            panic!("the walk listed at most {delivered} directories at once, 8 requested")
        });
        assert_eq!(rows.len(), 8);
        assert!(completeness.is_complete());
        assert_eq!(delivered, 8);
        assert_eq!(
            provider.display_name(),
            "pool-tree",
            "the caller gets its own provider back, not the placeholder"
        );
    }
}
