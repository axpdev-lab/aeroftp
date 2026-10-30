// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! AeroCloud incremental sync: the paths the watcher saw change, one by one.
//!
//! A full sync lists the whole remote folder. Between two full scans the
//! watcher says which local paths changed, and this pass syncs those paths
//! only. It reads each one on disk and against the baseline first, and asks
//! the remote about a path (one `stat`) only when the local side changed
//! since the baseline. The answer goes through the classifier, conflict
//! strategy and executor the full sync uses, so a path ends where a full sync
//! would have put it. It never lists a remote folder.
//!
//! What it cannot see is a change made on the remote by another device: that
//! arrives with the next full scan. For the same reason it never deletes a
//! local file because its remote copy is gone. That is a remote change, and
//! the full scan decides it behind its safety gate, which knows a remote
//! folder that vanished as a whole from a few files deleted on purpose.

use super::{
    open_aerocloud_provider, CloudService, FailedPath, PairCycleError, PairCycleReport,
    ProviderScanTraits, ScanScope, SkippedEntry, SyncOperationResult, UpdatePolicy,
};
use crate::cloud_config::{CloudConfig, CloudSyncStatus};
use crate::credential_store;
use crate::providers::{ProviderError, RemoteEntry, StorageProvider};
use crate::sync::{
    build_comparison_results_with_index, decide_sync_action, local_changed_since,
    normalize_relative_key, save_sync_index, CompareDirection, CompareOptions, FileComparison,
    FileInfo, SyncAction, SyncIndex, SyncIndexEntry, SyncStatus, SYNC_INDEX_VERSION,
};
use crate::sync_ignore::AeroIgnore;
use chrono::{DateTime, Utc};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter};

/// The most paths that need the remote one incremental pass takes on while a
/// full scan is allowed. Past it one listing of the folder costs less than a
/// `stat` per path, and the scheduler runs the full scan instead. When the
/// provider's floor keeps the full scan away, the pass takes them all.
pub const REMOTE_WORK_LIMIT: usize = 500;

/// The most local entries one pass reads while it expands the folders that
/// changed (a folder moved in, renamed or removed). Past it the change is a
/// whole tree, which only a full scan reads. Small under test, whose trees
/// the test writes.
const LOCAL_EXPANSION_LIMIT: usize = if cfg!(test) { 64 } else { 50_000 };

/// More renames than this in one pass are not tried on the remote one by
/// one: they are synced as removals and creations.
const RENAME_LIMIT: usize = 256;

/// What the watcher reported for one pair since its last cycle, as absolute
/// local paths. Lost events are not here: the scheduler answers them with a
/// full scan (see `crate::cloud_scan_schedule`).
#[derive(Debug, Clone, Default)]
pub struct LocalChanges {
    pub paths: Vec<PathBuf>,
    /// `(from, to)`: their paths are in `paths` too.
    pub renames: Vec<(PathBuf, PathBuf)>,
}

impl LocalChanges {
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty() && self.renames.is_empty()
    }
}

/// What the scheduler knows about a pair's provider before connecting.
#[derive(Debug, Clone, Copy)]
pub struct IncrementalHints {
    /// Whether local sizes are read against the baseline. False for a pair
    /// whose provider does not report exact sizes (an overlay that sizes on
    /// the wire, whose baseline size is the wire's). Unknown reads as true,
    /// which can only send more paths to the remote, never fewer.
    pub compare_size: bool,
}

/// The incremental pass for one pair, planned on the local side.
pub enum IncrementalPrep {
    /// Every changed path settled without asking the remote.
    Settled { full_scan_wanted: Option<String> },
    /// Some paths need the remote.
    Work {
        plan: Box<IncrementalPlan>,
        full_scan_wanted: Option<String>,
    },
    /// No incremental pass can run: only a full scan can sync the pair.
    FullScanOnly(String),
}

/// The paths of one incremental pass that need the remote.
pub struct IncrementalPlan {
    renames: Vec<PlannedRename>,
    /// Relative key -> the local copy, `None` when it is gone.
    work: BTreeMap<String, Option<FileInfo>>,
    /// The baseline, with the planned renames applied.
    index: SyncIndex,
}

impl IncrementalPlan {
    /// How many remote round trips the plan starts with: one per path, one
    /// per rename.
    pub fn remote_work(&self) -> usize {
        self.work.len() + self.renames.len()
    }
}

/// A local rename to replay on the remote, so the copy there moves instead
/// of being deleted and sent again.
struct PlannedRename {
    from: String,
    to: String,
    is_dir: bool,
    /// The baseline entries the rename moved, `(old key, new key)`.
    moved: Vec<(String, String)>,
    /// What the renamed paths need when the remote refuses the rename: the
    /// old ones a delete, the new ones an upload.
    fallback: BTreeMap<String, Option<FileInfo>>,
}

