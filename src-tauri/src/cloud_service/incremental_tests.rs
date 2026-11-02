// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Tests of the incremental pass: a remote in memory that records every
//! call, and the local side in a temporary folder. The baseline is passed in
//! and read back, never saved, so the tests run on every platform without
//! touching the data folder.

use super::*;
use crate::cloud_config::ConflictStrategy;
use crate::compress_overlay_provider::CompressOverlayProvider;
use crate::crypt_overlay_provider::{CryptOverlayProvider, OverlayKeys};
use crate::providers::ProviderType;
use crate::rclone_crypt::{FilenameEncryption, RcloneCryptKeys};
use crate::sync::CompareDirection;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The time every "previously synced" file has on both sides.
const SYNCED_AT: i64 = 1_700_000_000;

#[derive(Default)]
struct RemoteState {
    /// Path -> (bytes, mtime in Unix seconds).
    files: BTreeMap<String, (Vec<u8>, i64)>,
    cwd: String,
    dirs: BTreeSet<String>,
    /// The server's clock: each upload is stamped with the next tick.
    clock: i64,
    refuse_rename: bool,
    refuse_upload: bool,
    refuse_download: bool,
    /// Written, once, by a download that is about to be refused: an edit that
    /// lands while it runs.
    edit_during_download: Option<PathBuf>,
    unpublished: BTreeSet<String>,
    readiness_calls: Vec<String>,
    readiness_fails: bool,
    /// Paths whose `stat` and `exists` fail with a connection error.
    unreadable: BTreeSet<String>,
    floor: Option<Duration>,
    stats: Vec<String>,
    on_stat: Option<Box<dyn FnOnce() + Send>>,
    lists: usize,
    uploads: Vec<String>,
    downloads: Vec<String>,
    deletes: Vec<String>,
    renames: Vec<(String, String)>,
    /// A native bulk, as Proton Drive's in CLI mode, and its calls.
    bulk: bool,
    non_authoritative: bool,
    bulks: Vec<Vec<String>>,
}

/// A remote in memory, shared so a test still reads it once an overlay owns
/// the provider.
#[derive(Clone)]
struct MemRemote(Arc<Mutex<RemoteState>>);

impl MemRemote {
    fn new() -> Self {
        let state = RemoteState {
            dirs: BTreeSet::from(["/remote".to_string()]),
            clock: SYNCED_AT + 1_000,
            cwd: "/remote".into(),
            ..RemoteState::default()
        };
        Self(Arc::new(Mutex::new(state)))
    }

    fn state(&self) -> std::sync::MutexGuard<'_, RemoteState> {
        self.0.lock().unwrap()
    }

    fn put(&self, path: &str, bytes: &[u8], mtime: i64) {
        self.state()
            .files
            .insert(path.to_string(), (bytes.to_vec(), mtime));
    }

    fn calls_to(&self, path: &str) -> usize {
        self.state().stats.iter().filter(|p| *p == path).count()
    }
}

fn parent_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(parent, _)| parent)
}

fn not_found(path: &str) -> ProviderError {
    ProviderError::NotFound(path.to_string())
}

#[async_trait::async_trait]
impl StorageProvider for MemRemote {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
    fn provider_type(&self) -> ProviderType {
        ProviderType::Sftp
    }
    fn display_name(&self) -> String {
        "memory".to_string()
    }
    fn listing_is_authoritative(&self) -> bool {
        !self.state().non_authoritative
    }

    fn min_full_scan_interval(&self) -> Option<Duration> {
        self.state().floor
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
    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let mut state = self.state();
        state.lists += 1;
        let path = if path == "." {
            state.cwd.as_str()
        } else {
            path
        };
        let entries = state
            .files
            .iter()
            .filter(|(p, _)| parent_of(p) == path)
            .map(|(p, (bytes, mtime))| {
                let mut entry = RemoteEntry::file(
                    p.rsplit('/').next().unwrap().to_string(),
                    p.clone(),
                    bytes.len() as u64,
                );
                entry.modified = DateTime::<Utc>::from_timestamp(*mtime, 0)
                    .map(|time| time.format("%Y-%m-%d %H:%M:%S").to_string());
                entry
            })
            .collect();
        Ok(entries)
    }
    async fn pwd(&mut self) -> Result<String, ProviderError> {
        Ok(self.state().cwd.clone())
    }
    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        let mut state = self.state();
        if state.dirs.contains(path) {
            state.cwd = path.to_string();
            Ok(())
        } else {
            Err(not_found(path))
        }
    }
    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        Ok(())
    }
    async fn download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        _progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let mut state = self.state();
        state.downloads.push(remote_path.to_string());
        if state.refuse_download {
            if let Some(path) = state.edit_during_download.take() {
                std::fs::write(path, b"edited meanwhile").map_err(ProviderError::IoError)?;
            }
            return Err(ProviderError::Other("fixture download refused".into()));
        }
        let (bytes, _) = state
            .files
            .get(remote_path)
            .ok_or_else(|| not_found(remote_path))?;
        std::fs::write(local_path, bytes).map_err(ProviderError::IoError)
    }
    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        self.state()
            .files
            .get(remote_path)
            .map(|(bytes, _)| bytes.clone())
            .ok_or_else(|| not_found(remote_path))
    }
    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        _progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let bytes = std::fs::read(local_path).map_err(ProviderError::IoError)?;
        let mut state = self.state();
        if state.refuse_upload {
            return Err(ProviderError::Other("fixture upload refused".into()));
        }
        if !state.dirs.contains(parent_of(remote_path)) {
            return Err(not_found(parent_of(remote_path)));
        }
        state.clock += 10;
        let at = state.clock;
        state.uploads.push(remote_path.to_string());
        state.files.insert(remote_path.to_string(), (bytes, at));
        Ok(())
    }
    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        self.state()
            .dirs
            .insert(path.trim_end_matches('/').to_string());
        Ok(())
    }
    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        let mut state = self.state();
        state.deletes.push(path.to_string());
        state
            .files
            .remove(path)
            .map(|_| ())
            .ok_or_else(|| not_found(path))
    }
    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        self.state().dirs.remove(path);
        Ok(())
    }
    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        self.rmdir(path).await
    }
    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let mut state = self.state();
        if state.refuse_rename {
            return Err(ProviderError::NotSupported("rename".to_string()));
        }
        if !state.dirs.contains(parent_of(to)) {
            return Err(not_found(parent_of(to)));
        }
        state.renames.push((from.to_string(), to.to_string()));
        let moved = |path: &str| {
            (path == from || path.starts_with(&format!("{from}/")))
                .then(|| format!("{to}{}", &path[from.len()..]))
        };
        let files: Vec<(String, String)> = state
            .files
            .keys()
            .filter_map(|p| moved(p).map(|new| (p.clone(), new)))
            .collect();
        let dirs: Vec<(String, String)> = state
            .dirs
            .iter()
            .filter_map(|p| moved(p).map(|new| (p.clone(), new)))
            .collect();
        if files.is_empty() && dirs.is_empty() {
            return Err(not_found(from));
        }
        for (old, new) in files {
            let entry = state.files.remove(&old).unwrap();
            state.files.insert(new, entry);
        }
        for (old, new) in dirs {
            state.dirs.remove(&old);
            state.dirs.insert(new);
        }
        Ok(())
    }
    async fn prepare_file_move(&mut self, path: &str) -> Result<(), ProviderError> {
        let mut state = self.state();
        state.readiness_calls.push(path.to_string());
        if state.readiness_fails {
            return Err(ProviderError::TransferFailed(
                "bridge has not published the source".into(),
            ));
        }
        state.unpublished.remove(path);
        Ok(())
    }
    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        let mut state = self.state();
        state.stats.push(path.to_string());
        if let Some(hook) = state.on_stat.take() {
            hook();
        }
        if state.unpublished.contains(path) {
            return Err(not_found(path));
        }
        if state.unreadable.contains(path) {
            return Err(ProviderError::ConnectionFailed("reset by peer".to_string()));
        }
        let name = path.rsplit('/').next().unwrap_or(path).to_string();
        if let Some((bytes, mtime)) = state.files.get(path) {
            let mut entry = RemoteEntry::file(name, path.to_string(), bytes.len() as u64);
            entry.modified = DateTime::<Utc>::from_timestamp(*mtime, 0)
                .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string());
            Ok(entry)
        } else if state.dirs.contains(path) {
            Ok(RemoteEntry::directory(name, path.to_string()))
        } else {
            Err(not_found(path))
        }
    }
    async fn size(&mut self, path: &str) -> Result<u64, ProviderError> {
        self.state()
            .files
            .get(path)
            .map(|(bytes, _)| bytes.len() as u64)
            .ok_or_else(|| not_found(path))
    }
    async fn exists(&mut self, path: &str) -> Result<bool, ProviderError> {
        let state = self.state();
        if state.unreadable.contains(path) {
            return Err(ProviderError::ConnectionFailed("reset by peer".to_string()));
        }
        Ok(state.files.contains_key(path) || state.dirs.contains(path))
    }
    async fn keep_alive(&mut self) -> Result<(), ProviderError> {
        Ok(())
    }
    async fn server_info(&mut self) -> Result<String, ProviderError> {
        Ok("memory".to_string())
    }
    fn supports_bulk_upload(&self) -> bool {
        self.state().bulk
    }
    async fn upload_bulk(
        &mut self,
        items: &[crate::providers::bulk::BulkTransferItem],
        _progress: Option<crate::providers::bulk::BulkProgress>,
    ) -> Result<Vec<crate::providers::bulk::BulkTransferOutcome>, ProviderError> {
        let mut state = self.state();
        state
            .bulks
            .push(items.iter().map(|i| i.remote_path.clone()).collect());
        Ok(items
            .iter()
            .map(|item| {
                let bytes = std::fs::read(&item.local_path).unwrap();
                state.clock += 10;
                let at = state.clock;
                state.files.insert(item.remote_path.clone(), (bytes, at));
                crate::providers::bulk::BulkTransferOutcome::Completed
            })
            .collect())
    }
}

fn pair(root: &Path) -> CloudConfig {
    CloudConfig {
        enabled: true,
        local_folder: root.to_path_buf(),
        remote_folder: "/remote".to_string(),
        sync_direction: CompareDirection::Bidirectional,
        exclude_patterns: Vec::new(),
        ..CloudConfig::default()
    }
}

fn empty_index(config: &CloudConfig) -> SyncIndex {
    SyncIndex::new(
        config.local_folder.to_string_lossy().into_owned(),
        config.remote_folder.clone(),
    )
}

fn set_mtime(path: &Path, secs: i64) {
    filetime::set_file_mtime(path, filetime::FileTime::from_unix_time(secs, 0)).unwrap();
}

/// A file a previous full sync left identical on both sides.
fn synced(index: &mut SyncIndex, remote: &MemRemote, root: &Path, key: &str, bytes: &[u8]) {
    let path = root.join(key);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, bytes).unwrap();
    set_mtime(&path, SYNCED_AT);
    let mut parent = String::from("/remote");
    for dir in key
        .split('/')
        .rev()
        .skip(1)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
    {
        parent = format!("{parent}/{dir}");
        remote.state().dirs.insert(parent.clone());
    }
    remote.put(&format!("/remote/{key}"), bytes, SYNCED_AT);
    index.files.insert(
        key.to_string(),
        SyncIndexEntry {
            size: bytes.len() as u64,
            modified: DateTime::<Utc>::from_timestamp(SYNCED_AT, 0),
            is_dir: false,
            uploaded_at: None,
        },
    );
    // This fixture models a real prior Cloud send, not arbitrary comparator bookkeeping.
    index.cloud_receipts.insert(
        key.to_string(),
        crate::sync::CloudTransferReceipt {
            scope: CloudService::receipt_scope(&pair(root)),
            remote_key: key.to_string(),
            destination_side: CompareDirection::LocalToRemote,
            destination: index.files[key].clone(),
        },
    );
}

fn changes(root: &Path, paths: &[&str], renames: &[(&str, &str)]) -> LocalChanges {
    LocalChanges {
        paths: paths.iter().map(|p| root.join(p)).collect(),
        renames: renames
            .iter()
            .map(|(from, to)| (root.join(from), root.join(to)))
            .collect(),
    }
}

const EXACT: IncrementalHints = IncrementalHints { compare_size: true };

fn prep(config: &CloudConfig, index: &SyncIndex, paths: &[&str]) -> IncrementalPrep {
    plan(
        config,
        Some(index.clone()),
        &changes(&config.local_folder, paths, &[]),
        EXACT,
    )
    .unwrap()
}

fn work(prep: IncrementalPrep) -> Box<IncrementalPlan> {
    match prep {
        IncrementalPrep::Work { plan, .. } => plan,
        IncrementalPrep::Settled { .. } => panic!("expected work, every path settled"),
        IncrementalPrep::FullScanOnly(reason) => panic!("expected work, full scan only: {reason}"),
    }
}

fn settled(prep: &IncrementalPrep) -> bool {
    matches!(
        prep,
        IncrementalPrep::Settled {
            full_scan_wanted: None
        }
    )
}

