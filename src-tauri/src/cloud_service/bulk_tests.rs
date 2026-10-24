// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Tests of AeroCloud's bulk path: a remote in memory with a native bulk,
//! comparisons built by hand, and the loop's call (`next_outcome`) per
//! comparison, so nothing is saved in the data folder.

use super::*;
use crate::providers::bulk::{BulkProgress, BulkTransferItem};
use crate::providers::{ProviderError, ProviderType};
use crate::sync::{CompareDirection, FileInfo, SyncStatus};
use chrono::{DateTime, Utc};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// The time the remote gives every file it stores.
const SERVER_TIME: &str = "2026-10-24T06:00:00Z";

#[derive(Default)]
struct Remote {
    files: BTreeMap<String, Vec<u8>>,
    bulk_uploads: Vec<Vec<String>>,
    bulk_downloads: Vec<Vec<String>>,
    /// `up <path>` and `down <path>` of the single-file calls.
    singles: Vec<String>,
    stats: usize,
    lists: Vec<String>,
    /// Names the bulk answers as uncertain (and does not store).
    uncertain: HashSet<String>,
    /// A local file rewritten while the bulk sends it.
    edited_while_sent: Option<PathBuf>,
    /// Names whose listed size is this instead of the length stored.
    listed_size: HashMap<String, u64>,
    /// Names listed with this size claimed by their uploader.
    claimed: HashMap<String, u64>,
}

#[derive(Clone)]
struct BulkMemory {
    remote: Arc<Mutex<Remote>>,
    kind: ProviderType,
    bulk: bool,
}

impl BulkMemory {
    fn new(bulk: bool) -> Self {
        Self {
            remote: Arc::default(),
            kind: ProviderType::Proton,
            bulk,
        }
    }

    fn remote(&self) -> std::sync::MutexGuard<'_, Remote> {
        self.remote.lock().unwrap()
    }

    fn entry(remote: &Remote, path: &str) -> Option<RemoteEntry> {
        let content = remote.files.get(path)?;
        let name = path.rsplit('/').next().unwrap_or_default().to_string();
        let mut metadata = HashMap::new();
        if let Some(claimed) = remote.claimed.get(&name) {
            metadata.insert("claimed_size".to_string(), claimed.to_string());
        }
        Some(RemoteEntry {
            size: remote
                .listed_size
                .get(&name)
                .copied()
                .unwrap_or(content.len() as u64),
            name,
            path: path.to_string(),
            is_dir: false,
            modified: Some(SERVER_TIME.to_string()),
            permissions: None,
            owner: None,
            group: None,
            is_symlink: false,
            link_target: None,
            mime_type: None,
            metadata,
        })
    }

    fn bulk(&self, items: &[BulkTransferItem], upload: bool) -> Vec<BulkTransferOutcome> {
        let mut remote = self.remote();
        let paths = items.iter().map(|i| i.remote_path.clone()).collect();
        if upload {
            remote.bulk_uploads.push(paths);
        } else {
            remote.bulk_downloads.push(paths);
        }
        let outcomes = items
            .iter()
            .map(|item| {
                let name = item.remote_path.rsplit('/').next().unwrap_or_default();
                if remote.uncertain.contains(name) {
                    return BulkTransferOutcome::Uncertain("the program stopped".into());
                }
                if upload {
                    let bytes = std::fs::read(&item.local_path).unwrap();
                    remote.files.insert(item.remote_path.clone(), bytes);
                } else {
                    let bytes = remote.files[&item.remote_path].clone();
                    std::fs::write(&item.local_path, bytes).unwrap();
                }
                BulkTransferOutcome::Completed
            })
            .collect();
        if let Some(path) = remote.edited_while_sent.take() {
            std::fs::write(path, b"edited while it went up").unwrap();
        }
        outcomes
    }
}