impl PlannedRename {
    /// Move the baseline entries under `from` to `to`.
    fn apply(&mut self, index: &mut SyncIndex) {
        let old_keys: Vec<String> = index
            .files
            .keys()
            .filter(|key| *key == &self.from || is_under(key, &self.from))
            .cloned()
            .collect();
        for old in old_keys {
            let new = format!("{}{}", self.to, &old[self.from.len()..]);
            if let Some(entry) = index.files.remove(&old) {
                index.files.insert(new.clone(), entry);
                self.moved.push((old, new));
            }
        }
    }

    /// Put the moved entries back.
    fn restore(&self, index: &mut SyncIndex) {
        for (old, new) in &self.moved {
            if let Some(entry) = index.files.remove(new) {
                index.files.insert(old.clone(), entry);
            }
        }
    }
}

/// A local path as the full scan would read it.
#[derive(Debug, Clone)]
enum Local {
    File(FileInfo),
    Dir(FileInfo),
    Absent,
    /// A link, a path under one, or a path that cannot be read: the scan
    /// leaves it out, so nothing is decided about it.
    Skipped,
}

impl Local {
    fn info(&self) -> Option<&FileInfo> {
        match self {
            Local::File(info) | Local::Dir(info) => Some(info),
            Local::Absent | Local::Skipped => None,
        }
    }
}

/// Whether `path` is strictly below the folder `dir` (both relative keys).
fn is_under(path: &str, dir: &str) -> bool {
    path.len() > dir.len() && path.starts_with(dir) && path.as_bytes()[dir.len()] == b'/'
}

/// A legacy alias must not be the literal name of another local sibling.
/// Read actual directory names: `exists(alias)` also succeeds for the same
/// object on normalization-insensitive filesystems such as macOS APFS.
fn legacy_alias_is_unoccupied(
    root: &Path,
    requested: &str,
    alias: &str,
    listings: &mut HashMap<PathBuf, Option<HashSet<std::ffi::OsString>>>,
) -> bool {
    let mut parent = root.to_path_buf();
    for (want, candidate) in requested.split('/').zip(alias.split('/')) {
        if want != candidate {
            let names = listings.entry(parent.clone()).or_insert_with(|| {
                match std::fs::read_dir(&parent) {
                    Ok(entries) => entries
                        .map(|entry| entry.map(|e| e.file_name()))
                        .collect::<Result<HashSet<_>, _>>()
                        .ok(),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(HashSet::new()),
                    Err(_) => None,
                }
            });
            let Some(names) = names else { return false };
            if names.contains(std::ffi::OsStr::new(candidate)) {
                return false;
            }
        }
        parent.push(candidate);
    }
    true
}

/// Whether one path is the other or under it.
fn related(a: &str, b: &str) -> bool {
    a == b || is_under(a, b) || is_under(b, a)
}

/// Reads local paths the way the full scan does: links are not followed.
struct Disk<'a> {
    root: &'a Path,
    /// The root with its links resolved, when that is another path: FSEvents
    /// reports paths under it (`/var` is `/private/var` on macOS).
    resolved_root: Option<PathBuf>,
    /// Folder key -> whether it is a link or unreadable.
    blocked: HashMap<String, bool>,
}

impl<'a> Disk<'a> {
    fn new(root: &'a Path) -> Self {
        Self {
            root,
            resolved_root: root.canonicalize().ok().filter(|resolved| resolved != root),
            blocked: HashMap::new(),
        }
    }

    /// The relative key of an absolute path under the root, `None` for the
    /// root itself, a path outside it, or a name that is not UTF-8 (the scan
    /// keys such a name lossily, and the lossy key names no file on disk: read
    /// as gone, it would delete the remote copy).
    fn key(&self, path: &Path) -> Option<String> {
        let relative = match path.strip_prefix(self.root) {
            Ok(relative) => relative,
            Err(_) => path.strip_prefix(self.resolved_root.as_deref()?).ok()?,
        };
        let key = normalize_relative_key(relative.to_str()?);
        (!key.is_empty()).then_some(key)
    }

    /// Whether a folder above `key` is a link or cannot be read.
    fn under_blocked(&mut self, key: &str) -> bool {
        let root = self.root;
        key.match_indices('/').any(|(end, _)| {
            let prefix = &key[..end];
            *self.blocked.entry(prefix.to_string()).or_insert_with(|| {
                match std::fs::symlink_metadata(root.join(prefix)) {
                    Ok(meta) => meta.file_type().is_symlink(),
                    Err(e) => e.kind() != std::io::ErrorKind::NotFound,
                }
            })
        })
    }

    fn entry(&mut self, key: &str) -> Local {
        if self.under_blocked(key) {
            return Local::Skipped;
        }
        let path = self.root.join(key);
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() => Local::Skipped,
            Ok(meta) => {
                let name = key.rsplit('/').next().unwrap_or(key).to_string();
                local_info(&path, &meta, name)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Local::Absent,
            Err(_) => Local::Skipped,
        }
    }