async fn execute<P: StorageProvider + ?Sized>(
    config: &CloudConfig,
    plan: Box<IncrementalPlan>,
    provider: &mut P,
    work_limit: Option<usize>,
) -> (IncrementalRun, SyncIndex) {
    let svc = CloudService::new();
    svc.init(config.clone()).await;
    svc.execute_incremental(provider, plan, work_limit)
        .await
        .expect("the pass runs")
}

fn done(run: IncrementalRun) -> (SyncOperationResult, Vec<FailedPath>) {
    match run {
        IncrementalRun::Done(result, failed) => (result, failed),
        IncrementalRun::TooLarge(_) => panic!("expected the pass to finish"),
    }
}

/// Failed watcher writes are excluded from `done`, but must retire cached clocks.
#[tokio::test]
async fn failed_watcher_mutations_retire_only_the_touched_metadata_observation() {
    for upload in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let mut config = pair(root.path());
        config.update_only = true;
        config.sync_direction = if upload {
            CompareDirection::LocalToRemote
        } else {
            CompareDirection::RemoteToLocal
        };
        let mut remote = MemRemote::new();
        let mut index = empty_index(&config);
        for key in ["changed", "untouched"] {
            std::fs::write(root.path().join(key), b"old").unwrap();
            set_mtime(&root.path().join(key), SYNCED_AT);
            remote.put(&format!("/remote/{key}"), b"old", SYNCED_AT);
            index.files.insert(
                key.into(),
                SyncIndexEntry {
                    size: 3,
                    modified: DateTime::<Utc>::from_timestamp(SYNCED_AT, 0),
                    is_dir: false,
                    uploaded_at: None,
                },
            );
            let file = FileInfo {
                name: key.into(),
                path: format!("/remote/{key}"),
                size: 3,
                modified: DateTime::<Utc>::from_timestamp(SYNCED_AT, 0),
                is_dir: false,
                checksum: None,
                checksum_alg: None,
            };
            index.cloud_remote_metadata.insert(
                key.into(),
                crate::sync::CloudRemoteObservation {
                    scope: "fixture-scope".into(),
                    listed: file.clone(),
                    logical: file,
                    local_key: key.into(),
                    local: index.files[key].clone(),
                },
            );
        }
        std::fs::write(root.path().join("changed"), b"new local").unwrap();
        set_mtime(&root.path().join("changed"), SYNCED_AT + 10);
        if upload {
            remote.state().refuse_upload = true;
        } else {
            remote.put("/remote/changed", b"new remote", SYNCED_AT + 100);
            remote.state().refuse_download = true;
        }
        let (run, saved) = execute(
            &config,
            work(prep(&config, &index, &["changed"])),
            &mut remote,
            None,
        )
        .await;
        let (result, failed) = done(run);
        assert!(!result.errors.is_empty());
        assert_eq!(failed.len(), 1);
        assert!(!saved.cloud_remote_metadata.contains_key("changed"));
        assert!(saved.cloud_remote_metadata.contains_key("untouched"));
    }
}

/// A probe failure after stat must finish the pass and report its normal error state.
#[tokio::test]
async fn a_failed_update_source_probe_disables_deletes_and_completes_the_pass() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("source");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("new"), b"source").unwrap();
    let mut config = pair(&root);
    config.update_only = true;
    config.sync_direction = CompareDirection::LocalToRemote;
    let prepared = work(prep(&config, &empty_index(&config), &["new"]));
    let mut remote = MemRemote::new();
    let source = root.clone();
    let retired = fixture.path().join("retired");
    remote.state().on_stat = Some(Box::new(move || {
        std::fs::rename(&source, retired).unwrap();
        std::fs::write(source, b"not a directory").unwrap();
    }));
    let svc = CloudService::new();
    svc.init(config.clone()).await;
    let (run, _) = svc
        .execute_incremental(&mut remote, prepared, None)
        .await
        .expect("probe error is an operation result, not an aborted pass");
    let (result, _) = done(run);
    assert!(result
        .errors
        .iter()
        .any(|error| error.contains("cannot inspect local source")));
    assert_eq!(result.deleted, 0);
    assert!(remote.state().deletes.is_empty());
    assert!(matches!(
        *svc.status.read().await,
        CloudSyncStatus::Error { .. }
    ));
}

/// The class defect: a watcher event used to run a full sync that listed the
/// whole remote folder. A new file now costs one `stat` before the upload
/// (and the one after it that every upload makes), and no listing; the
/// event its own time stamp raises then settles on the local side.
#[tokio::test]
async fn a_new_file_is_sent_after_one_stat_and_no_listing() {
    let root = tempfile::tempdir().unwrap();
    let config = pair(root.path());
    let mut remote = MemRemote::new();
    std::fs::write(root.path().join("new.txt"), b"hello").unwrap();
    let index = empty_index(&config);

    let plan = work(prep(&config, &index, &["new.txt"]));
    assert_eq!(plan.remote_work(), 1);
    let (run, saved) = execute(&config, plan, &mut remote, None).await;
    let (result, failed) = done(run);

    assert_eq!(result.uploaded, 1);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(failed.is_empty());
    assert_eq!(remote.state().lists, 0, "no listing");
    assert_eq!(remote.state().files["/remote/new.txt"].0, b"hello");
    assert_eq!(remote.calls_to("/remote/new.txt"), 2);
    assert!(
        settled(&prep(&config, &saved, &["new.txt"])),
        "the event of the upload's own time stamp asks the remote nothing"
    );
}

/// The baseline a pass leaves records the profile it synced with, as the full
/// sync's does: one that recorded none would serve any profile, so another
/// server's sync of the same folders would read it as its own and turn files
/// only that server holds into deletions.
#[tokio::test]
async fn the_baseline_of_a_pass_records_its_profile() {
    let root = tempfile::tempdir().unwrap();
    let mut config = pair(root.path());
    config.server_profile_id = Some("srv_p1".to_string());
    let mut remote = MemRemote::new();
    std::fs::write(root.path().join("new.txt"), b"hello").unwrap();
    let index = empty_index(&config);

    let plan = work(prep(&config, &index, &["new.txt"]));
    let (run, saved) = execute(&config, plan, &mut remote, None).await;
    assert_eq!(done(run).0.uploaded, 1);
    assert_eq!(saved.profile_id.as_deref(), Some("srv_p1"));
}

/// On a provider with a native bulk (Proton Drive in CLI mode starts one
/// program per call) the new files of a pass go up in one call, and one
/// listing of their folder replaces the `stat` after each upload.
#[tokio::test]
async fn a_pass_sends_its_new_files_in_one_bulk() {
    let root = tempfile::tempdir().unwrap();
    let config = pair(root.path());
    let mut remote = MemRemote::new();
    remote.state().bulk = true;
    let names = ["a.txt", "b.txt", "c.txt"];
    for name in names {
        std::fs::write(root.path().join(name), name).unwrap();
    }
    let index = empty_index(&config);

    let plan = work(prep(&config, &index, &names));
    let (run, saved) = execute(&config, plan, &mut remote, None).await;
    let (result, failed) = done(run);
    assert_eq!(result.uploaded, 3);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(failed.is_empty());
    for name in names {
        assert_eq!(
            remote.calls_to(&format!("/remote/{name}")),
            1,
            "only the stat before"
        );
        assert!(saved.files.contains_key(name));
    }
    let state = remote.state();
    assert_eq!(
        state.bulks,
        [vec!["/remote/a.txt", "/remote/b.txt", "/remote/c.txt"]]
    );
    assert!(state.uploads.is_empty(), "no single-file upload");
    assert_eq!(state.lists, 1, "one listing instead of a stat per file");
}

/// A file the watcher reports but that matches its baseline (written by the
/// last cycle, touched without a change) needs nothing from the remote.
#[tokio::test]
async fn an_unchanged_file_settles_without_the_remote() {
    let root = tempfile::tempdir().unwrap();
    let config = pair(root.path());
    let remote = MemRemote::new();
    let mut index = empty_index(&config);
    synced(&mut index, &remote, root.path(), "doc.txt", b"same");
    assert!(settled(&prep(&config, &index, &["doc.txt"])));
    // Nor does a path that never existed on either side (a temporary file).
    assert!(settled(&prep(&config, &index, &["gone.tmp"])));
}

#[tokio::test]
async fn a_modified_file_is_sent_over_its_unchanged_remote_copy() {
    let root = tempfile::tempdir().unwrap();
    let config = pair(root.path());
    let mut remote = MemRemote::new();
    let mut index = empty_index(&config);
    synced(&mut index, &remote, root.path(), "doc.txt", b"v1");
    std::fs::write(root.path().join("doc.txt"), b"v2 longer").unwrap();

    let (run, saved) = execute(
        &config,
        work(prep(&config, &index, &["doc.txt"])),
        &mut remote,
        None,
    )
    .await;
    let (result, _) = done(run);
    assert_eq!(result.uploaded, 1);
    assert_eq!(remote.state().files["/remote/doc.txt"].0, b"v2 longer");
    assert_eq!(saved.files["doc.txt"].size, 9);
}

/// The watcher key is the local spelling. The bytes live at the remote
/// spelling. Statting the local spelling misses, and a bidirectional pass
/// then treats the edit as a remote delete and skips it.
async fn edited_spelling_uploads_the_remote_object(
    root: &std::path::Path,
    index: &SyncIndex,
    remote: &mut MemRemote,
    local_key: &str,
    remote_key: &str,
) {
    let config = pair(root);
    let path = root.join(local_key);
    std::fs::write(&path, b"v2 longer").unwrap();
    let plan = work(prep(&config, index, &[local_key]));
    let (run, saved) = execute(&config, plan, remote, None).await;
    let (result, failed) = done(run);
    assert_eq!(result.uploaded, 1, "{:?}", result.errors);
    assert_eq!(result.deleted, 0);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(failed.is_empty());
    assert_eq!(
        remote.state().files[&format!("/remote/{remote_key}")].0,
        b"v2 longer"
    );
    assert!(
        !remote
            .state()
            .files
            .contains_key(&format!("/remote/{local_key}")),
        "the pass must not create a second object at the local spelling"
    );
    assert_eq!(remote.calls_to(&format!("/remote/{local_key}")), 0);
    assert_eq!(saved.files.get(local_key).map(|entry| entry.size), Some(9));
    assert!(!saved.files.contains_key(remote_key));
    assert_eq!(
        saved.remote_spellings.get(local_key).map(String::as_str),
        Some(remote_key)
    );
}