#[async_trait::async_trait]
impl StorageProvider for BulkMemory {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
    fn provider_type(&self) -> ProviderType {
        self.kind
    }
    fn display_name(&self) -> String {
        "bulk memory".into()
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
        let mut remote = self.remote();
        remote.lists.push(path.to_string());
        let prefix = format!("{}/", path.trim_end_matches('/'));
        Ok(remote
            .files
            .keys()
            .filter(|p| {
                p.strip_prefix(&prefix)
                    .is_some_and(|rest| !rest.contains('/'))
            })
            .filter_map(|p| Self::entry(&remote, p))
            .collect())
    }
    async fn pwd(&mut self) -> Result<String, ProviderError> {
        Ok("/".into())
    }
    async fn cd(&mut self, _: &str) -> Result<(), ProviderError> {
        Ok(())
    }
    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        Ok(())
    }
    async fn download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        _: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let mut remote = self.remote();
        remote.singles.push(format!("down {remote_path}"));
        let bytes = remote.files[remote_path].clone();
        std::fs::write(local_path, bytes).map_err(|e| ProviderError::Other(e.to_string()))
    }
    async fn download_to_bytes(&mut self, path: &str) -> Result<Vec<u8>, ProviderError> {
        Ok(self.remote().files[path].clone())
    }
    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        _: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let mut remote = self.remote();
        remote.singles.push(format!("up {remote_path}"));
        let bytes = std::fs::read(local_path).map_err(|e| ProviderError::Other(e.to_string()))?;
        remote.files.insert(remote_path.to_string(), bytes);
        Ok(())
    }
    async fn mkdir(&mut self, _: &str) -> Result<(), ProviderError> {
        Ok(())
    }
    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        self.remote().files.remove(path);
        Ok(())
    }
    async fn rmdir(&mut self, _: &str) -> Result<(), ProviderError> {
        Ok(())
    }
    async fn rmdir_recursive(&mut self, _: &str) -> Result<(), ProviderError> {
        Ok(())
    }
    async fn rename(&mut self, _: &str, _: &str) -> Result<(), ProviderError> {
        Ok(())
    }
    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        let mut remote = self.remote();
        remote.stats += 1;
        Self::entry(&remote, path).ok_or_else(|| ProviderError::NotFound(path.into()))
    }
    async fn size(&mut self, path: &str) -> Result<u64, ProviderError> {
        Ok(self.remote().files.get(path).map_or(0, |b| b.len() as u64))
    }
    async fn exists(&mut self, path: &str) -> Result<bool, ProviderError> {
        Ok(self.remote().files.contains_key(path))
    }
    async fn keep_alive(&mut self) -> Result<(), ProviderError> {
        Ok(())
    }
    async fn server_info(&mut self) -> Result<String, ProviderError> {
        Ok(String::new())
    }
    fn supports_bulk_upload(&self) -> bool {
        self.bulk
    }
    fn supports_bulk_download(&self) -> bool {
        self.bulk
    }
    async fn upload_bulk(
        &mut self,
        items: &[BulkTransferItem],
        _: Option<BulkProgress>,
    ) -> Result<Vec<BulkTransferOutcome>, ProviderError> {
        Ok(self.bulk(items, true))
    }
    async fn download_bulk(
        &mut self,
        items: &[BulkTransferItem],
        _: Option<BulkProgress>,
    ) -> Result<Vec<BulkTransferOutcome>, ProviderError> {
        Ok(self.bulk(items, false))
    }
}

fn config(local: &Path) -> CloudConfig {
    CloudConfig {
        enabled: true,
        local_folder: local.to_path_buf(),
        remote_folder: "/remote".into(),
        sync_direction: CompareDirection::Bidirectional,
        ..Default::default()
    }
}

fn info(path: String, size: u64) -> FileInfo {
    FileInfo {
        name: path.rsplit('/').next().unwrap_or_default().to_string(),
        path,
        size,
        modified: Some(Utc::now()),
        is_dir: false,
        checksum: None,
        checksum_alg: None,
    }
}

fn comparison(relative_path: &str, status: SyncStatus) -> FileComparison {
    FileComparison {
        baseline_key: None,
        local_relative_path: None,
        remote_relative_path: None,
        name_match_blocked: false,
        relative_path: relative_path.to_string(),
        status,
        local_info: None,
        remote_info: None,
        is_dir: false,
        sync_reason: String::new(),
        previously_synced: false,
    }
}

/// `count` new local files under `docs/`, each an upload.
fn new_local_files(local: &Path, count: usize) -> Vec<FileComparison> {
    std::fs::create_dir_all(local.join("docs")).unwrap();
    (0..count)
        .map(|i| {
            let rel = format!("docs/f{i}.txt");
            let path = local.join(&rel);
            std::fs::write(&path, format!("file {i}")).unwrap();
            let mut c = comparison(&rel, SyncStatus::LocalOnly);
            c.local_info = Some(info(
                path.to_string_lossy().to_string(),
                format!("file {i}").len() as u64,
            ));
            c
        })
        .collect()
}