    /// Read the folder `key` and everything the scan walks under it into
    /// `out`, spending `budget`. `Err` when the budget runs out.
    fn walk(
        &mut self,
        key: &str,
        scope: &ScanScope,
        out: &mut BTreeMap<String, Local>,
        budget: &mut usize,
    ) -> Result<(), ()> {
        // A folder that cannot be read keeps what it holds for the full scan.
        let Ok(entries) = std::fs::read_dir(self.root.join(key)) else {
            return Ok(());
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = path.symlink_metadata() else {
                continue;
            };
            if meta.file_type().is_symlink() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            *budget = budget.checked_sub(1).ok_or(())?;
            let child = format!("{key}/{name}");
            let local = local_info(&path, &meta, name);
            let is_dir = matches!(local, Local::Dir(_));
            out.insert(child.clone(), local);
            if is_dir && scope.walks(&child) {
                self.walk(&child, scope, out, budget)?;
            }
        }
        Ok(())
    }
}

fn local_info(path: &Path, meta: &std::fs::Metadata, name: String) -> Local {
    let is_dir = meta.is_dir();
    let info = FileInfo {
        name,
        path: path.to_string_lossy().into_owned(),
        size: if is_dir { 0 } else { meta.len() },
        modified: meta.modified().ok().map(DateTime::<Utc>::from),
        is_dir,
        checksum: None,
        checksum_alg: None,
    };
    if is_dir {
        Local::Dir(info)
    } else {
        Local::File(info)
    }
}

/// Plan the incremental pass for `changes`: everything decided on the local
/// side, before any connection. Loads the pair's baseline.
pub fn prepare_incremental(
    config: &CloudConfig,
    changes: &LocalChanges,
    hints: IncrementalHints,
) -> Result<IncrementalPrep, String> {
    let local = config.local_folder.to_string_lossy().to_string();
    // An index that cannot be read is no baseline, as the full sync reads it.
    let index = crate::sync::load_sync_index_for_profile(
        &local,
        &config.remote_folder,
        config.server_profile_id.as_deref(),
    )
    .ok()
    .flatten();
    plan(config, index, changes, hints)
}

fn plan(
    config: &CloudConfig,
    index: Option<SyncIndex>,
    changes: &LocalChanges,
    hints: IncrementalHints,
) -> Result<IncrementalPrep, String> {
    let root = config.local_folder.as_path();
    if !root.is_dir() {
        return Err(format!(
            "{}: the local folder is missing; nothing is synced until it is back",
            root.display()
        ));
    }
    let Some(mut index) = index else {
        return Ok(IncrementalPrep::FullScanOnly(
            "the pair has no baseline yet".to_string(),
        ));
    };
    if !index.unverified_keys.is_empty() {
        // Written back by a pass that compared only a few of them, they would
        // read as synced (see `save_unsettled_into`); a full scan compares
        // them all.
        return Ok(IncrementalPrep::FullScanOnly(
            "the baseline holds paths an older index wrote that only a full scan can confirm"
                .to_string(),
        ));
    }
    let scope = ScanScope::new(config)?;
    let mut disk = Disk::new(root);
    let mut full_scan_wanted = None;

    let mut touched: BTreeSet<String> = changes
        .paths
        .iter()
        .filter_map(|path| disk.key(path))
        .collect();
    let mut candidates = Vec::new();
    for (from, to) in &changes.renames {
        match (disk.key(from), disk.key(to)) {
            (Some(from), Some(to)) if from != to => {
                touched.insert(from.clone());
                touched.insert(to.clone());
                candidates.push((from, to));
            }
            (from, to) => touched.extend(from.into_iter().chain(to)),
        }
    }
    if touched.contains(".aeroignore") {
        // What the new rules include that the old ones left out was never
        // reported by the watcher: only a full scan reads it.
        full_scan_wanted = Some("the .aeroignore rules changed".to_string());
    }

    // A remote rename could replace a newer destination without consulting
    // its timestamp. One-way Update compares both paths before any write.
    let mut renames =
        if config.update_only && config.sync_direction != CompareDirection::Bidirectional {
            Vec::new()
        } else {
            plan_renames(&candidates, &mut index, &mut disk, &scope, config)
        };

    // Every path the changes reach: the changed ones, what a folder that
    // changed holds on disk, and what the baseline holds under it (a folder
    // replaced or removed leaves baseline entries no event names).
    let too_many = || {
        Ok(IncrementalPrep::FullScanOnly(
            "too many local paths changed at once".to_string(),
        ))
    };
    let mut locals: BTreeMap<String, Local> = BTreeMap::new();
    let mut budget = LOCAL_EXPANSION_LIMIT;
    let baseline_keys: BTreeSet<String> = index.files.keys().cloned().collect();
    for key in &touched {
        let local = disk.entry(key);
        let walk = matches!(local, Local::Dir(_)) && scope.walks(key);
        locals.insert(key.clone(), local);
        if walk && disk.walk(key, &scope, &mut locals, &mut budget).is_err() {
            return too_many();
        }
        // `/` sorts right before `0`: the range is every key under `key/`.
        for below in baseline_keys.range(format!("{key}/")..format!("{key}0")) {
            if !locals.contains_key(below) {
                let Some(left) = budget.checked_sub(1) else {
                    return too_many();
                };
                budget = left;
                let local = disk.entry(below);
                locals.insert(below.clone(), local);
            }
        }
    }

    let mut work = BTreeMap::new();
    for (key, local) in &locals {
        let base = index.files.get(key);
        let is_dir = match local {
            Local::Dir(_) => true,
            Local::File(_) => false,
            Local::Absent => base.is_some_and(|entry| entry.is_dir),
            Local::Skipped => continue,
        };
        // Outside the scan's rules: neither side of it is synced.
        if !scope.lists(key, is_dir) {
            continue;
        }
        match (local, base) {
            (Local::File(info), Some(entry))
                if !entry.is_dir && !changed_since(info, entry, hints.compare_size) => {}
            (Local::Dir(_), Some(entry)) if entry.is_dir => {}
            (Local::Absent, None)
                if config.update_only
                    && config.sync_direction == CompareDirection::LocalToRemote =>
            {
                // A remove event can name a file this device never synced.
                // Stat the destination so it can be kept and reported, never
                // inferred to be a user-approved delete from the event alone.
                work.insert(key.clone(), None);
            }
            (Local::Absent, None) => {}
            (Local::Absent, Some(_)) => {
                work.insert(key.clone(), None);
            }
            (Local::File(info) | Local::Dir(info), _) => {
                work.insert(key.clone(), Some(info.clone()));
            }
            (Local::Skipped, _) => {}
        }
    }

    // What each rename's paths need if the remote refuses it: the moved
    // baseline entries read as deleted, the new paths as new.
    for rename in &mut renames {
        for (old, new) in &rename.moved {
            let is_dir = index.files.get(new).is_some_and(|entry| entry.is_dir);
            if scope.lists(old, is_dir) {
                rename.fallback.insert(old.clone(), None);
            }
        }
        for (key, local) in &locals {
            if key == &rename.to || is_under(key, &rename.to) {
                if let Some(info) = local.info().filter(|info| scope.lists(key, info.is_dir)) {
                    rename.fallback.insert(key.clone(), Some(info.clone()));
                }
            }
        }
    }

    if work.is_empty() && renames.is_empty() {
        return Ok(IncrementalPrep::Settled { full_scan_wanted });
    }
    Ok(IncrementalPrep::Work {
        plan: Box::new(IncrementalPlan {
            renames,
            work,
            index,
        }),
        full_scan_wanted,
    })
}