#[tokio::test]
async fn an_edited_local_spelling_updates_the_remote_object() {
    let root = tempfile::tempdir().unwrap();
    let config = pair(root.path());
    let mut remote = MemRemote::new();
    let mut index = empty_index(&config);
    let local_key = "cafe\u{301}.txt";
    let remote_key = "café.txt";
    index.files.insert(
        local_key.to_string(),
        SyncIndexEntry {
            size: 2,
            modified: chrono::DateTime::<chrono::Utc>::from_timestamp(SYNCED_AT, 0),
            is_dir: false,
            uploaded_at: None,
        },
    );
    index
        .remote_spellings
        .insert(local_key.to_string(), remote_key.to_string());
    remote.put(&format!("/remote/{remote_key}"), b"v1", SYNCED_AT);
    edited_spelling_uploads_the_remote_object(
        root.path(),
        &index,
        &mut remote,
        local_key,
        remote_key,
    )
    .await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn shark_review_new_unicode_twin_never_overwrites_its_existing_sibling() {
    let root = tempfile::tempdir().unwrap();
    let config = pair(root.path());
    let mut remote = MemRemote::new();
    let mut index = empty_index(&config);
    let nfc = "café.txt";
    let nfd = "cafe\u{301}.txt";
    synced(&mut index, &remote, root.path(), nfc, b"original");
    std::fs::write(root.path().join(nfd), b"new independent twin bytes").unwrap();
    let (run, _) = execute(
        &config,
        work(prep(&config, &index, &[nfd])),
        &mut remote,
        None,
    )
    .await;
    let (result, _) = done(run);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_eq!(
        remote.state().files[&format!("/remote/{nfc}")].0,
        b"original",
        "a newly created sibling is not a legacy alias"
    );
}

#[tokio::test]
async fn a_legacy_remote_key_still_points_the_watcher_at_the_remote_object() {
    let root = tempfile::tempdir().unwrap();
    let config = pair(root.path());
    let mut remote = MemRemote::new();
    let mut index = empty_index(&config);
    let local_key = "cafe\u{301}.txt";
    let remote_key = "café.txt";
    index.files.insert(
        remote_key.to_string(),
        SyncIndexEntry {
            size: 2,
            modified: chrono::DateTime::<chrono::Utc>::from_timestamp(SYNCED_AT, 0),
            is_dir: false,
            uploaded_at: None,
        },
    );
    remote.put(&format!("/remote/{remote_key}"), b"v1", SYNCED_AT);
    edited_spelling_uploads_the_remote_object(
        root.path(),
        &index,
        &mut remote,
        local_key,
        remote_key,
    )
    .await;
}

/// A new file under a parent whose remote spelling differs must be created
/// in that remote directory. Statting and uploading the local spelling
/// would leave a second directory beside the one the baseline already maps.
#[tokio::test]
async fn a_new_local_child_is_uploaded_under_the_remote_parent_spelling() {
    let root = tempfile::tempdir().unwrap();
    let config = pair(root.path());
    let mut remote = MemRemote::new();
    let mut index = empty_index(&config);
    let parent = "cafe\u{301}";
    let remote_parent = "café";
    index.files.insert(
        parent.to_string(),
        SyncIndexEntry {
            size: 0,
            modified: None,
            is_dir: true,
            uploaded_at: None,
        },
    );
    index
        .remote_spellings
        .insert(parent.to_string(), remote_parent.to_string());
    remote
        .state()
        .dirs
        .insert(format!("/remote/{remote_parent}"));
    let local_key = format!("{parent}/new.txt");
    std::fs::create_dir_all(root.path().join(parent)).unwrap();
    std::fs::write(root.path().join(&local_key), b"hello").unwrap();

    let (run, saved) = execute(
        &config,
        work(prep(&config, &index, &[&local_key])),
        &mut remote,
        None,
    )
    .await;
    let (result, failed) = done(run);
    assert_eq!(result.uploaded, 1, "{:?}", result.errors);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(failed.is_empty());
    assert_eq!(
        remote.state().files[&format!("/remote/{remote_parent}/new.txt")].0,
        b"hello"
    );
    assert!(
        !remote.state().dirs.contains(&format!("/remote/{parent}")),
        "the pass must not create the local spelling of the parent"
    );
    assert_eq!(
        saved.remote_spellings.get(&local_key).map(String::as_str),
        Some("café/new.txt")
    );
    assert!(saved.files.contains_key(&local_key));
}

/// An exact tracked parent is evidence even when a canonical sibling exists.
#[tokio::test]
async fn an_exact_parent_spelling_wins_among_tracked_twins() {
    let root = tempfile::tempdir().unwrap();
    let config = pair(root.path());
    let mut remote = MemRemote::new();
    let mut index = empty_index(&config);
    for key in ["café", "cafe\u{301}"] {
        index.files.insert(
            key.to_string(),
            SyncIndexEntry {
                size: 0,
                modified: None,
                is_dir: true,
                uploaded_at: None,
            },
        );
        remote.state().dirs.insert(format!("/remote/{key}"));
    }
    let local_key = "cafe\u{301}/new.txt";
    std::fs::create_dir_all(root.path().join("cafe\u{301}")).unwrap();
    std::fs::write(root.path().join(local_key), b"hello").unwrap();

    let (run, saved) = execute(
        &config,
        work(prep(&config, &index, &[local_key])),
        &mut remote,
        None,
    )
    .await;
    let (result, _) = done(run);
    assert_eq!(result.uploaded, 1, "{:?}", result.errors);
    assert_eq!(result.skipped, 0);
    assert_eq!(
        remote.state().files[&format!("/remote/{local_key}")].0,
        b"hello"
    );
    assert!(!remote.state().files.contains_key("/remote/café/new.txt"));
    assert!(saved.files.contains_key(local_key));
}

/// The `stat` is the conflict check: a remote copy that changed since the
/// baseline too is a conflict, asked about, never overwritten.
#[tokio::test]
async fn a_file_changed_on_both_sides_is_a_conflict_not_an_upload() {
    let root = tempfile::tempdir().unwrap();
    let config = pair(root.path());
    let mut remote = MemRemote::new();
    let mut index = empty_index(&config);
    synced(&mut index, &remote, root.path(), "doc.txt", b"v1");
    std::fs::write(root.path().join("doc.txt"), b"local edit").unwrap();
    remote.put("/remote/doc.txt", b"remote edit!", SYNCED_AT + 500);

    let (run, saved) = execute(
        &config,
        work(prep(&config, &index, &["doc.txt"])),
        &mut remote,
        None,
    )
    .await;
    let (result, _) = done(run);
    assert_eq!(result.conflicts, 1);
    assert_eq!(result.uploaded, 0);
    assert!(remote.state().uploads.is_empty());
    assert_eq!(remote.state().files["/remote/doc.txt"].0, b"remote edit!");
    assert_eq!(saved.files["doc.txt"].size, 2, "the baseline stays");
}

#[tokio::test]
async fn one_way_update_incremental_bulk_keeps_newer_files_and_propagates_deletes() {
    for bulk in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let config = CloudConfig {
            sync_direction: CompareDirection::LocalToRemote,
            update_only: true,
            preserve_remote_deletes: false,
            conflict_strategy: ConflictStrategy::PreferRemote,
            ..pair(root.path())
        };
        let mut remote = MemRemote::new();
        remote.state().bulk = bulk;
        let mut index = empty_index(&config);
        for name in ["keep", "older", "older2", "gone"] {
            synced(&mut index, &remote, root.path(), name, b"original");
        }
        std::fs::write(root.path().join("keep"), b"local edit").unwrap();
        set_mtime(&root.path().join("keep"), SYNCED_AT + 10);
        remote.put("/remote/keep", b"newer remote edit", SYNCED_AT + 100);
        std::fs::write(root.path().join("older"), b"source update").unwrap();
        set_mtime(&root.path().join("older"), SYNCED_AT + 100);
        std::fs::write(root.path().join("older2"), b"second source update").unwrap();
        set_mtime(&root.path().join("older2"), SYNCED_AT + 100);
        std::fs::remove_file(root.path().join("gone")).unwrap();
        let (run, _) = execute(
            &config,
            work(prep(&config, &index, &["keep", "older", "older2", "gone"])),
            &mut remote,
            None,
        )
        .await;
        let (result, _) = done(run);
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert_eq!(
            (result.uploaded, result.downloaded, result.deleted),
            (2, 0, 1)
        );
        assert_eq!(remote.state().files["/remote/keep"].0, b"newer remote edit");
        assert_eq!(
            std::fs::read(root.path().join("keep")).unwrap(),
            b"local edit"
        );
        assert_eq!(remote.state().files["/remote/older"].0, b"source update");
        assert!(!remote.state().files.contains_key("/remote/gone"));
        assert_eq!(
            remote.state().files["/remote/older2"].0,
            b"second source update"
        );
        assert_eq!(!remote.state().bulks.is_empty(), bulk);
        if bulk {
            assert!(remote.state().bulks.iter().any(|group| group.len() == 2));
        }
    }
}

#[tokio::test]
async fn one_way_update_watcher_refuses_a_root_removed_after_prepare() {
    for with_baseline in [false, true] {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("source");
        std::fs::create_dir(&root).unwrap();
        let config = CloudConfig {
            sync_direction: CompareDirection::LocalToRemote,
            update_only: true,
            preserve_remote_deletes: false,
            ..pair(&root)
        };
        let mut remote = MemRemote::new();
        let mut index = empty_index(&config);
        if with_baseline {
            synced(&mut index, &remote, &root, "gone", b"original");
            std::fs::remove_file(root.join("gone")).unwrap();
        }
        remote.put("/remote/untracked", b"never synced", SYNCED_AT);
        std::fs::write(root.join("new"), b"source").unwrap();
        let prepared = work(prep(&config, &index, &["new", "gone", "untracked"]));
        std::fs::rename(&root, parent.path().join("renamed")).unwrap();
        let svc = CloudService::new();
        svc.init(config).await;
        let outcome = svc.execute_incremental(&mut remote, prepared, None).await;
        assert!(matches!(outcome, Err(ref error) if error.contains("local folder is unavailable")));
        let state = remote.state();
        assert!(state.deletes.is_empty());
        assert!(state.uploads.is_empty());
        assert!(state.renames.is_empty());
        assert_eq!(state.files["/remote/untracked"].0, b"never synced");
        if with_baseline {
            assert_eq!(state.files["/remote/gone"].0, b"original");
        }
    }
}

#[tokio::test]
async fn one_way_update_watcher_does_not_delete_from_an_empty_source() {
    let root = tempfile::tempdir().unwrap();
    let config = CloudConfig {
        sync_direction: CompareDirection::LocalToRemote,
        update_only: true,
        preserve_remote_deletes: false,
        ..pair(root.path())
    };
    let mut remote = MemRemote::new();
    let mut index = empty_index(&config);
    synced(&mut index, &remote, root.path(), "gone", b"original");
    std::fs::remove_file(root.path().join("gone")).unwrap();
    remote.put("/remote/untracked", b"never synced", SYNCED_AT);
    let (run, _) = execute(
        &config,
        work(prep(&config, &index, &["gone", "untracked"])),
        &mut remote,
        None,
    )
    .await;
    let (result, _) = done(run);
    assert_eq!(result.deleted, 0);
    assert!(remote.state().deletes.is_empty());
    assert_eq!(remote.state().files["/remote/gone"].0, b"original");
    assert_eq!(remote.state().files["/remote/untracked"].0, b"never synced");
}

#[tokio::test]
async fn one_way_update_watcher_keeps_and_reports_untracked_destinations_every_pass() {
    for direction in [
        CompareDirection::LocalToRemote,
        CompareDirection::RemoteToLocal,
    ] {
        for bulk in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let upload = direction == CompareDirection::LocalToRemote;
            let config = CloudConfig {
                sync_direction: direction,
                update_only: true,
                preserve_remote_deletes: false,
                ..pair(root.path())
            };
            let mut remote = MemRemote::new();
            remote.state().bulk = bulk;
            let mut index = empty_index(&config);
            synced(
                &mut index,
                &remote,
                root.path(),
                "keep",
                b"source stays available",
            );
            if upload {
                synced(
                    &mut index,
                    &remote,
                    root.path(),
                    "gone",
                    b"previously synced",
                );
                std::fs::remove_file(root.path().join("gone")).unwrap();
                remote.put("/remote/phone", b"only copy from lost phone", SYNCED_AT);
            } else {
                std::fs::write(root.path().join("phone"), b"only copy from lost phone").unwrap();
            }
            for round in 0..3 {
                let prepared = work(prep(&config, &index, &["phone", "gone"]));
                let (run, saved) = execute(&config, prepared, &mut remote, None).await;
                let (result, failed) = done(run);
                assert!(failed.is_empty());
                assert!(result.errors.is_empty(), "{:?}", result.errors);
                assert_eq!(result.deleted, u32::from(upload && round == 0));
                let warnings: Vec<_> = result
                    .skipped_details
                    .iter()
                    .filter(|entry| entry.needs_review)
                    .collect();
                assert_eq!(warnings.len(), 1);
                assert_eq!(warnings[0].path, "phone");
                assert!(warnings[0].reason.contains("AeroSync"));
                assert!(!saved.files.contains_key("phone"));
                let bytes = if upload {
                    remote.state().files["/remote/phone"].0.clone()
                } else {
                    std::fs::read(root.path().join("phone")).unwrap()
                };
                assert_eq!(bytes, b"only copy from lost phone");
                assert!(remote
                    .state()
                    .deletes
                    .iter()
                    .all(|path| path == "/remote/gone"));
                index = saved;
            }
        }
    }
}

/// A scan-time comparison, deliberately retained while the next test changes the destination.
fn update_row(
    config: &CloudConfig,
    remote: &MemRemote,
    key: &str,
    synced_before: bool,
) -> FileComparison {
    let local = Disk::new(&config.local_folder).entry(key).info().cloned();
    let path = format!("/remote/{key}");
    let remote_info = remote
        .state()
        .files
        .get(&path)
        .map(|(bytes, time)| FileInfo {
            name: key.into(),
            path: path.clone(),
            size: bytes.len() as u64,
            modified: DateTime::<Utc>::from_timestamp(*time, 0),
            is_dir: false,
            checksum: None,
            checksum_alg: None,
        });
    FileComparison {
        relative_path: key.into(),
        local_relative_path: None,
        remote_relative_path: None,
        status: match (local.is_some(), remote_info.is_some()) {
            (true, true) => SyncStatus::Conflict,
            (true, false) => SyncStatus::LocalOnly,
            _ => SyncStatus::RemoteOnly,
        },
        local_info: local,
        remote_info,
        is_dir: false,
        sync_reason: String::new(),
        previously_synced: synced_before,
        baseline_key: None,
        name_match_blocked: false,
    }
}

/// Explicit receipt for tests whose premise is a previous completed transfer.
fn remember_transferred_fixture(svc: &CloudService, config: &CloudConfig, row: &FileComparison) {
    let destination = match config.sync_direction {
        CompareDirection::LocalToRemote => row.remote_info.as_ref(),
        _ => row.local_info.as_ref(),
    }
    .unwrap();
    svc.cloud_receipts.lock().unwrap().insert(
        row.relative_path.clone(),
        crate::sync::CloudTransferReceipt {
            scope: CloudService::receipt_scope(config),
            remote_key: row
                .remote_relative_path
                .as_deref()
                .unwrap_or(&row.relative_path)
                .to_string(),
            destination_side: config.sync_direction,
            destination: SyncIndexEntry {
                size: destination.size,
                modified: destination.modified,
                is_dir: false,
                uploaded_at: None,
            },
        },
    );
}