async fn cycle(
    provider: &mut BulkMemory,
    config: &CloudConfig,
    comparisons: &[FileComparison],
) -> (CloudService, Vec<Result<SyncAction, String>>) {
    let svc = CloudService::new();
    let mut ahead = BulkAhead::default();
    let mut outcomes = Vec::new();
    for index in 0..comparisons.len() {
        outcomes.push(
            svc.next_outcome(provider, config, comparisons, index, &mut ahead)
                .await,
        );
    }
    (svc, outcomes)
}

fn mtime(path: &Path) -> DateTime<Utc> {
    DateTime::<Utc>::from(std::fs::metadata(path).unwrap().modified().unwrap())
}

fn server_time() -> DateTime<Utc> {
    SERVER_TIME.parse().unwrap()
}

/// Each upload of a cycle started the official program twice on Proton
/// Drive in CLI mode: one upload, one `stat`. Two hundred files now take one
/// call and one listing of their folder, and each is stamped with the time
/// the server gave it, as after its own `stat`.
#[tokio::test]
async fn two_hundred_uploads_take_one_call_and_one_listing() {
    let local = tempfile::tempdir().unwrap();
    let comparisons = new_local_files(local.path(), 200);
    let mut provider = BulkMemory::new(true);
    let (svc, outcomes) = cycle(&mut provider, &config(local.path()), &comparisons).await;
    assert!(outcomes.iter().all(|o| matches!(o, Ok(SyncAction::Upload))));
    let remote = provider.remote();
    assert_eq!(remote.bulk_uploads.len(), 1);
    assert_eq!(remote.bulk_uploads[0].len(), 200);
    assert_eq!((remote.singles.len(), remote.stats), (0, 0));
    assert_eq!(remote.lists, ["/remote/docs"]);
    assert_eq!(mtime(&local.path().join("docs/f7.txt")), server_time());
    assert_eq!(svc.landed.lock().unwrap().len(), 200);
}

/// A file rewritten while the bulk sent it goes again on its own, through
/// the single-file path, which settles it as its own sends.
#[tokio::test]
async fn a_file_edited_while_the_bulk_sent_it_goes_again_alone() {
    let local = tempfile::tempdir().unwrap();
    let comparisons = new_local_files(local.path(), 3);
    let mut provider = BulkMemory::new(true);
    provider.remote().edited_while_sent = Some(local.path().join("docs/f1.txt"));
    let (_, outcomes) = cycle(&mut provider, &config(local.path()), &comparisons).await;
    assert!(outcomes.iter().all(|o| matches!(o, Ok(SyncAction::Upload))));
    let remote = provider.remote();
    assert_eq!(remote.singles, ["up /remote/docs/f1.txt"]);
    assert_eq!(
        remote.files["/remote/docs/f1.txt"],
        b"edited while it went up"
    );
}

/// A file the program may have sent is not sent again in the cycle, and
/// nothing landed is noted for it: the next cycle compares it again.
#[tokio::test]
async fn a_file_that_may_have_reached_the_server_is_left_to_the_next_cycle() {
    let local = tempfile::tempdir().unwrap();
    let comparisons = new_local_files(local.path(), 3);
    let before = mtime(&local.path().join("docs/f2.txt"));
    let mut provider = BulkMemory::new(true);
    provider.remote().uncertain.insert("f2.txt".into());
    let (svc, outcomes) = cycle(&mut provider, &config(local.path()), &comparisons).await;
    assert!(matches!(&outcomes[2], Err(e) if e.contains("the program stopped")));
    assert!(provider.remote().singles.is_empty());
    assert!(!svc.landed.lock().unwrap().contains_key("docs/f2.txt"));
    assert_eq!(mtime(&local.path().join("docs/f2.txt")), before);
}

#[tokio::test]
async fn a_provider_without_the_bulk_stays_on_the_single_file_path() {
    let local = tempfile::tempdir().unwrap();
    let comparisons = new_local_files(local.path(), 3);
    let mut provider = BulkMemory::new(false);
    let (_, outcomes) = cycle(&mut provider, &config(local.path()), &comparisons).await;
    assert!(outcomes.iter().all(|o| matches!(o, Ok(SyncAction::Upload))));
    let remote = provider.remote();
    assert!(remote.bulk_uploads.is_empty());
    assert_eq!((remote.singles.len(), remote.stats), (3, 3));
}