/// Whether the local file changed since the baseline, as the classifier reads
/// it, and more readily: a time either side lacks reads as a change, so the
/// remote is asked rather than the path settled on a guess.
fn changed_since(local: &FileInfo, entry: &SyncIndexEntry, compare_size: bool) -> bool {
    local.modified.is_none()
        || entry.modified.is_none()
        || local_changed_since(local, entry, compare_size)
}

/// The renames among `candidates` that can be replayed on the remote, with
/// their baseline entries moved in `index`. A rename qualifies when it is a
/// pure move a full sync would turn into "delete the old remote copy, send
/// the new local one": the old path is gone locally and synced, the new one
/// exists locally with the same kind and nothing synced at or under it, the
/// pair's direction sends new files and propagates local deletes, and no
/// other rename of the pass touches the same paths (each must stand alone,
/// since the remote may refuse any of them).
fn plan_renames(
    candidates: &[(String, String)],
    index: &mut SyncIndex,
    disk: &mut Disk<'_>,
    scope: &ScanScope,
    config: &CloudConfig,
) -> Vec<PlannedRename> {
    let mut planned = Vec::new();
    if candidates.len() > RENAME_LIMIT {
        return planned;
    }
    let deletes_remote = decide_sync_action(
        &SyncStatus::RemoteOnly,
        &config.sync_direction,
        true,
        config.preserve_remote_deletes,
    ) == SyncAction::DeleteRemote;
    let uploads = decide_sync_action(
        &SyncStatus::LocalOnly,
        &config.sync_direction,
        false,
        config.preserve_remote_deletes,
    ) == SyncAction::Upload;
    if !deletes_remote || !uploads {
        return planned;
    }
    let literal_endpoints: Vec<bool> = {
        let remote_keys = crate::sync::RemoteKeyIndex::build(index);
        candidates
            .iter()
            .map(|(from, to)| {
                remote_keys.project(from).as_deref() == Some(from.as_str())
                    && remote_keys.project(to).as_deref() == Some(to.as_str())
            })
            .collect()
    };
    for (i, (from, to)) in candidates.iter().enumerate() {
        let overlaps = candidates
            .iter()
            .enumerate()
            .any(|(j, (other_from, other_to))| {
                i != j
                    && [other_from, other_to]
                        .iter()
                        .any(|other| related(from, other) || related(to, other))
            });
        if overlaps || related(from, to) {
            continue;
        }
        // A folder may have no row of its own (a backend that lists none)
        // while the files under it are synced.
        let is_dir = match index.files.get(from) {
            Some(entry) => entry.is_dir,
            None if index.files.keys().any(|key| is_under(key, from)) => true,
            None => continue,
        };
        let kind_matches = match disk.entry(to) {
            Local::Dir(_) => is_dir,
            Local::File(_) => !is_dir,
            Local::Absent | Local::Skipped => false,
        };
        if !kind_matches
            || !matches!(disk.entry(from), Local::Absent)
            || !scope.lists(from, is_dir)
            || !scope.lists(to, is_dir)
            || index.files.keys().any(|key| key == to || is_under(key, to))
        {
            continue;
        }
        // Fast rename uses literal paths. If either endpoint (or a child)
        // has another remote identity, let the ordinary mapped paths handle
        // it instead of moving a possibly unrelated literal remote sibling.
        if !literal_endpoints[i]
            || index
                .remote_spellings
                .keys()
                .any(|key| key == from || is_under(key, from))
        {
            continue;
        }
        let mut rename = PlannedRename {
            from: from.clone(),
            to: to.clone(),
            is_dir,
            moved: Vec::new(),
            fallback: BTreeMap::new(),
        };
        rename.apply(index);
        planned.push(rename);
    }
    planned
}