#[tokio::test]
async fn update_freshness_guard_covers_single_and_real_bulk_and_preserves_prior_baseline() {
    for upload in [true, false] {
        for bulk in [true, false] {
            let root = tempfile::tempdir().unwrap();
            let config = CloudConfig {
                update_only: true,
                sync_direction: if upload {
                    CompareDirection::LocalToRemote
                } else {
                    CompareDirection::RemoteToLocal
                },
                ..pair(root.path())
            };
            let mut remote = MemRemote::new();
            remote.state().bulk = bulk;
            let mut prior = empty_index(&config);
            for name in ["a", "b", "c"] {
                synced(&mut prior, &remote, root.path(), name, b"original");
                if upload {
                    std::fs::write(root.path().join(name), b"source edit").unwrap();
                    set_mtime(&root.path().join(name), SYNCED_AT + 100);
                } else {
                    remote.put(&format!("/remote/{name}"), b"source edit", SYNCED_AT + 100);
                }
            }
            let rows: Vec<_> = ["a", "b", "c"]
                .iter()
                .map(|name| update_row(&config, &remote, name, true))
                .collect();
            if upload {
                remote.put("/remote/a", b"concurrent newer", SYNCED_AT + 500);
            } else {
                std::fs::write(root.path().join("a"), b"concurrent newer").unwrap();
                set_mtime(&root.path().join("a"), SYNCED_AT + 500);
            }
            let svc = CloudService::new();
            svc.init(config.clone()).await;
            let policy = UpdatePolicy {
                root_identity: super::super::LocalRootIdentity::read(root.path()),
                allow_orphan_deletes: true,
                ..Default::default()
            };
            let mut ahead = super::super::bulk::BulkAhead::new(policy);
            for i in 0..rows.len() {
                let action = svc
                    .next_outcome(&mut remote, &config, &rows, i, &mut ahead)
                    .await
                    .unwrap();
                assert_eq!(
                    action,
                    if i == 0 {
                        SyncAction::Skip
                    } else if upload {
                        SyncAction::Upload
                    } else {
                        SyncAction::Download
                    }
                );
            }
            let kept = if upload {
                remote.state().files["/remote/a"].0.clone()
            } else {
                std::fs::read(root.path().join("a")).unwrap()
            };
            assert_eq!(kept, b"concurrent newer");
            let saved = svc.post_sync_baseline(&rows, &config, Some(&prior), policy);
            assert_eq!(saved["a"].modified, prior.files["a"].modified);
            assert_eq!(saved["a"].size, prior.files["a"].size);
            if bulk && upload {
                assert_eq!(remote.state().bulks[0].len(), 2);
            }
        }
    }
}

#[tokio::test]
async fn update_source_only_upload_refuses_a_non_authoritative_destination() {
    let root = tempfile::tempdir().unwrap();
    let config = CloudConfig {
        update_only: true,
        sync_direction: CompareDirection::LocalToRemote,
        ..pair(root.path())
    };
    let mut remote = MemRemote::new();
    remote.state().non_authoritative = true;
    std::fs::write(root.path().join("new"), b"source").unwrap();
    let row = update_row(&config, &remote, "new", false);
    let svc = CloudService::new();
    svc.init(config.clone()).await;
    let policy = UpdatePolicy {
        root_identity: super::super::LocalRootIdentity::read(root.path()),
        ..Default::default()
    };
    let error = svc
        .process_comparison_with_provider(&mut remote, &config, &row, policy)
        .await
        .unwrap_err();
    assert!(error.contains("not authoritative"));
    assert!(remote.state().uploads.is_empty());
}

#[tokio::test]
async fn update_recreated_source_is_not_propagated_as_a_delete_in_either_direction() {
    for upload in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let config = CloudConfig {
            update_only: true,
            preserve_remote_deletes: false,
            sync_direction: if upload {
                CompareDirection::LocalToRemote
            } else {
                CompareDirection::RemoteToLocal
            },
            ..pair(root.path())
        };
        let mut remote = MemRemote::new();
        let mut prior = empty_index(&config);
        for name in ["keep", "gone"] {
            synced(&mut prior, &remote, root.path(), name, b"original");
        }
        if upload {
            std::fs::remove_file(root.path().join("gone")).unwrap();
        } else {
            remote.state().files.remove("/remote/gone");
        }
        let row = update_row(&config, &remote, "gone", true);
        if upload {
            std::fs::write(root.path().join("gone"), b"recreated").unwrap();
        } else {
            remote.put("/remote/gone", b"recreated", SYNCED_AT + 100);
        }
        let svc = CloudService::new();
        svc.init(config.clone()).await;
        remember_transferred_fixture(&svc, &config, &row);
        let policy = UpdatePolicy {
            allow_orphan_deletes: true,
            root_identity: super::super::LocalRootIdentity::read(root.path()),
            ..Default::default()
        };
        assert_eq!(
            svc.process_comparison_with_provider(&mut remote, &config, &row, policy)
                .await
                .unwrap(),
            SyncAction::Skip
        );
        assert!(remote.state().deletes.is_empty());
        assert!(root.path().join("gone").exists());
        let saved = svc.post_sync_baseline(&[row], &config, Some(&prior), policy);
        assert!(saved.contains_key("gone"));
    }
}

/// Replace a local parent or root while the provider stat yields control.
/// Neither downloads nor propagated deletes may touch the replacement tree.
#[cfg(unix)]
#[tokio::test]
async fn update_rechecks_local_parent_and_root_after_async_stat() {
    for replace_root in [false, true] {
        for delete in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("local");
            let outside = temp.path().join("outside");
            std::fs::create_dir_all(root.join("sub")).unwrap();
            std::fs::create_dir_all(&outside).unwrap();
            std::fs::write(outside.join("f"), b"outside sole copy").unwrap();
            std::fs::write(root.join("sub/f"), b"old").unwrap();
            set_mtime(&root.join("sub/f"), SYNCED_AT);
            let config = CloudConfig {
                update_only: true,
                preserve_remote_deletes: false,
                sync_direction: CompareDirection::RemoteToLocal,
                ..pair(&root)
            };
            let mut remote = MemRemote::new();
            if !delete {
                remote.put("/remote/sub/f", b"new source", SYNCED_AT + 100);
            }
            let row = update_row(&config, &remote, "sub/f", true);
            let policy = UpdatePolicy {
                root_identity: super::super::LocalRootIdentity::read(&root),
                allow_orphan_deletes: true,
                ..Default::default()
            };
            let moved = temp.path().join("original");
            let replaced = if replace_root {
                root.clone()
            } else {
                root.join("sub")
            };
            let target = outside.clone();
            remote.state().on_stat = Some(Box::new(move || {
                std::fs::rename(&replaced, &moved).unwrap();
                if replace_root {
                    std::fs::create_dir_all(replaced.join("sub")).unwrap();
                    std::fs::write(replaced.join("sub/f"), b"replacement tree").unwrap();
                } else {
                    std::os::unix::fs::symlink(target, replaced).unwrap();
                }
            }));
            let svc = CloudService::new();
            svc.init(config.clone()).await;
            if delete {
                remember_transferred_fixture(&svc, &config, &row);
            }
            let error = svc
                .process_comparison_with_provider(&mut remote, &config, &row, policy)
                .await
                .unwrap_err();
            assert!(!error.is_empty());
            assert_eq!(
                std::fs::read(outside.join("f")).unwrap(),
                b"outside sole copy"
            );
            assert!(remote.state().downloads.is_empty());
            assert!(remote.state().deletes.is_empty());
            if replace_root {
                assert_eq!(
                    std::fs::read(root.join("sub/f")).unwrap(),
                    b"replacement tree"
                );
            }
        }
    }
}

/// A successful but non-authoritative listing cannot retire a saved warning.
#[tokio::test]
async fn update_incomplete_inventory_cannot_retire_review_notices() {
    let root = tempfile::tempdir().unwrap();
    let config = CloudConfig {
        update_only: true,
        sync_direction: CompareDirection::LocalToRemote,
        ..pair(root.path())
    };
    let mut remote = MemRemote::new();
    remote.state().non_authoritative = true;
    let svc = CloudService::new();
    svc.init(config.clone()).await;
    let plan = svc
        .build_sync_plan_with_provider(&mut remote, &config, true)
        .await
        .unwrap();
    assert!(!plan.inventory_complete);
    let result = SyncOperationResult {
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
    let partial = CloudService::completion_payload(&config, &result, plan.inventory_complete);
    assert_eq!(partial["review_inventory_complete"], false);
    remote.state().non_authoritative = false;
    let complete = svc
        .build_sync_plan_with_provider(&mut remote, &config, true)
        .await
        .unwrap();
    assert!(complete.inventory_complete);
    assert_eq!(
        CloudService::completion_payload(&config, &result, true)["review_scope"],
        partial["review_scope"]
    );
}

#[tokio::test]
async fn update_receive_initializes_a_missing_destination_only_for_execution() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("missing");
    let config = CloudConfig {
        update_only: true,
        sync_direction: CompareDirection::RemoteToLocal,
        ..pair(&root)
    };
    let mut remote = MemRemote::new();
    remote.put("/remote/photo", b"sole remote copy", SYNCED_AT);
    let svc = CloudService::new();
    svc.init(config.clone()).await;
    let preview = svc
        .build_sync_plan_with_provider(&mut remote, &config, false)
        .await
        .unwrap();
    assert!(!root.exists());
    assert_eq!(preview.comparisons.len(), 1);
    let plan = svc
        .build_sync_plan_with_provider(&mut remote, &config, true)
        .await
        .unwrap();
    assert!(root.is_dir());
    for row in &plan.comparisons {
        svc.process_comparison_with_provider(&mut remote, &config, row, plan.update_policy)
            .await
            .unwrap();
    }
    assert_eq!(
        std::fs::read(root.join("photo")).unwrap(),
        b"sole remote copy"
    );
}

/// The fresh delete check must catch a modification made after the full plan was built.
#[tokio::test]
async fn update_fresh_delete_keeps_a_destination_changed_after_scan() {
    for upload in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let config = CloudConfig {
            update_only: true,
            preserve_remote_deletes: false,
            sync_direction: if upload {
                CompareDirection::LocalToRemote
            } else {
                CompareDirection::RemoteToLocal
            },
            ..pair(root.path())
        };
        let mut remote = MemRemote::new();
        let mut index = empty_index(&config);
        synced(&mut index, &remote, root.path(), "keep", b"keep");
        std::fs::write(root.path().join("photo"), b"old").unwrap();
        set_mtime(&root.path().join("photo"), SYNCED_AT);
        if upload {
            set_mtime(&root.path().join("photo"), SYNCED_AT + 100);
        } else {
            remote.put("/remote/photo", b"remote source", SYNCED_AT + 100);
        }
        let (run, saved) = execute(
            &config,
            work(prep(&config, &index, &["photo"])),
            &mut remote,
            None,
        )
        .await;
        assert!(done(run).0.errors.is_empty());
        assert!(saved.cloud_receipts.contains_key("photo"));
        if upload {
            std::fs::remove_file(root.path().join("photo")).unwrap();
        } else {
            remote.state().files.remove("/remote/photo");
        }
        let row = update_row(&config, &remote, "photo", true);
        let svc = CloudService::new();
        svc.init(config.clone()).await;
        svc.remember_cloud_receipts(Some(&saved));
        let policy = UpdatePolicy {
            root_identity: super::super::LocalRootIdentity::read(root.path()),
            allow_orphan_deletes: true,
            ..Default::default()
        };
        assert_eq!(
            svc.resolve_action(&config, &row, policy),
            if upload {
                SyncAction::DeleteRemote
            } else {
                SyncAction::DeleteLocal
            }
        );
        if upload {
            remote.put("/remote/photo", b"new phone copy", SYNCED_AT + 900);
        } else {
            std::fs::write(root.path().join("photo"), b"new local copy").unwrap();
        }
        let outcome = svc
            .process_comparison_with_provider(&mut remote, &config, &row, policy)
            .await
            .unwrap();
        assert_eq!(outcome, SyncAction::Skip);
        assert!(remote.state().deletes.is_empty());
        let bytes = if upload {
            remote.state().files["/remote/photo"].0.clone()
        } else {
            std::fs::read(root.path().join("photo")).unwrap()
        };
        assert_eq!(
            bytes,
            if upload {
                b"new phone copy"
            } else {
                b"new local copy"
            }
        );
        let mut result = SyncOperationResult {
            uploaded: 0,
            downloaded: 0,
            deleted: 0,
            skipped: 0,
            skipped_details: vec![],
            conflicts: 0,
            errors: vec![],
            duration_secs: 0,
            file_details: vec![],
        };
        svc.record_sync_action(&mut result, &config, &row, &outcome);
        assert!(result.skipped_details[0].needs_review);
    }
}