/// A folder between two runs of uploads keeps its place: the files before
/// it go first (a lone one on its own), the folder, then the rest together.
#[tokio::test]
async fn a_folder_between_uploads_keeps_its_place() {
    let local = tempfile::tempdir().unwrap();
    let mut comparisons = new_local_files(local.path(), 3);
    let mut folder = comparison("docs/sub", SyncStatus::LocalOnly);
    folder.is_dir = true;
    std::fs::create_dir_all(local.path().join("docs/sub")).unwrap();
    folder.local_info = Some(FileInfo {
        is_dir: true,
        ..info(
            local.path().join("docs/sub").to_string_lossy().to_string(),
            0,
        )
    });
    comparisons.insert(1, folder);
    let mut provider = BulkMemory::new(true);
    let (_, outcomes) = cycle(&mut provider, &config(local.path()), &comparisons).await;
    assert!(outcomes.iter().all(|o| o.is_ok()), "{outcomes:?}");
    let remote = provider.remote();
    assert_eq!(remote.singles, ["up /remote/docs/f0.txt"]);
    assert_eq!(
        remote.bulk_uploads,
        [vec!["/remote/docs/f1.txt", "/remote/docs/f2.txt"]]
    );
}

/// Downloads of a run keep the copy each one replaces first, as on the
/// single-file path, and give each local file the remote's time.
#[tokio::test]
async fn downloads_keep_the_replaced_copy_first_and_take_the_remote_time() {
    let local = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(local.path().join("docs")).unwrap();
    std::fs::write(local.path().join("docs/f0.txt"), b"old local copy").unwrap();
    let mut provider = BulkMemory::new(true);
    let remote_time = Utc::now() - chrono::Duration::days(3);
    let comparisons: Vec<_> = (0..3)
        .map(|i| {
            let rel = format!("docs/f{i}.txt");
            provider
                .remote()
                .files
                .insert(format!("/remote/{rel}"), format!("remote {i}").into_bytes());
            let mut c = comparison(&rel, SyncStatus::RemoteOnly);
            let mut remote = info(format!("/remote/{rel}"), 8);
            remote.modified = Some(remote_time);
            c.remote_info = Some(remote);
            c
        })
        .collect();
    let mut config = config(local.path());
    config.versioning_strategy =
        crate::sync_versioning::VersioningStrategy::Simple { max_copies: 5 };
    let (svc, outcomes) = cycle(&mut provider, &config, &comparisons).await;
    assert!(outcomes
        .iter()
        .all(|o| matches!(o, Ok(SyncAction::Download))));
    assert_eq!(provider.remote().bulk_downloads.len(), 1);
    assert!(provider.remote().singles.is_empty());
    assert_eq!(
        std::fs::read(local.path().join("docs/f0.txt")).unwrap(),
        b"remote 0"
    );
    let kept: Vec<_> = walkdir::WalkDir::new(local.path())
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file() && e.path().to_string_lossy().contains("f0~"))
        .collect();
    assert_eq!(kept.len(), 1, "the replaced copy is kept once");
    assert_eq!(std::fs::read(kept[0].path()).unwrap(), b"old local copy");
    assert_eq!(
        mtime(&local.path().join("docs/f2.txt")).timestamp(),
        remote_time.timestamp()
    );
    assert_eq!(svc.landed.lock().unwrap().len(), 3);
}

/// Proton Drive lists the stored, encrypted size of a file whose uploader
/// claimed none: a listing (after a bulk) or a `stat` (after one upload)
/// that shows only that size says nothing of what was sent, and the file
/// is stamped with the server's time as usual. A claimed size that differs
/// does say another client wrote it: the file keeps the time it was sent.
#[tokio::test]
async fn only_a_claimed_size_tells_a_proton_file_from_what_was_sent() {
    for bulk in [true, false] {
        for claimed in [false, true] {
            let local = tempfile::tempdir().unwrap();
            let comparisons = new_local_files(local.path(), 2);
            let sent_time = mtime(&local.path().join("docs/f0.txt"));
            let mut provider = BulkMemory::new(bulk);
            if claimed {
                provider.remote().claimed.insert("f0.txt".into(), 9999);
            } else {
                provider.remote().listed_size.insert("f0.txt".into(), 9999);
            }
            let (svc, outcomes) = cycle(&mut provider, &config(local.path()), &comparisons).await;
            assert!(outcomes.iter().all(|o| o.is_ok()));
            let stamped = mtime(&local.path().join("docs/f0.txt"));
            let landed = svc.landed.lock().unwrap()["docs/f0.txt"];
            if claimed {
                assert_eq!(
                    stamped, sent_time,
                    "bulk {bulk}: a different claimed size is another write"
                );
                assert_eq!(landed.modified, sent_time);
            } else {
                assert_eq!(
                    stamped,
                    server_time(),
                    "bulk {bulk}: a storage size is no evidence"
                );
                assert_eq!(landed.modified, server_time());
            }
        }
    }
}