/// What the remote holds at `path`: `None` when nothing. A `stat` that fails
/// for another reason than absence is settled by `exists`, the way the full
/// sync settles a failed `cd` of its folder; one that cannot answer either
/// is an error, never an absence.
async fn remote_state<P: StorageProvider + ?Sized>(
    provider: &mut P,
    path: &str,
) -> Result<Option<RemoteEntry>, String> {
    match provider.stat(path).await {
        Ok(entry) => Ok(Some(entry)),
        Err(ProviderError::NotFound(_)) => Ok(None),
        Err(stat_error) => match provider.exists(path).await {
            Ok(false) => Ok(None),
            Ok(true) => Err(format!("it exists but cannot be read: {stat_error}")),
            Err(e) => Err(format!("{stat_error}; checking it exists failed too: {e}")),
        },
    }
}

/// The remote copy as the full scan's listing would describe it.
fn remote_info(key: &str, path: &str, entry: &RemoteEntry) -> FileInfo {
    FileInfo {
        name: key.rsplit('/').next().unwrap_or(key).to_string(),
        path: path.to_string(),
        size: if entry.is_dir { 0 } else { entry.size },
        modified: entry
            .modified
            .as_deref()
            .and_then(crate::parse_remote_datetime),
        is_dir: entry.is_dir,
        checksum_alg: None,
        checksum: entry.metadata.get("content_hash").cloned(),
    }
}

/// Whether the local folder holds nothing, or is gone: a pass that would
/// propagate deletes from such a folder trips the safety gate.
pub(super) fn local_root_is_empty(root: &Path) -> bool {
    std::fs::read_dir(root).map_or(true, |mut entries| entries.next().is_none())
}

/// How an executed pass ended.
pub(super) enum IncrementalRun {
    /// With the paths that failed.
    Done(SyncOperationResult, Vec<FailedPath>),
    /// The remote refused renames whose paths, synced one by one, came to
    /// more than the pass takes. What ran is reported; the rest needs a full
    /// scan.
    TooLarge(SyncOperationResult),
}

impl CloudService {
    /// Run `plan` against `provider` and save the baseline it leaves.
    pub(super) async fn perform_incremental_sync_with_provider<P: StorageProvider + ?Sized>(
        &self,
        provider: &mut P,
        plan: Box<IncrementalPlan>,
        work_limit: Option<usize>,
    ) -> Result<IncrementalRun, String> {
        let (run, index) = self.execute_incremental(provider, plan, work_limit).await?;
        if let Err(e) = save_sync_index(&index) {
            tracing::warn!("Failed to save AeroCloud sync index: {}", e);
        }
        Ok(run)
    }