/// A proof for another server, root, alias or direction is not authority at this destination.
#[tokio::test]
async fn update_receipt_cannot_be_borrowed_across_profile_root_side_or_alias() {
    let root = tempfile::tempdir().unwrap();
    let config = CloudConfig {
        update_only: true,
        preserve_remote_deletes: false,
        sync_direction: CompareDirection::LocalToRemote,
        ..pair(root.path())
    };
    let remote = MemRemote::new();
    let mut index = empty_index(&config);
    synced(&mut index, &remote, root.path(), "photo", b"sent bytes");
    std::fs::remove_file(root.path().join("photo")).unwrap();
    let row = update_row(&config, &remote, "photo", true);
    let svc = CloudService::new();
    svc.remember_cloud_receipts(Some(&index));
    let policy = UpdatePolicy {
        allow_orphan_deletes: true,
        ..Default::default()
    };
    assert_eq!(
        svc.resolve_action(&config, &row, policy),
        SyncAction::DeleteRemote
    );
    for change in ["profile", "root", "side", "alias"] {
        let mut cfg = config.clone();
        let mut changed = row.clone();
        match change {
            "profile" => cfg.server_profile = "different server".into(),
            "root" => cfg.local_folder = root.path().join("different"),
            "side" => cfg.sync_direction = CompareDirection::RemoteToLocal,
            _ => changed.remote_relative_path = Some("different-photo".into()),
        }
        assert!(svc.cloud_receipt_for(&cfg, &changed).is_none(), "{change}");
    }
}

/// Two-sided skips must remain untrusted even after watcher baseline save/reload.
#[tokio::test]
async fn update_watcher_skip_does_not_create_future_delete_authority() {
    for upload in [true, false] {
        for identical in [true, false] {
            let root = tempfile::tempdir().unwrap();
            let config = CloudConfig {
                update_only: true,
                preserve_remote_deletes: false,
                sync_direction: if upload {
                    CompareDirection::LocalToRemote
                } else {
                    CompareDirection::RemoteToLocal
                },
                ..pair(root.path())
            };
            let mut remote = MemRemote::new();
            let mut index = empty_index(&config);
            synced(&mut index, &remote, root.path(), "keep", b"keep");
            std::fs::write(root.path().join("photo"), b"local").unwrap();
            set_mtime(
                &root.path().join("photo"),
                if !upload && !identical {
                    SYNCED_AT + 500
                } else {
                    SYNCED_AT
                },
            );
            remote.put(
                "/remote/photo",
                if identical { b"local" } else { b"cloud" },
                if upload && !identical {
                    SYNCED_AT + 500
                } else {
                    SYNCED_AT
                },
            );
            let (run, saved) = execute(
                &config,
                work(prep(&config, &index, &["photo"])),
                &mut remote,
                None,
            )
            .await;
            assert_eq!(done(run).0.deleted, 0);
            assert!(!saved.cloud_receipts.contains_key("photo"));
            index = serde_json::from_str(&serde_json::to_string(&saved).unwrap()).unwrap();
            if upload {
                std::fs::remove_file(root.path().join("photo")).unwrap();
            } else {
                remote.state().files.remove("/remote/photo");
            }
            for _ in 0..3 {
                let (run, saved) = execute(
                    &config,
                    work(prep(&config, &index, &["photo"])),
                    &mut remote,
                    None,
                )
                .await;
                let result = done(run).0;
                assert_eq!(result.deleted, 0);
                assert!(result
                    .skipped_details
                    .iter()
                    .any(|row| row.path == "photo" && row.needs_review));
                assert!(!saved.cloud_receipts.contains_key("photo"));
                let bytes = if upload {
                    remote.state().files["/remote/photo"].0.clone()
                } else {
                    std::fs::read(root.path().join("photo")).unwrap()
                };
                assert_eq!(
                    bytes,
                    if upload && !identical {
                        b"cloud"
                    } else {
                        b"local"
                    }
                );
                index = saved;
            }
        }
    }
}

#[tokio::test]
async fn update_watcher_with_only_excluded_entries_cannot_delete() {
    let root = tempfile::tempdir().unwrap();
    let config = CloudConfig {
        update_only: true,
        preserve_remote_deletes: false,
        sync_direction: CompareDirection::LocalToRemote,
        exclude_patterns: vec!["*.tmp".into()],
        ..pair(root.path())
    };
    let mut remote = MemRemote::new();
    let mut index = empty_index(&config);
    synced(&mut index, &remote, root.path(), "gone", b"original");
    std::fs::remove_file(root.path().join("gone")).unwrap();
    std::fs::write(root.path().join("ignored.tmp"), b"excluded").unwrap();
    let (run, _) = execute(
        &config,
        work(prep(&config, &index, &["gone"])),
        &mut remote,
        None,
    )
    .await;
    assert_eq!(done(run).0.deleted, 0);
    assert!(remote.state().deletes.is_empty());
}

#[tokio::test]
async fn watcher_discards_a_prepared_rename_after_retention_or_direction_changes() {
    for change in ["direction", "update", "retention", "remote", "profile"] {
        let root = tempfile::tempdir().unwrap();
        let mut config = CloudConfig {
            sync_direction: CompareDirection::LocalToRemote,
            preserve_remote_deletes: false,
            ..pair(root.path())
        };
        let mut remote = MemRemote::new();
        let mut index = empty_index(&config);
        synced(&mut index, &remote, root.path(), "old", b"original");
        std::fs::rename(root.path().join("old"), root.path().join("new")).unwrap();
        let prepared = work(
            plan(
                &config,
                Some(index),
                &changes(root.path(), &["old", "new"], &[("old", "new")]),
                EXACT,
            )
            .unwrap(),
        );
        match change {
            "direction" => config.sync_direction = CompareDirection::RemoteToLocal,
            "update" => config.update_only = true,
            "retention" => config.preserve_remote_deletes = true,
            "remote" => config.remote_folder = "/other".into(),
            _ => config.server_profile_id = Some("different".into()),
        };
        let svc = CloudService::new();
        svc.init(config).await;
        assert!(svc
            .execute_incremental(&mut remote, prepared, None)
            .await
            .is_err());
        assert!(remote.state().renames.is_empty());
        assert!(remote.state().deletes.is_empty());
    }
}

#[tokio::test]
async fn update_watcher_large_delete_batch_waits_for_destination_full_scan() {
    let root = tempfile::tempdir().unwrap();
    let config = CloudConfig {
        update_only: true,
        preserve_remote_deletes: false,
        sync_direction: CompareDirection::LocalToRemote,
        ..pair(root.path())
    };
    let mut remote = MemRemote::new();
    let mut index = empty_index(&config);
    synced(&mut index, &remote, root.path(), "keep", b"kept");
    let names: Vec<_> = (0..10).map(|i| format!("gone{i}")).collect();
    for name in &names {
        synced(&mut index, &remote, root.path(), name, b"original");
        std::fs::remove_file(root.path().join(name)).unwrap();
    }
    for i in 0..100 {
        index
            .files
            .insert(format!("unrelated{i}"), index.files["keep"].clone());
    }
    let prepared = work(prep(
        &config,
        &index,
        &names.iter().map(String::as_str).collect::<Vec<_>>(),
    ));
    let (run, _) = execute(&config, prepared, &mut remote, Some(REMOTE_WORK_LIMIT)).await;
    assert!(matches!(run, IncrementalRun::TooLarge(_)));
    assert!(remote.state().deletes.is_empty());
    // The provider can defer the full scan while its work is rate-limited.
    let prepared = work(prep(
        &config,
        &index,
        &names.iter().map(String::as_str).collect::<Vec<_>>(),
    ));
    let (run, saved) = execute(&config, prepared, &mut remote, None).await;
    let (result, failed) = done(run);
    assert_eq!(result.deleted, 0);
    assert!(failed.is_empty());
    assert!(remote.state().deletes.is_empty());
    assert!(result
        .errors
        .iter()
        .any(|error| error.contains("full destination scan")));
    assert_eq!(
        serde_json::to_value(&saved.files).unwrap(),
        serde_json::to_value(&index.files).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&saved.cloud_receipts).unwrap(),
        serde_json::to_value(&index.cloud_receipts).unwrap()
    );
}

#[tokio::test]
async fn one_way_update_rename_does_not_replace_a_newer_destination() {
    let root = tempfile::tempdir().unwrap();
    let config = CloudConfig {
        sync_direction: CompareDirection::LocalToRemote,
        update_only: true,
        preserve_remote_deletes: false,
        ..pair(root.path())
    };
    let mut remote = MemRemote::new();
    let mut index = empty_index(&config);
    synced(&mut index, &remote, root.path(), "report.pdf", b"original");
    std::fs::rename(
        root.path().join("report.pdf"),
        root.path().join("final.pdf"),
    )
    .unwrap();
    remote.put(
        "/remote/final.pdf",
        b"newer from another device",
        SYNCED_AT + 1000,
    );
    let prepared = plan(
        &config,
        Some(index),
        &changes(
            root.path(),
            &["report.pdf", "final.pdf"],
            &[("report.pdf", "final.pdf")],
        ),
        EXACT,
    )
    .unwrap();
    let (run, _) = execute(&config, work(prepared), &mut remote, None).await;
    let (result, _) = done(run);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_eq!(result.uploaded, 0);
    assert!(remote.state().renames.is_empty());
    assert_eq!(
        remote.state().files["/remote/final.pdf"].0,
        b"newer from another device"
    );
    assert_eq!(
        std::fs::read(root.path().join("final.pdf")).unwrap(),
        b"original"
    );
}

/// A local delete reaches the remote only when the pair propagates deletes
/// that way, as the full sync decides it.
#[tokio::test]
async fn a_local_delete_propagates_only_when_the_pair_allows_it() {
    struct Case {
        direction: CompareDirection,
        preserve_remote_deletes: bool,
        remote_deleted: bool,
        local_restored: bool,
    }
    let cases = [
        Case {
            direction: CompareDirection::Bidirectional,
            preserve_remote_deletes: true,
            remote_deleted: true,
            local_restored: false,
        },
        Case {
            direction: CompareDirection::LocalToRemote,
            preserve_remote_deletes: true,
            remote_deleted: false,
            local_restored: false,
        },
        Case {
            direction: CompareDirection::LocalToRemote,
            preserve_remote_deletes: false,
            remote_deleted: true,
            local_restored: false,
        },
        Case {
            direction: CompareDirection::RemoteToLocal,
            preserve_remote_deletes: false,
            remote_deleted: false,
            local_restored: true,
        },
    ];
    for case in cases {
        let root = tempfile::tempdir().unwrap();
        let config = CloudConfig {
            sync_direction: case.direction,
            preserve_remote_deletes: case.preserve_remote_deletes,
            ..pair(root.path())
        };
        let mut remote = MemRemote::new();
        let mut index = empty_index(&config);
        synced(&mut index, &remote, root.path(), "a.txt", b"aaa");
        synced(&mut index, &remote, root.path(), "keep.txt", b"kkk");
        std::fs::remove_file(root.path().join("a.txt")).unwrap();

        let (run, saved) = execute(
            &config,
            work(prep(&config, &index, &["a.txt"])),
            &mut remote,
            None,
        )
        .await;
        let (result, _) = done(run);
        let label = format!(
            "{:?} preserve={}",
            case.direction, case.preserve_remote_deletes
        );
        assert!(result.errors.is_empty(), "{label}: {:?}", result.errors);
        assert_eq!(
            !remote.state().files.contains_key("/remote/a.txt"),
            case.remote_deleted,
            "{label}"
        );
        assert_eq!(
            saved.files.contains_key("a.txt"),
            !case.remote_deleted,
            "{label}"
        );
        assert_eq!(
            root.path().join("a.txt").exists(),
            case.local_restored,
            "{label}"
        );
        assert_eq!(remote.state().lists, 0, "{label}");
    }
}

#[tokio::test]
async fn a_local_change_in_a_receive_only_pair_is_not_sent() {
    let root = tempfile::tempdir().unwrap();
    let config = CloudConfig {
        sync_direction: CompareDirection::RemoteToLocal,
        ..pair(root.path())
    };
    let mut remote = MemRemote::new();
    std::fs::write(root.path().join("mine.txt"), b"local only").unwrap();
    let (run, saved) = execute(
        &config,
        work(prep(&config, &empty_index(&config), &["mine.txt"])),
        &mut remote,
        None,
    )
    .await;
    let (result, _) = done(run);
    assert_eq!(result.uploaded, 0);
    assert!(remote.state().uploads.is_empty());
    assert!(
        !saved.files.contains_key("mine.txt"),
        "a spared file gets no baseline"
    );
}

/// The exclude list, the excluded folders and `.aeroignore` leave a path out
/// exactly as the scan does: nothing is asked, sent or deleted for it.
#[test]
fn an_excluded_path_is_left_alone() {
    let root = tempfile::tempdir().unwrap();
    let config = CloudConfig {
        exclude_patterns: vec!["*.log".to_string()],
        excluded_folders: vec!["private".to_string()],
        ..pair(root.path())
    };
    std::fs::write(root.path().join(".aeroignore"), "secret/\n").unwrap();
    for dir in ["private", "secret", "open"] {
        std::fs::create_dir(root.path().join(dir)).unwrap();
    }
    for file in ["app.log", "private/x.txt", "secret/y.txt", "open/z.txt"] {
        std::fs::write(root.path().join(file), b"data").unwrap();
    }
    let index = empty_index(&config);
    for excluded in [
        "app.log",
        "private/x.txt",
        "secret/y.txt",
        "private",
        "secret",
    ] {
        assert!(settled(&prep(&config, &index, &[excluded])), "{excluded}");
    }
    let plan = work(prep(
        &config,
        &index,
        &["app.log", "private/x.txt", "secret/y.txt", "open/z.txt"],
    ));
    assert_eq!(plan.work.keys().collect::<Vec<_>>(), vec!["open/z.txt"]);
}