    /// Run `plan` against `provider`: the renames first, then one `stat` per
    /// path, the classifier, the delete gate and the executor. Returns the
    /// baseline to save: the plan's, with each operation that succeeded
    /// applied and nothing else. A path that failed keeps its old entry, so
    /// the next pass or scan reads it as it read it now.
    pub(super) async fn execute_incremental<P: StorageProvider + ?Sized>(
        &self,
        provider: &mut P,
        plan: Box<IncrementalPlan>,
        work_limit: Option<usize>,
    ) -> Result<(IncrementalRun, SyncIndex), String> {
        let config = self.config.read().await.clone();
        if !config.enabled {
            return Err("AeroCloud is not enabled".to_string());
        }
        // A watcher plan can outlive the mounted/renamed local root. Refuse
        // before any remote rename or write, rather than interpreting its
        // disappearance as a batch of user deletes.
        if !config.local_folder.is_dir() {
            return Err("AeroCloud local folder is unavailable; incremental sync refused".into());
        }
        let start = std::time::Instant::now();
        let IncrementalPlan {
            renames,
            mut work,
            mut index,
        } = *plan;
        let local_str = config.local_folder.to_string_lossy().to_string();
        let remote_root = config.remote_folder.trim_end_matches('/').to_string();
        let mut result = SyncOperationResult {
            uploaded: 0,
            downloaded: 0,
            deleted: 0,
            skipped: 0,
            skipped_details: Vec::new(),
            conflicts: 0,
            errors: Vec::new(),
            duration_secs: 0,
            file_details: Vec::new(),
        };
        self.forget_landed();
        self.set_status(CloudSyncStatus::Syncing {
            current_file: "Checking changed files...".to_string(),
            progress: 0.0,
            files_done: 0,
            files_total: (work.len() + renames.len()) as u32,
        })
        .await;

        for rename in renames {
            match self.rename_on_remote(provider, &remote_root, &rename).await {
                Ok(()) => tracing::info!(
                    "AeroCloud: renamed '{}' to '{}' on the remote",
                    rename.from,
                    rename.to
                ),
                Err(reason) => {
                    tracing::info!(
                        "AeroCloud: '{}' -> '{}' not renamed on the remote ({}); syncing the paths one by one",
                        rename.from,
                        rename.to,
                        reason
                    );
                    rename.restore(&mut index);
                    work.extend(rename.fallback);
                }
            }
        }
        if work_limit.is_some_and(|limit| work.len() > limit) {
            result.duration_secs = start.elapsed().as_secs();
            return Ok((IncrementalRun::TooLarge(result), index));
        }

        let mut failed = Vec::new();
        // The remote copy of each path, read one by one.
        let mut locals = HashMap::new();
        let mut remotes = HashMap::new();
        let mut gone_on_both = Vec::new();
        // Watcher key -> remote spelling, when the baseline names a different
        // parent than the key the watcher reported. A comparison that still
        // carries the local spelling is pointed here before the upload.
        let mut projected: HashMap<String, String> = HashMap::new();
        // One pass over the baseline. Each watcher path below only looks up.
        let remote_keys = crate::sync::RemoteKeyIndex::build(&index);
        let mut alias_listings = HashMap::new();
        for (key, local) in &work {
            // The watcher key is the local spelling. A file the baseline
            // already names is read at that object's spelling. A new child
            // borrows its parent's. An ambiguous parent is not guessed.
            let Some(remote_key) = remote_keys.project_checked(key, |requested, alias| {
                legacy_alias_is_unoccupied(
                    &config.local_folder,
                    requested,
                    alias,
                    &mut alias_listings,
                )
            }) else {
                result.skipped += 1;
                result.skipped_details.push(SkippedEntry {
                    path: key.clone(),
                    reason: "this path has an ambiguous or occupied remote spelling; it waits for the next full scan".to_string(),
                    is_dir: local.as_ref().is_some_and(|info| info.is_dir),
                    needs_review: false,
                });
                continue;
            };
            if remote_key != *key {
                projected.insert(key.clone(), remote_key.clone());
            }
            let remote_path = format!("{remote_root}/{remote_key}");
            match remote_state(provider, &remote_path).await {
                Ok(Some(entry)) => {
                    remotes.insert(
                        remote_key.to_string(),
                        remote_info(&remote_key, &remote_path, &entry),
                    );
                }
                Ok(None) if local.is_none() => {
                    // The checked projection has no other local owner.
                    // Retire only that one exact/legacy baseline, not twins.
                    let baseline = if index.files.contains_key(key) {
                        key
                    } else {
                        &remote_key
                    };
                    gone_on_both.push(baseline.to_string());
                }
                Ok(None) => {}
                Err(e) => {
                    self.notify_reauth_if_token_revoked(&config.protocol_type, &e);
                    result
                        .errors
                        .push(format!("{key}: cannot read the remote copy: {e}"));
                    failed.push(FailedPath {
                        key: key.clone(),
                        error: e,
                    });
                    continue;
                }
            }
            if let Some(info) = local {
                locals.insert(key.clone(), info.clone());
            }
        }
        Self::bound_local_scan_paths(&local_str, &mut locals, &mut remotes);

        // The options the full plan compares with (`build_sync_plan_with_provider`).
        let options = CompareOptions {
            compare_timestamp: true,
            compare_size: provider.reports_exact_size(),
            compare_checksum: remotes.values().any(|f| f.checksum.is_some()),
            exclude_patterns: config.exclude_patterns.clone(),
            direction: config.sync_direction,
            aeroignore: AeroIgnore::load(&config.local_folder).map(std::sync::Arc::new),
            modify_window: crate::sync_core::mtime::ModifyWindow::against_provider(
                None, &*provider,
            ),
            ..Default::default()
        };
        if !options.modify_window.compares_times() {
            super::log_size_only_window("AeroCloud", &config, options.modify_window);
        }
        self.remember_unsettled(Some(&index));
        let mut comparisons =
            build_comparison_results_with_index(locals, remotes, &options, Some(&index));
        // A new child is absent from the remote inventory, so pairing keeps
        // the local spelling. The parent projection above is where the bytes
        // belong. A row that already names another remote path is left as the
        // classifier paired it.
        for comparison in &mut comparisons {
            if comparison.name_match_blocked {
                continue;
            }
            let Some(remote) = projected.get(&comparison.relative_path) else {
                continue;
            };
            let current = comparison
                .remote_relative_path
                .as_deref()
                .unwrap_or(comparison.relative_path.as_str());
            if current == comparison.relative_path {
                comparison.remote_relative_path = Some(remote.clone());
            }
        }
        if !provider.listing_is_authoritative()
            && !(config.update_only && config.sync_direction != CompareDirection::Bidirectional)
        {
            for c in &mut comparisons {
                if c.status == SyncStatus::LocalOnly {
                    c.previously_synced = false;
                }
            }
        }

        let mut update_policy = UpdatePolicy {
            window: options.modify_window,
            // This bounded pass stats only changed paths, not the entire
            // destination. Missing remote copies are deferred to a full scan
            // below. Unknown remove events are inspected only to report a
            // never-synced destination file; deletion still needs a baseline.
            // A failed stat stays out of the comparisons altogether.
            allow_orphan_deletes: config.sync_direction == CompareDirection::LocalToRemote
                && !local_root_is_empty(&config.local_folder),
        };

        // A local file whose remote copy is gone reads as deleted on the
        // remote: a remote change, left to the full scan and its gate.
        let (deferred, mut comparisons): (Vec<FileComparison>, Vec<FileComparison>) =
            comparisons.into_iter().partition(|c| {
                self.resolve_action(&config, c, update_policy) == SyncAction::DeleteLocal
            });
        for c in &deferred {
            result.skipped += 1;
            result.skipped_details.push(SkippedEntry {
                path: c.relative_path.clone(),
                reason: "the remote copy is gone; deletes from the remote side wait for the next full scan".to_string(),
                is_dir: c.is_dir,
                needs_review: false,
            });
        }

        // The full sync's gate, on what this pass can see: the baseline, the
        // local folder, and the deletes it would propagate.
        let prior_count = index.files.len();
        let pending_deletes = self.count_pending_deletes(&config, &comparisons, update_policy);
        let local_empty = local_root_is_empty(&config.local_folder);
        if Self::delete_safety_trips(prior_count, local_empty, false, pending_deletes, false) {
            update_policy.allow_orphan_deletes = false;
            tracing::warn!(
                "AeroCloud safety gate (incremental): {} pending deletes vs {} baselined (local folder empty: {}). Disabling delete propagation this pass to prevent accidental wipe.",
                pending_deletes,
                prior_count,
                local_empty
            );
            for c in &mut comparisons {
                if !(config.update_only && config.sync_direction != CompareDirection::Bidirectional)
                    && (c.status == SyncStatus::LocalOnly || c.status == SyncStatus::RemoteOnly)
                {
                    c.previously_synced = false;
                }
            }
        }

        let total = comparisons.len();
        let mut done = Vec::with_capacity(total);
        let mut ahead = super::bulk::BulkAhead::new(update_policy);
        for (i, comparison) in comparisons.iter().enumerate() {
            self.set_status(CloudSyncStatus::Syncing {
                current_file: comparison.relative_path.clone(),
                progress: (i as f64 / total.max(1) as f64) * 100.0,
                files_done: i as u32,
                files_total: total as u32,
            })
            .await;
            match self
                .next_outcome(provider, &config, &comparisons, i, &mut ahead)
                .await
            {
                Ok(SyncAction::AskUser) => {
                    result.conflicts += 1;
                    self.push_conflict(comparison).await;
                    done.push(comparison.clone());
                }
                Ok(action) => {
                    Self::record_sync_action(&mut result, &config, comparison, &action);
                    done.push(comparison.clone());
                }
                Err(e) => {
                    result
                        .errors
                        .push(format!("{}: {}", comparison.relative_path, e));
                    failed.push(FailedPath {
                        key: comparison.relative_path.clone(),
                        error: e.clone(),
                    });
                    if self.notify_reauth_if_token_revoked(&config.protocol_type, &e) {
                        result
                            .errors
                            .push("Sync aborted: re-authorization required".to_string());
                        break;
                    }
                }
            }
        }

        let mut files = self.post_sync_baseline(&done, &config, Some(&index), update_policy);
        for key in gone_on_both {
            files.remove(&key);
        }
        let remote_spellings =
            crate::sync::spellings_after_baseline(&done, Some(&index.remote_spellings), &files);
        let saved = SyncIndex {
            profile_id: config.server_profile_id.clone(),
            version: SYNC_INDEX_VERSION,
            last_sync: Utc::now(),
            local_path: local_str,
            remote_path: config.remote_folder.clone(),
            files,
            remote_spellings,
            unverified_keys: Default::default(),
        };

        result.duration_secs = start.elapsed().as_secs();
        for path in self.take_unsettled() {
            result.errors.push(format!(
                "{path}: it kept changing while it uploaded; the next cycle reads it as a conflict, asked about under PreferNewer or PreferRemote until one cycle runs under PreferLocal (sends the local copy) or KeepBoth"
            ));
        }
        if result.conflicts > 0 {
            self.set_status(CloudSyncStatus::HasConflicts {
                count: result.conflicts,
            })
            .await;
        } else if !result.errors.is_empty() {
            self.set_status(CloudSyncStatus::Error {
                message: format!("{} errors during sync", result.errors.len()),
            })
            .await;
        } else {
            // The status says when files last synced. The config's
            // `last_sync` is not moved: it dates the last full scan, and
            // the countdown to the next one is read from it.
            self.set_status(CloudSyncStatus::Idle {
                last_sync: Some(Utc::now()),
                next_sync: None,
            })
            .await;
        }
        if let Some(app) = &self.app_handle {
            let _ = app.emit("cloud_sync_complete", &result);
        }
        Ok((IncrementalRun::Done(result, failed), saved))
    }