/// The scan does not follow links: a path through one is neither sent nor
/// read as deleted, whatever its baseline says.
#[cfg(unix)]
#[test]
fn a_path_under_a_link_is_left_alone() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("f.txt"), b"elsewhere").unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("link")).unwrap();
    let config = pair(root.path());
    let mut index = empty_index(&config);
    index.files.insert(
        "link/old.txt".to_string(),
        SyncIndexEntry {
            size: 1,
            modified: DateTime::<Utc>::from_timestamp(SYNCED_AT, 0),
            is_dir: false,
            uploaded_at: None,
        },
    );
    assert!(settled(&prep(
        &config,
        &index,
        &["link", "link/f.txt", "link/old.txt"]
    )));
}

/// A folder moved into the pair arrives as one event: its tree is read on
/// disk and sent, the remote is never listed.
#[tokio::test]
async fn a_folder_moved_in_is_walked_and_sent_without_a_listing() {
    let root = tempfile::tempdir().unwrap();
    let config = pair(root.path());
    let mut remote = MemRemote::new();
    std::fs::create_dir_all(root.path().join("album/sub")).unwrap();
    for file in ["album/a.jpg", "album/b.jpg", "album/sub/c.jpg"] {
        std::fs::write(root.path().join(file), file.as_bytes()).unwrap();
    }
    let plan = work(prep(&config, &empty_index(&config), &["album"]));
    assert_eq!(plan.remote_work(), 5, "two folders and three files");
    let (run, saved) = execute(&config, plan, &mut remote, None).await;
    let (result, _) = done(run);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    let state = remote.state();
    assert_eq!(state.lists, 0);
    for file in ["album/a.jpg", "album/b.jpg", "album/sub/c.jpg"] {
        assert_eq!(state.files[&format!("/remote/{file}")].0, file.as_bytes());
        assert!(saved.files.contains_key(file));
    }
    assert!(saved.files["album/sub"].is_dir);
}

/// A rename moves the remote copy: no upload, no delete, and the baseline
/// follows it, for a file and for a folder.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn shark_review_rename_uses_the_recorded_remote_identity() {
    let root = tempfile::tempdir().unwrap();
    let config = pair(root.path());
    let mut remote = MemRemote::new();
    let mut index = empty_index(&config);
    let local_key = "cafe\u{301}.txt";
    let remote_key = "café.txt";
    synced(&mut index, &remote, root.path(), local_key, b"tracked");
    remote.state().files.remove(&format!("/remote/{local_key}"));
    remote.put(&format!("/remote/{remote_key}"), b"tracked", SYNCED_AT);
    index
        .remote_spellings
        .insert(local_key.into(), remote_key.into());
    remote.put(
        &format!("/remote/{local_key}"),
        b"independent remote twin",
        SYNCED_AT,
    );
    std::fs::rename(root.path().join(local_key), root.path().join("renamed.txt")).unwrap();
    let prepared = plan(
        &config,
        Some(index),
        &changes(
            root.path(),
            &[local_key, "renamed.txt"],
            &[(local_key, "renamed.txt")],
        ),
        EXACT,
    )
    .unwrap();
    let (run, _) = execute(&config, work(prepared), &mut remote, None).await;
    let (result, _) = done(run);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_eq!(
        remote
            .state()
            .files
            .get(&format!("/remote/{local_key}"))
            .map(|f| f.0.as_slice()),
        Some(b"independent remote twin".as_slice())
    );
    assert_eq!(
        remote
            .state()
            .files
            .get("/remote/renamed.txt")
            .map(|f| f.0.as_slice()),
        Some(b"tracked".as_slice())
    );
}

#[tokio::test]
async fn a_rename_moves_the_remote_copy_instead_of_sending_it_again() {
    let root = tempfile::tempdir().unwrap();
    let config = pair(root.path());
    let mut remote = MemRemote::new();
    let mut index = empty_index(&config);
    synced(&mut index, &remote, root.path(), "report.pdf", b"pdf");
    synced(&mut index, &remote, root.path(), "photos/one.jpg", b"1");
    synced(&mut index, &remote, root.path(), "photos/two.jpg", b"22");
    index.files.insert(
        "photos".to_string(),
        SyncIndexEntry {
            size: 0,
            modified: None,
            is_dir: true,
            uploaded_at: None,
        },
    );
    std::fs::rename(
        root.path().join("report.pdf"),
        root.path().join("final.pdf"),
    )
    .unwrap();
    std::fs::rename(root.path().join("photos"), root.path().join("pictures")).unwrap();

    let renames = [("report.pdf", "final.pdf"), ("photos", "pictures")];
    let prep = plan(
        &config,
        Some(index),
        &changes(
            root.path(),
            &["report.pdf", "final.pdf", "photos", "pictures"],
            &renames,
        ),
        EXACT,
    )
    .unwrap();
    let plan = work(prep);
    assert_eq!(plan.renames.len(), 2);
    assert!(
        plan.work.is_empty(),
        "the moved baseline settles the new paths"
    );
    let (run, saved) = execute(&config, plan, &mut remote, None).await;
    let (result, _) = done(run);
    assert!(result.errors.is_empty(), "{:?}", result.errors);

    let state = remote.state();
    assert!(state.uploads.is_empty() && state.deletes.is_empty());
    assert_eq!(state.lists, 0);
    assert_eq!(state.files["/remote/final.pdf"].0, b"pdf");
    assert_eq!(state.files["/remote/pictures/two.jpg"].0, b"22");
    assert!(!state.files.contains_key("/remote/report.pdf"));
    for key in [
        "final.pdf",
        "pictures",
        "pictures/one.jpg",
        "pictures/two.jpg",
    ] {
        assert!(saved.files.contains_key(key), "{key}");
    }
    for key in ["report.pdf", "photos", "photos/one.jpg"] {
        assert!(!saved.files.contains_key(key), "{key}");
    }
}

#[tokio::test]
async fn filen_bridge_incremental_rename_waits_before_source_stat() {
    let mut remote = MemRemote::new();
    remote.put("/remote/fresh.dat", b"ok", SYNCED_AT);
    remote
        .state()
        .unpublished
        .insert("/remote/fresh.dat".into());
    let rename = PlannedRename {
        from: "fresh.dat".into(),
        to: "renamed.dat".into(),
        is_dir: false,
        moved: Vec::new(),
        fallback: BTreeMap::new(),
    };
    let outcome = CloudService::new()
        .rename_on_remote(&mut remote, "/remote", &rename)
        .await;
    assert!(outcome.is_ok(), "{outcome:?}");
    let state = remote.state();
    assert_eq!(state.readiness_calls, ["/remote/fresh.dat"]);
    assert_eq!(
        state.renames,
        [("/remote/fresh.dat".into(), "/remote/renamed.dat".into())]
    );
    assert!(state.uploads.is_empty() && state.deletes.is_empty());
}

#[tokio::test]
async fn filen_bridge_incremental_readiness_error_preserves_source() {
    let mut remote = MemRemote::new();
    remote.put("/remote/fresh.dat", b"ok", SYNCED_AT);
    remote.state().readiness_fails = true;
    let rename = PlannedRename {
        from: "fresh.dat".into(),
        to: "renamed.dat".into(),
        is_dir: false,
        moved: Vec::new(),
        fallback: BTreeMap::new(),
    };
    let outcome = CloudService::new()
        .rename_on_remote(&mut remote, "/remote", &rename)
        .await;
    assert!(outcome.unwrap_err().contains("not published"));
    let state = remote.state();
    assert!(state.renames.is_empty());
    assert!(state.files.contains_key("/remote/fresh.dat"));
}

#[tokio::test]
async fn filen_bridge_incremental_folder_is_not_probed_as_an_object() {
    let mut remote = MemRemote::new();
    remote.state().dirs.insert("/remote/folder".into());
    remote.state().readiness_fails = true;
    let rename = PlannedRename {
        from: "folder".into(),
        to: "renamed".into(),
        is_dir: true,
        moved: Vec::new(),
        fallback: BTreeMap::new(),
    };
    let outcome = CloudService::new()
        .rename_on_remote(&mut remote, "/remote", &rename)
        .await;
    assert!(outcome.is_ok(), "{outcome:?}");
    assert!(remote.state().readiness_calls.is_empty());
}

/// A remote that cannot rename gets the full sync's answer: the old copy
/// deleted, the new one sent.
#[tokio::test]
async fn a_rename_the_remote_refuses_is_synced_as_a_delete_and_an_upload() {
    let root = tempfile::tempdir().unwrap();
    let config = pair(root.path());
    let mut remote = MemRemote::new();
    remote.state().refuse_rename = true;
    let mut index = empty_index(&config);
    synced(&mut index, &remote, root.path(), "old.txt", b"text");
    std::fs::rename(root.path().join("old.txt"), root.path().join("new.txt")).unwrap();

    let prep = plan(
        &config,
        Some(index),
        &changes(
            root.path(),
            &["old.txt", "new.txt"],
            &[("old.txt", "new.txt")],
        ),
        EXACT,
    )
    .unwrap();
    let (run, saved) = execute(&config, work(prep), &mut remote, None).await;
    let (result, _) = done(run);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_eq!((result.uploaded, result.deleted), (1, 1));
    let state = remote.state();
    assert_eq!(state.files["/remote/new.txt"].0, b"text");
    assert!(!state.files.contains_key("/remote/old.txt"));
    assert!(saved.files.contains_key("new.txt") && !saved.files.contains_key("old.txt"));
}

/// Paths a refused rename leaves to sync one by one count against the pass's
/// limit: past it the pass stops with the baseline as it was, for a full scan.
#[tokio::test]
async fn a_refused_rename_past_the_limit_hands_over_to_a_full_scan() {
    let root = tempfile::tempdir().unwrap();
    let config = pair(root.path());
    let mut remote = MemRemote::new();
    remote.state().refuse_rename = true;
    let mut index = empty_index(&config);
    for file in ["d/1", "d/2", "d/3"] {
        synced(&mut index, &remote, root.path(), file, b"x");
    }
    index.files.insert(
        "d".to_string(),
        SyncIndexEntry {
            size: 0,
            modified: None,
            is_dir: true,
            uploaded_at: None,
        },
    );
    std::fs::rename(root.path().join("d"), root.path().join("e")).unwrap();
    let prep = plan(
        &config,
        Some(index),
        &changes(root.path(), &["d", "e"], &[("d", "e")]),
        EXACT,
    )
    .unwrap();
    let (run, saved) = execute(&config, work(prep), &mut remote, Some(2)).await;
    assert!(matches!(run, IncrementalRun::TooLarge(_)));
    assert!(saved.files.contains_key("d/1") && !saved.files.contains_key("e/1"));
    assert!(remote.state().uploads.is_empty() && remote.state().deletes.is_empty());
}

/// A local file whose remote copy is gone reads as deleted on the remote.
/// The pass does not delete it: a remote change waits for the full scan,
/// whose gate knows a vanished remote folder from a deleted file.
#[tokio::test]
async fn a_missing_remote_copy_never_deletes_the_local_file() {
    let root = tempfile::tempdir().unwrap();
    let config = pair(root.path());
    let mut remote = MemRemote::new();
    let mut index = empty_index(&config);
    synced(&mut index, &remote, root.path(), "notes.txt", b"v1");
    std::fs::write(root.path().join("notes.txt"), b"v2, edited").unwrap();
    remote.state().files.clear();

    let (run, saved) = execute(
        &config,
        work(prep(&config, &index, &["notes.txt"])),
        &mut remote,
        None,
    )
    .await;
    let (result, _) = done(run);
    assert!(root.path().join("notes.txt").exists());
    assert_eq!(result.deleted, 0);
    assert_eq!(result.skipped, 1);
    assert!(result.skipped_details[0].reason.contains("next full scan"));
    assert!(saved.files.contains_key("notes.txt"));
}

/// Deleting most of a synced folder at once trips the full sync's gate: no
/// remote copy is deleted.
#[tokio::test]
async fn a_mass_local_delete_trips_the_safety_gate() {
    let root = tempfile::tempdir().unwrap();
    let config = pair(root.path());
    let mut remote = MemRemote::new();
    let mut index = empty_index(&config);
    let names: Vec<String> = (0..20).map(|i| format!("f{i:02}.txt")).collect();
    for name in &names {
        synced(&mut index, &remote, root.path(), name, b"data");
    }
    let deleted: Vec<&str> = names[..15].iter().map(String::as_str).collect();
    for name in &deleted {
        std::fs::remove_file(root.path().join(name)).unwrap();
    }
    let (run, _) = execute(
        &config,
        work(prep(&config, &index, &deleted)),
        &mut remote,
        None,
    )
    .await;
    let (result, _) = done(run);
    assert_eq!(result.deleted, 0);
    assert!(remote.state().deletes.is_empty());
    assert_eq!(remote.state().files.len(), 20);
}