    /// Replay one local rename on the remote. Refuses, leaving the remote as
    /// it was, when the new name is taken there or the old copy is gone or
    /// of another kind: then the paths are synced one by one.
    async fn rename_on_remote<P: StorageProvider + ?Sized>(
        &self,
        provider: &mut P,
        remote_root: &str,
        rename: &PlannedRename,
    ) -> Result<(), String> {
        let from = format!("{remote_root}/{}", rename.from);
        let to = format!("{remote_root}/{}", rename.to);
        if remote_state(provider, &to).await?.is_some() {
            return Err("the new name is taken on the remote".to_string());
        }
        // A bridge can acknowledge the previous upload before a fresh
        // connection can stat it. Do not turn that window into a fallback
        // delete/upload. Folders are virtual prefixes, not readable objects.
        if !rename.is_dir {
            provider
                .prepare_file_move(&from)
                .await
                .map_err(|e| e.to_string())?;
        }
        match remote_state(provider, &from).await? {
            Some(entry) if entry.is_dir == rename.is_dir => {}
            Some(_) => return Err("the remote copy is of another kind".to_string()),
            None => return Err("the remote copy is gone".to_string()),
        }
        if let Some((parent, _)) = rename.to.rsplit_once('/') {
            let parent_path = format!("{remote_root}/{parent}");
            if provider.cd(&parent_path).await.is_err() {
                crate::sync::ensure_remote_dir(provider, &parent_path).await;
            }
        }
        provider.rename(&from, &to).await.map_err(|e| e.to_string())
    }
}

/// An incremental pass for one pair, for the background worker: connect,
/// run `plan`, disconnect. When the remote refuses renames and their paths
/// come to more than the pass takes, and `full_scan_allowed`, the full scan
/// runs on the same connection.
pub async fn run_pair_incremental_cycle(
    config: CloudConfig,
    store: Option<&credential_store::CredentialStore>,
    app: Option<AppHandle>,
    plan: Box<IncrementalPlan>,
    full_scan_allowed: bool,
) -> Result<PairCycleReport, PairCycleError> {
    let mut provider = open_aerocloud_provider(&config, store)
        .await
        .map_err(PairCycleError::before_connecting)?;
    let traits = ProviderScanTraits::of(&*provider);
    let mut svc = CloudService::new();
    if let Some(handle) = app {
        svc.set_app_handle(handle);
    }
    svc.init(config).await;
    let work_limit = full_scan_allowed.then_some(REMOTE_WORK_LIMIT);
    let outcome = svc
        .perform_incremental_sync_with_provider(&mut *provider, plan, work_limit)
        .await;
    let report = match outcome {
        Ok(IncrementalRun::Done(result, failed_paths)) => Ok(PairCycleReport {
            result,
            ran_full_scan: false,
            traits: Some(traits),
            failed_paths,
        }),
        Ok(IncrementalRun::TooLarge(partial)) => svc
            .perform_full_sync_with_provider(&mut *provider)
            .await
            .map(|mut full| {
                full.errors.splice(0..0, partial.errors);
                full.duration_secs += partial.duration_secs;
                PairCycleReport {
                    result: full,
                    ran_full_scan: true,
                    traits: Some(traits),
                    failed_paths: Vec::new(),
                }
            }),
        Err(e) => Err(e),
    };
    let _ = provider.disconnect().await;
    report.map_err(|message| PairCycleError {
        message,
        traits: Some(traits),
    })
}

#[cfg(test)]
#[path = "incremental_tests.rs"]
mod tests;