/// A `stat` that fails is not an absence: nothing is deleted on its word, the
/// baseline stays, and the path is reported for a retry.
#[tokio::test]
async fn an_unreadable_remote_copy_is_an_error_not_an_absence() {
    let root = tempfile::tempdir().unwrap();
    let config = pair(root.path());
    let mut remote = MemRemote::new();
    let mut index = empty_index(&config);
    synced(&mut index, &remote, root.path(), "a.txt", b"aaa");
    std::fs::remove_file(root.path().join("a.txt")).unwrap();
    remote
        .state()
        .unreadable
        .insert("/remote/a.txt".to_string());

    let (run, saved) = execute(
        &config,
        work(prep(&config, &index, &["a.txt"])),
        &mut remote,
        None,
    )
    .await;
    let (result, failed) = done(run);
    assert_eq!(result.deleted, 0);
    assert!(remote.state().files.contains_key("/remote/a.txt"));
    assert!(saved.files.contains_key("a.txt"));
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].key, "a.txt");
}

/// Gone on both sides: the baseline entry goes too. Kept, it would read a
/// new file created later at the same path as synced, and delete it.
#[tokio::test]
async fn a_path_gone_on_both_sides_leaves_the_baseline() {
    let root = tempfile::tempdir().unwrap();
    let config = pair(root.path());
    let mut remote = MemRemote::new();
    let mut index = empty_index(&config);
    synced(&mut index, &remote, root.path(), "a.txt", b"aaa");
    std::fs::remove_file(root.path().join("a.txt")).unwrap();
    remote.state().files.clear();
    let (run, saved) = execute(
        &config,
        work(prep(&config, &index, &["a.txt"])),
        &mut remote,
        None,
    )
    .await;
    assert!(done(run).0.errors.is_empty());
    assert!(!saved.files.contains_key("a.txt"));
}

/// A KeepBoth download that fails puts the local copy back: a path left empty
/// reads as a local deletion next cycle, which a two-way sync sends to the remote.
#[tokio::test]
async fn keep_both_restores_the_local_copy_when_the_download_fails() {
    let root = tempfile::tempdir().unwrap();
    let config = CloudConfig {
        conflict_strategy: ConflictStrategy::KeepBoth,
        ..pair(root.path())
    };
    let mut remote = MemRemote::new();
    let mut index = empty_index(&config);
    synced(&mut index, &remote, root.path(), "plan.txt", b"v1");
    std::fs::write(root.path().join("plan.txt"), b"local edit").unwrap();
    remote.put("/remote/plan.txt", b"remote edit!", SYNCED_AT + 500);
    remote.state().refuse_download = true;

    let (run, _) = execute(
        &config,
        work(prep(&config, &index, &["plan.txt"])),
        &mut remote,
        None,
    )
    .await;
    assert!(!done(run).0.errors.is_empty());
    assert_eq!(
        std::fs::read(root.path().join("plan.txt")).unwrap(),
        b"local edit"
    );
    let copies = std::fs::read_dir(root.path())
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("AeroCloud conflict")
        })
        .count();
    assert_eq!(copies, 0, "nothing stays under a conflict name");
}

/// A file that appeared at the path while the download ran is not replaced: both
/// stay, and the error says where the copy set aside is.
#[tokio::test]
async fn keep_both_keeps_an_edit_made_during_a_download_that_fails() {
    let root = tempfile::tempdir().unwrap();
    let config = CloudConfig {
        conflict_strategy: ConflictStrategy::KeepBoth,
        ..pair(root.path())
    };
    let mut remote = MemRemote::new();
    let mut index = empty_index(&config);
    synced(&mut index, &remote, root.path(), "plan.txt", b"v1");
    std::fs::write(root.path().join("plan.txt"), b"local edit").unwrap();
    remote.put("/remote/plan.txt", b"remote edit!", SYNCED_AT + 500);
    {
        let mut state = remote.state();
        state.refuse_download = true;
        state.edit_during_download = Some(root.path().join("plan.txt"));
    }

    let (run, _) = execute(
        &config,
        work(prep(&config, &index, &["plan.txt"])),
        &mut remote,
        None,
    )
    .await;
    let (result, _) = done(run);
    assert!(
        result
            .errors
            .iter()
            .any(|e| e.contains("AeroCloud conflict")),
        "{:?}",
        result.errors
    );
    assert_eq!(
        std::fs::read(root.path().join("plan.txt")).unwrap(),
        b"edited meanwhile"
    );
    let copy = std::fs::read_dir(root.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|path| path.to_string_lossy().contains("AeroCloud conflict"))
        .expect("the copy set aside stays");
    assert_eq!(std::fs::read(copy).unwrap(), b"local edit");
}

/// KeepBoth sets the local copy aside and puts the remote one at the path.
/// The baseline used to stay as it was, so the next cycle read the same
/// conflict and set aside another copy, one per cycle; the watcher's own
/// events after the download made that one every cooldown. The path now
/// settles, and the copy set aside is sent as a new file.
#[tokio::test]
async fn keep_both_sets_one_copy_aside_not_one_per_cycle() {
    let root = tempfile::tempdir().unwrap();
    let config = CloudConfig {
        conflict_strategy: ConflictStrategy::KeepBoth,
        ..pair(root.path())
    };
    let mut remote = MemRemote::new();
    let mut index = empty_index(&config);
    synced(&mut index, &remote, root.path(), "plan.txt", b"v1");
    std::fs::write(root.path().join("plan.txt"), b"local edit").unwrap();
    remote.put("/remote/plan.txt", b"remote edit!", SYNCED_AT + 500);

    let (run, saved) = execute(
        &config,
        work(prep(&config, &index, &["plan.txt"])),
        &mut remote,
        None,
    )
    .await;
    assert!(done(run).0.errors.is_empty());
    assert_eq!(
        std::fs::read(root.path().join("plan.txt")).unwrap(),
        b"remote edit!"
    );
    let copies: Vec<String> = std::fs::read_dir(root.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains("AeroCloud conflict"))
        .collect();
    assert_eq!(copies.len(), 1);

    // The watcher reports the download and the copy set aside.
    let next = work(prep(&config, &saved, &["plan.txt", copies[0].as_str()]));
    assert_eq!(
        next.work.keys().collect::<Vec<_>>(),
        vec![&copies[0]],
        "the path holds the remote copy the baseline names"
    );
    let (run, _) = execute(&config, next, &mut remote, None).await;
    let (result, _) = done(run);
    assert_eq!(result.uploaded, 1);
    let copies_now = std::fs::read_dir(root.path())
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("AeroCloud conflict")
        })
        .count();
    assert_eq!(copies_now, 1, "no second copy");
}

fn rclone_overlay(inner: MemRemote) -> CryptOverlayProvider {
    let (name_key, data_key, name_tweak) =
        crate::rclone_crypt::derive_keys_with_tweak("overlay-pass", "overlay-salt").unwrap();
    let keys = OverlayKeys::Rclone(RcloneCryptKeys {
        name_key,
        data_key,
        name_tweak,
        filename_encryption: FilenameEncryption::Standard,
        off_suffix: ".bin".to_string(),
        directory_name_encryption: true,
    });
    CryptOverlayProvider::new(Box::new(inner), keys, "/remote")
}

/// The pass reaches the remote through the pair's overlay stack, so a crypt
/// overlay encrypts the names it asks about, sends and renames, exactly as
/// on the full sync's path.
#[tokio::test]
async fn a_crypt_overlay_rewrites_the_names_the_pass_reaches() {
    let root = tempfile::tempdir().unwrap();
    let config = pair(root.path());
    let remote = MemRemote::new();
    let mut overlay = rclone_overlay(remote.clone());
    std::fs::create_dir(root.path().join("docs")).unwrap();
    std::fs::write(root.path().join("docs/secret.txt"), b"plaintext body").unwrap();

    let (run, saved) = execute(
        &config,
        work(prep(&config, &empty_index(&config), &["docs/secret.txt"])),
        &mut overlay,
        None,
    )
    .await;
    assert!(done(run).0.errors.is_empty());
    {
        let state = remote.state();
        assert_eq!(state.files.len(), 1);
        let (wire, (bytes, _)) = state.files.iter().next().unwrap();
        assert!(wire.starts_with("/remote/"), "{wire}");
        assert!(!wire.contains("docs") && !wire.contains("secret"), "{wire}");
        assert_ne!(bytes.as_slice(), b"plaintext body");
        assert!(state.stats.iter().all(|p| !p.contains("secret")));
    }

    std::fs::rename(
        root.path().join("docs/secret.txt"),
        root.path().join("docs/renamed.txt"),
    )
    .unwrap();
    let prep = plan(
        &config,
        Some(saved),
        &changes(
            root.path(),
            &["docs/secret.txt", "docs/renamed.txt"],
            &[("docs/secret.txt", "docs/renamed.txt")],
        ),
        EXACT,
    )
    .unwrap();
    let (run, _) = execute(&config, work(prep), &mut overlay, None).await;
    assert!(done(run).0.errors.is_empty());
    {
        let state = remote.state();
        assert_eq!(state.renames.len(), 1);
        let (from, to) = &state.renames[0];
        assert!(!from.contains("secret") && !to.contains("renamed"));
        assert_ne!(from, to);
        assert_eq!(state.uploads.len(), 1, "moved, not sent again");
        assert_eq!(state.readiness_calls.as_slice(), std::slice::from_ref(from));
    }
    let moved = overlay.stat("/remote/docs/renamed.txt").await.unwrap();
    assert_eq!(moved.size, 14);
}

/// The compress overlay keeps names and sizes on the wire: the pass reads no
/// size against the baseline for it, and the upload's own event settles.
#[tokio::test]
async fn a_compress_overlay_is_read_by_time_not_by_its_wire_size() {
    let root = tempfile::tempdir().unwrap();
    let config = CloudConfig {
        compress_enabled: true,
        ..pair(root.path())
    };
    let remote = MemRemote::new();
    let mut overlay = CompressOverlayProvider::new(Box::new(remote.clone()), 3);
    std::fs::write(root.path().join("big.txt"), "a".repeat(4096)).unwrap();
    let hints = IncrementalHints {
        compare_size: false,
    };
    let first = plan(
        &config,
        Some(empty_index(&config)),
        &changes(root.path(), &["big.txt"], &[]),
        hints,
    )
    .unwrap();
    let (run, saved) = execute(&config, work(first), &mut overlay, None).await;
    assert!(done(run).0.errors.is_empty());
    let wire_len = remote.state().files["/remote/big.txt"].0.len();
    assert!(wire_len < 4096, "sent compressed under its own name");
    let again = plan(
        &config,
        Some(saved),
        &changes(root.path(), &["big.txt"], &[]),
        hints,
    )
    .unwrap();
    assert!(settled(&again));
}

#[tokio::test]
async fn filen_bridge_move_preparation_crosses_both_overlays() {
    let remote = MemRemote::new();
    let mut overlay = CompressOverlayProvider::new(Box::new(rclone_overlay(remote.clone())), 3);
    overlay
        .prepare_file_move("/remote/📦-100%.dat")
        .await
        .unwrap();
    let state = remote.state();
    assert_eq!(state.readiness_calls.len(), 1);
    let wire = &state.readiness_calls[0];
    assert!(wire.starts_with("/remote/"));
    assert!(!wire.contains("📦") && !wire.contains("100%"), "{wire}");
}

/// The provider's floor is seen through both overlays.
#[test]
fn the_full_scan_floor_is_forwarded_by_the_overlays() {
    let remote = MemRemote::new();
    assert_eq!(remote.clone().min_full_scan_interval(), None);
    remote.state().floor = Some(Duration::from_secs(3600));
    let compress = CompressOverlayProvider::new(Box::new(remote.clone()), 3);
    assert_eq!(
        compress.min_full_scan_interval(),
        Some(Duration::from_secs(3600))
    );
    let crypt = rclone_overlay(remote.clone());
    assert_eq!(
        crypt.min_full_scan_interval(),
        Some(Duration::from_secs(3600))
    );
    let stacked = CompressOverlayProvider::new(Box::new(rclone_overlay(remote)), 3);
    assert_eq!(
        ProviderScanTraits::of(&stacked).min_full_scan_interval,
        Some(Duration::from_secs(3600))
    );
}

#[test]
fn what_only_a_full_scan_can_answer() {
    let root = tempfile::tempdir().unwrap();
    let config = pair(root.path());
    std::fs::write(root.path().join("a.txt"), b"a").unwrap();
    let paths = changes(root.path(), &["a.txt"], &[]);

    let no_baseline = plan(&config, None, &paths, EXACT).unwrap();
    assert!(matches!(no_baseline, IncrementalPrep::FullScanOnly(_)));

    let mut migrated = empty_index(&config);
    migrated.unverified_keys.insert("a.txt".to_string());
    assert!(matches!(
        plan(&config, Some(migrated), &paths, EXACT).unwrap(),
        IncrementalPrep::FullScanOnly(_)
    ));

    std::fs::write(root.path().join(".aeroignore"), "*.bak\n").unwrap();
    let rules = plan(
        &config,
        Some(empty_index(&config)),
        &changes(root.path(), &[".aeroignore", "a.txt"], &[]),
        EXACT,
    )
    .unwrap();
    assert!(matches!(
        rules,
        IncrementalPrep::Work {
            full_scan_wanted: Some(_),
            ..
        }
    ));

    // A tree larger than the pass reads on disk.
    std::fs::create_dir(root.path().join("tree")).unwrap();
    for i in 0..=LOCAL_EXPANSION_LIMIT {
        std::fs::write(root.path().join(format!("tree/{i}")), b"x").unwrap();
    }
    assert!(matches!(
        plan(
            &config,
            Some(empty_index(&config)),
            &changes(root.path(), &["tree"], &[]),
            EXACT
        )
        .unwrap(),
        IncrementalPrep::FullScanOnly(_)
    ));

    let gone = pair(&root.path().join("missing"));
    assert!(plan(&gone, Some(empty_index(&gone)), &paths, EXACT).is_err());
}

/// A provider that forwards every call to `inner` and counts the listings.
struct CountingLists {
    inner: Box<dyn StorageProvider>,
    lists: Arc<Mutex<usize>>,
}

#[async_trait::async_trait]
impl StorageProvider for CountingLists {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
    fn provider_type(&self) -> ProviderType {
        self.inner.provider_type()
    }
    fn display_name(&self) -> String {
        self.inner.display_name()
    }
    fn mtime_precision(&self) -> Option<Duration> {
        self.inner.mtime_precision()
    }
    fn reports_exact_size(&self) -> bool {
        self.inner.reports_exact_size()
    }
    fn listing_is_authoritative(&self) -> bool {
        self.inner.listing_is_authoritative()
    }
    fn min_full_scan_interval(&self) -> Option<Duration> {
        self.inner.min_full_scan_interval()
    }
    fn manages_upload_retries(&self) -> bool {
        self.inner.manages_upload_retries()
    }
    async fn connect(&mut self) -> Result<(), ProviderError> {
        self.inner.connect().await
    }
    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.inner.disconnect().await
    }
    fn is_connected(&self) -> bool {
        self.inner.is_connected()
    }
    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        *self.lists.lock().unwrap() += 1;
        self.inner.list(path).await
    }
    async fn pwd(&mut self) -> Result<String, ProviderError> {
        self.inner.pwd().await
    }
    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        self.inner.cd(path).await
    }
    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        self.inner.cd_up().await
    }
    async fn download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        self.inner.download(remote_path, local_path, progress).await
    }
    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        self.inner.download_to_bytes(remote_path).await
    }
    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        self.inner.upload(local_path, remote_path, progress).await
    }
    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        self.inner.mkdir(path).await
    }
    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        self.inner.delete(path).await
    }
    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        self.inner.rmdir(path).await
    }
    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        self.inner.rmdir_recursive(path).await
    }
    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        self.inner.rename(from, to).await
    }
    async fn prepare_file_move(&mut self, path: &str) -> Result<(), ProviderError> {
        self.inner.prepare_file_move(path).await
    }
    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        self.inner.stat(path).await
    }
    async fn size(&mut self, path: &str) -> Result<u64, ProviderError> {
        self.inner.size(path).await
    }
    async fn exists(&mut self, path: &str) -> Result<bool, ProviderError> {
        self.inner.exists(path).await
    }
    async fn keep_alive(&mut self) -> Result<(), ProviderError> {
        self.inner.keep_alive().await
    }
    async fn server_info(&mut self) -> Result<String, ProviderError> {
        self.inner.server_info().await
    }
}

/// Live: an incremental pass against a real saved profile, through the
/// pair's real overlay stack. A full sync sets the baseline in a fresh
/// remote test folder, then a new file, an edit, a rename and a delete are
/// synced by the incremental pass, which must list nothing. The remote
/// folder and the baseline are removed at the end, on failure too.
///
/// `AEROFTP_LIVE_AEROCLOUD_PROFILES` names the profiles (comma separated).
/// Reads the vault the binary reads: the release one with
/// `cargo test --config 'profile.test.package.aeroftp.debug-assertions=false'
/// --lib live_incremental_pass -- --ignored --nocapture`.
#[tokio::test]
#[ignore = "live: writes and removes a test folder on each named saved profile"]
async fn live_incremental_pass_on_saved_profiles() {
    use crate::credential_store::CredentialStore;

    let Ok(names) = std::env::var("AEROFTP_LIVE_AEROCLOUD_PROFILES") else {
        eprintln!("AEROFTP_LIVE_AEROCLOUD_PROFILES is not set: nothing to check");
        return;
    };
    assert_eq!(CredentialStore::init().expect("open the vault"), "OK");
    let store = CredentialStore::from_cache().expect("the vault is open");
    let profiles = crate::user_partitions::mcp_list_active_server_profiles(&store)
        .expect("list the saved profiles");
    let mut failures = Vec::new();
    for name in names.split(',').map(str::trim).filter(|n| !n.is_empty()) {
        let profile = profiles
            .iter()
            .find(|p| p.get("name").and_then(|v| v.as_str()) == Some(name))
            .unwrap_or_else(|| panic!("no saved profile named {name}"));
        let base = profile
            .get("aeroCryptOverlay")
            .filter(|o| o.get("enabled").and_then(|v| v.as_bool()) == Some(true))
            .and_then(|o| o.get("remoteScope"))
            .or_else(|| profile.get("initialPath"))
            .and_then(|v| v.as_str())
            .map(|s| s.trim().trim_end_matches('/').to_string())
            .unwrap_or_default();
        let stamp = Utc::now().format("%Y%m%d-%H%M%S");
        let local = tempfile::tempdir().unwrap();
        let config = CloudConfig {
            enabled: true,
            server_profile: name.to_string(),
            server_profile_id: profile
                .get("id")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            protocol_type: profile
                .get("protocol")
                .and_then(|v| v.as_str())
                .unwrap_or("ftp")
                .to_string(),
            connection_params: serde_json::json!({}),
            local_folder: local.path().to_path_buf(),
            remote_folder: format!("{base}/aerocloud-incremental-live-{stamp}"),
            exclude_patterns: Vec::new(),
            sync_direction: CompareDirection::Bidirectional,
            ..CloudConfig::default()
        };
        let outcome = live_round(&config, &store).await;
        // Clean up whatever the round left, whatever its outcome.
        if let Ok(mut provider) = open_aerocloud_provider(&config, Some(&store)).await {
            if let Err(e) = provider.rmdir_recursive(&config.remote_folder).await {
                eprintln!("{name}: could not remove {}: {e}", config.remote_folder);
            }
            let _ = provider.disconnect().await;
        }
        crate::sync::remove_sync_index(
            &config.local_folder.to_string_lossy(),
            &config.remote_folder,
        );
        match outcome {
            Ok(summary) => eprintln!("OK    {name}: {summary}"),
            Err(e) => {
                eprintln!("FAIL  {name}: {e}");
                failures.push(format!("{name}: {e}"));
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

async fn live_round(
    config: &CloudConfig,
    store: &crate::credential_store::CredentialStore,
) -> Result<String, String> {
    let root = config.local_folder.clone();
    std::fs::create_dir(root.join("dir")).unwrap();
    std::fs::write(root.join("edit.txt"), b"first version").unwrap();
    std::fs::write(root.join("keep.txt"), b"renamed later").unwrap();
    std::fs::write(root.join("dir/gone.txt"), b"deleted later").unwrap();

    // The baseline: a full sync, as the scheduler runs it.
    let full = crate::cloud_service::run_pair_full_cycle(config.clone(), Some(store), None)
        .await
        .map_err(|e| e.message)?;
    if !full.result.errors.is_empty() {
        return Err(format!("full sync: {:?}", full.result.errors));
    }

    // Local changes, as the watcher reports them. A second passes so a
    // same-size edit moves the time.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    std::fs::write(root.join("new.txt"), b"brand new").unwrap();
    std::fs::write(root.join("edit.txt"), b"second version, longer").unwrap();
    std::fs::rename(root.join("keep.txt"), root.join("kept.txt")).unwrap();
    std::fs::remove_file(root.join("dir/gone.txt")).unwrap();
    let changes = changes(
        &root,
        &[
            "new.txt",
            "edit.txt",
            "keep.txt",
            "kept.txt",
            "dir/gone.txt",
        ],
        &[("keep.txt", "kept.txt")],
    );
    let traits = full.traits.expect("the full sync connected");
    let hints = IncrementalHints {
        compare_size: traits.exact_sizes,
    };
    let plan = match prepare_incremental(config, &changes, hints)? {
        IncrementalPrep::Work { plan, .. } => plan,
        IncrementalPrep::Settled { .. } => return Err("every change settled locally".into()),
        IncrementalPrep::FullScanOnly(reason) => return Err(format!("full scan only: {reason}")),
    };
    let planned = format!(
        "{} path(s) and {} rename(s) for the remote",
        plan.work.len(),
        plan.renames.len()
    );

    let lists = Arc::new(Mutex::new(0usize));
    let mut provider = CountingLists {
        inner: open_aerocloud_provider(config, Some(store)).await?,
        lists: lists.clone(),
    };
    let svc = CloudService::new();
    svc.init(config.clone()).await;
    let run = svc
        .perform_incremental_sync_with_provider(&mut provider, plan, None)
        .await?;
    let (result, failed) = match run {
        IncrementalRun::Done(result, failed) => (result, failed),
        IncrementalRun::TooLarge(_) => return Err("the pass stopped at its limit".into()),
    };
    let listed_during_pass = *lists.lock().unwrap();
    if !result.errors.is_empty() || !failed.is_empty() {
        return Err(format!("incremental pass: {:?}", result.errors));
    }
    if listed_during_pass != 0 {
        return Err(format!("the pass listed {listed_during_pass} folder(s)"));
    }

    // What the remote holds now, read by a listing after the measured pass.
    let mut seen = BTreeMap::new();
    for folder in [
        config.remote_folder.clone(),
        format!("{}/dir", config.remote_folder),
    ] {
        for entry in provider
            .inner
            .list(&folder)
            .await
            .map_err(|e| e.to_string())?
        {
            if !entry.is_dir {
                let key = format!("{}/{}", folder, entry.name);
                seen.insert(key, entry.size);
            }
        }
    }
    let _ = provider.disconnect().await;
    let at = |key: &str| format!("{}/{key}", config.remote_folder);
    for (key, len) in [("new.txt", 9u64), ("edit.txt", 22), ("kept.txt", 13)] {
        match seen.get(&at(key)) {
            Some(size) if !traits.exact_sizes || *size == len => {}
            other => {
                return Err(format!(
                    "{key}: expected {len} bytes, the remote has {other:?}"
                ))
            }
        }
    }
    for key in ["keep.txt", "dir/gone.txt"] {
        if seen.contains_key(&at(key)) {
            return Err(format!("{key} is still on the remote"));
        }
    }
    Ok(format!(
        "{planned}; uploaded {}, deleted {}, 0 listings during the pass",
        result.uploaded, result.deleted
    ))
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_new_unicode_parent_twin_does_not_borrow_the_existing_directory() {
    let root = tempfile::tempdir().unwrap();
    let config = pair(root.path());
    let mut remote = MemRemote::new();
    let mut index = empty_index(&config);
    synced(
        &mut index,
        &remote,
        root.path(),
        "café/old.txt",
        b"original",
    );
    remote.put("/remote/café/new.txt", b"remote independent", SYNCED_AT);
    let new = "cafe\u{301}/new.txt";
    std::fs::create_dir_all(root.path().join("cafe\u{301}")).unwrap();
    std::fs::write(root.path().join(new), b"new sibling bytes").unwrap();
    let (run, _) = execute(
        &config,
        work(prep(&config, &index, &[new])),
        &mut remote,
        None,
    )
    .await;
    let (result, _) = done(run);
    assert_eq!(result.skipped, 1);
    assert_eq!(result.uploaded, 0);
    assert_eq!(
        remote.state().files["/remote/café/new.txt"].0,
        b"remote independent"
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_new_twin_of_a_mapped_implicit_parent_waits_for_the_full_scan() {
    let root = tempfile::tempdir().unwrap();
    let config = pair(root.path());
    let mut remote = MemRemote::new();
    let mut index = empty_index(&config);
    let tracked = "cafe\u{301}/old.txt";
    synced(&mut index, &remote, root.path(), tracked, b"original");
    remote.state().files.remove(&format!("/remote/{tracked}"));
    remote.put("/remote/café/old.txt", b"original", SYNCED_AT);
    index
        .remote_spellings
        .insert(tracked.into(), "café/old.txt".into());
    remote.put("/remote/café/new.txt", b"remote independent", SYNCED_AT);
    std::fs::create_dir_all(root.path().join("café")).unwrap();
    std::fs::write(root.path().join("café/new.txt"), b"new sibling bytes").unwrap();
    let (run, _) = execute(
        &config,
        work(prep(&config, &index, &["café/new.txt"])),
        &mut remote,
        None,
    )
    .await;
    let (result, _) = done(run);
    assert_eq!(result.skipped, 1);
    assert_eq!(result.uploaded, 0);
    assert_eq!(
        remote.state().files["/remote/café/new.txt"].0,
        b"remote independent"
    );
}
