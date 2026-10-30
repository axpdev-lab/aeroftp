// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! AeroCloud on a provider's native bulk (Requested 77). On Proton Drive in
//! CLI mode a cycle uploaded and stat'ed each file through its own run of the
//! official program. The uploads (and the downloads) of a cycle that come in
//! a row now travel in one native call per folder, and one listing per folder
//! replaces the `stat` of each uploaded file. Each file then ends as it does
//! on the single-file path ([`CloudService::settle_upload`], the remote time
//! of a download); a file the bulk did not take, or one edited while it went
//! up, goes through that path itself.

use super::{CloudService, LocalFileState, UpdatePolicy};
use crate::cloud_config::CloudConfig;
use crate::providers::bulk::{goes_back_to_single_file, BulkTransferOutcome, MAX_BULK_FILES};
use crate::providers::{RemoteEntry, StorageProvider};
use crate::sync::{FileComparison, SyncAction};
use crate::transfer_bulk::{bulk_item, group_key, plan_groups_by_key};
use crate::transfer_domain::TransferEntry;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// The outcomes the bulk settled ahead of a cycle's loop, by the index of the
/// comparison, and the comparisons it looked at and left to the single-file
/// path.
#[derive(Default)]
pub(super) struct BulkAhead {
    update_policy: UpdatePolicy,
    settled: HashMap<usize, Result<SyncAction, String>>,
    passed: HashSet<usize>,
}

impl BulkAhead {
    pub(super) fn new(update_policy: UpdatePolicy) -> Self {
        Self {
            update_policy,
            ..Self::default()
        }
    }
}

/// A file of an upload group, as read right before it went.
struct Upload {
    index: usize,
    local_path: PathBuf,
    remote_path: String,
    sent: Option<LocalFileState>,
}

impl CloudService {
    /// The outcome of `comparisons[index]` for a cycle's loop, in plan order:
    /// what the bulk settled for it, else the single-file path. A run of
    /// uploads or downloads that starts here goes through the bulk first.
    pub(super) async fn next_outcome<P: StorageProvider + ?Sized>(
        &self,
        provider: &mut P,
        config: &CloudConfig,
        comparisons: &[FileComparison],
        index: usize,
        ahead: &mut BulkAhead,
    ) -> Result<SyncAction, String> {
        if let Some(outcome) = ahead.settled.remove(&index) {
            return outcome;
        }
        if !ahead.passed.contains(&index) {
            if let Some(upload) =
                self.bulk_way(provider, config, &comparisons[index], ahead.update_policy)
            {
                self.run_bulk(provider, config, comparisons, index, upload, ahead)
                    .await;
                if let Some(outcome) = ahead.settled.remove(&index) {
                    return outcome;
                }
            }
        }
        self.process_comparison_with_provider(
            provider,
            config,
            &comparisons[index],
            ahead.update_policy,
        )
        .await
    }

    /// `Some(true)` for a file upload the provider's bulk can carry,
    /// `Some(false)` for a file download, `None` for everything else: folders,
    /// deletes, a conflict, a path spelled differently on the two sides, an
    /// overlay (which has no bulk).
    fn bulk_way<P: StorageProvider + ?Sized>(
        &self,
        provider: &P,
        config: &CloudConfig,
        comparison: &FileComparison,
        policy: UpdatePolicy,
    ) -> Option<bool> {
        if comparison.is_dir
            || crate::sync::validate_relative_path(&comparison.relative_path).is_err()
            || comparison
                .local_relative_path
                .as_ref()
                .is_some_and(|p| *p != comparison.relative_path)
            || comparison
                .remote_relative_path
                .as_ref()
                .is_some_and(|p| *p != comparison.relative_path)
        {
            return None;
        }
        let (upload, download) =
            crate::transfer_bulk::bulk_directions(&provider.transfer_capabilities());
        match self.resolve_action(config, comparison, policy) {
            SyncAction::Upload if upload && comparison.local_info.is_some() => Some(true),
            SyncAction::Download if download && comparison.remote_info.is_some() => Some(false),
            _ => None,
        }
    }

    fn remote_path_of(config: &CloudConfig, relative_path: &str) -> String {
        format!(
            "{}/{}",
            config.remote_folder.trim_end_matches('/'),
            relative_path
        )
    }

    /// Send the run of files from `start` that go the same way, at most
    /// [`MAX_BULK_FILES`], and settle what can be settled.
    async fn run_bulk<P: StorageProvider + ?Sized>(
        &self,
        provider: &mut P,
        config: &CloudConfig,
        comparisons: &[FileComparison],
        start: usize,
        upload: bool,
        ahead: &mut BulkAhead,
    ) {
        let run: Vec<usize> = (start..comparisons.len())
            .take(MAX_BULK_FILES)
            .take_while(|&i| {
                self.bulk_way(provider, config, &comparisons[i], ahead.update_policy)
                    == Some(upload)
            })
            .collect();
        let entries: Vec<TransferEntry> = run
            .iter()
            .map(|&i| {
                let comparison = &comparisons[i];
                let (local, remote, size) = if upload {
                    let local = comparison.local_info.as_ref().expect("checked by bulk_way");
                    (
                        local.path.clone(),
                        Self::remote_path_of(config, &comparison.relative_path),
                        local.size,
                    )
                } else {
                    let remote = comparison
                        .remote_info
                        .as_ref()
                        .expect("checked by bulk_way");
                    (
                        config
                            .local_folder
                            .join(&comparison.relative_path)
                            .to_string_lossy()
                            .to_string(),
                        remote.path.clone(),
                        remote.size,
                    )
                };
                TransferEntry {
                    id: i.to_string(),
                    display_name: comparison.relative_path.clone(),
                    remote_path: remote,
                    local_path: local,
                    size,
                    modified: None,
                }
            })
            .collect();
        for group in plan_groups_by_key(&entries, |entry| group_key(entry, upload)) {
            let members: Vec<usize> = group.iter().map(|&k| run[k]).collect();
            if members.len() < 2 {
                ahead.passed.extend(members);
                continue;
            }
            let members: Vec<(usize, &TransferEntry)> =
                group.iter().map(|&k| (run[k], &entries[k])).collect();
            if upload {
                self.bulk_upload(provider, comparisons, &members, ahead)
                    .await;
            } else {
                self.bulk_download(provider, config, comparisons, &members, ahead)
                    .await;
            }
        }
        ahead.passed.insert(start);
    }

    async fn bulk_upload<P: StorageProvider + ?Sized>(
        &self,
        provider: &mut P,
        comparisons: &[FileComparison],
        members: &[(usize, &TransferEntry)],
        ahead: &mut BulkAhead,
    ) {
        // One group shares one remote folder: made once if it is missing.
        let parent = members[0]
            .1
            .remote_path
            .rsplit_once('/')
            .map(|(parent, _)| parent.to_string())
            .unwrap_or_default();
        if !parent.is_empty() && provider.cd(&parent).await.is_err() {
            crate::sync::ensure_remote_dir(provider, &parent).await;
        }
        // One bound for the group, as for the sends of one file.
        let quiet_deadline = tokio::time::Instant::now() + self.send_quiet_bound;
        let mut uploads = Vec::with_capacity(members.len());
        for &(index, entry) in members {
            let local_path = PathBuf::from(&entry.local_path);
            Self::wait_until_quiet(&local_path, self.send_quiet_window, quiet_deadline).await;
            uploads.push(Upload {
                index,
                sent: Self::local_file_state(&local_path),
                local_path,
                remote_path: entry.remote_path.clone(),
            });
        }
        let items: Vec<_> = uploads
            .iter()
            .map(|u| {
                bulk_item(
                    &u.local_path.to_string_lossy(),
                    &u.remote_path,
                    u.sent.map_or(0, |s| s.len),
                )
            })
            .collect();
        tracing::info!(
            "AeroCloud: uploading {} files to '{}' in one call",
            items.len(),
            parent
        );
        let outcomes = match provider.upload_bulk(&items, None).await {
            Ok(outcomes) if outcomes.len() == items.len() => outcomes,
            // Refused before any file went: each takes the single-file path.
            Ok(_) | Err(_) => {
                ahead.passed.extend(uploads.iter().map(|u| u.index));
                return;
            }
        };
        // One listing of the folder instead of one `stat` per file.
        let listed: HashMap<String, RemoteEntry> = provider
            .list(&parent)
            .await
            .map(|entries| entries.into_iter().map(|e| (e.name.clone(), e)).collect())
            .unwrap_or_default();
        let exact_sizes = provider
            .reports_exact_size()
            .then(|| provider.provider_type());
        for (upload, outcome) in uploads.iter().zip(outcomes) {
            let comparison = &comparisons[upload.index];
            if goes_back_to_single_file(&outcome) {
                ahead.passed.insert(upload.index);
                continue;
            }
            match outcome {
                BulkTransferOutcome::Completed => {}
                BulkTransferOutcome::Failed(error) => {
                    ahead
                        .settled
                        .insert(upload.index, Err(format!("Upload failed: {error}")));
                    continue;
                }
                // It may have reached the server: never sent again in this
                // cycle, and nothing landed is noted, so the next cycle
                // compares the file again.
                BulkTransferOutcome::Uncertain(message) => {
                    ahead
                        .settled
                        .insert(upload.index, Err(format!("Upload failed: {message}")));
                    continue;
                }
                BulkTransferOutcome::NotAttempted(_) => {
                    ahead.passed.insert(upload.index);
                    continue;
                }
            }
            // Changed while it went up (or unreadable before): the single-file
            // path sends it again and settles it as it does its own sends.
            let Some(sent) = upload
                .sent
                .filter(|sent| Self::local_file_state(&upload.local_path).as_ref() == Some(sent))
            else {
                tracing::info!(
                    "AeroCloud: '{}' changed while it uploaded; sending it again",
                    comparison.relative_path
                );
                ahead.passed.insert(upload.index);
                continue;
            };
            let name = upload.remote_path.rsplit('/').next().unwrap_or_default();
            self.settle_upload(
                &comparison.relative_path,
                &upload.local_path,
                sent,
                listed.get(name),
                exact_sizes,
            );
            ahead.settled.insert(upload.index, Ok(SyncAction::Upload));
        }
    }

    async fn bulk_download<P: StorageProvider + ?Sized>(
        &self,
        provider: &mut P,
        config: &CloudConfig,
        comparisons: &[FileComparison],
        members: &[(usize, &TransferEntry)],
        ahead: &mut BulkAhead,
    ) {
        let versioning = crate::sync_versioning::SyncVersioning::new(
            &config.local_folder,
            config.versioning_strategy.clone(),
        );
        for (_, entry) in members {
            let local_path = Path::new(&entry.local_path);
            // The copy a download replaces is kept first, as on the
            // single-file path; a file that then falls back to a single
            // download is not kept twice (see below).
            if local_path.exists() && versioning.is_enabled() {
                if let Err(e) = versioning.archive(local_path) {
                    tracing::warn!("Versioning archive failed: {}", e);
                }
            }
            if let Some(parent) = local_path.parent() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    tracing::warn!("Failed to create directory {}: {}", parent.display(), e);
                }
            }
        }
        let items: Vec<_> = members
            .iter()
            .map(|(_, entry)| bulk_item(&entry.local_path, &entry.remote_path, entry.size))
            .collect();
        let outcomes = match provider.download_bulk(&items, None).await {
            Ok(outcomes) if outcomes.len() == items.len() => outcomes,
            _ => items
                .iter()
                .map(|_| BulkTransferOutcome::NotAttempted("refused".into()))
                .collect(),
        };
        for ((index, entry), outcome) in members.iter().zip(outcomes) {
            let comparison = &comparisons[*index];
            let result = match outcome {
                BulkTransferOutcome::Completed => Ok(()),
                // Nothing of it was written: one download on its own, the
                // single-file path's one attempt, its copy already kept.
                other if goes_back_to_single_file(&other) => provider
                    .download(&entry.remote_path, &entry.local_path, None)
                    .await
                    .map_err(|e| e.to_string()),
                BulkTransferOutcome::Failed(error) => Err(error.to_string()),
                BulkTransferOutcome::Uncertain(message)
                | BulkTransferOutcome::NotAttempted(message) => Err(message),
            };
            let outcome = match result {
                Ok(()) => {
                    if let Some(mtime) = comparison.remote_info.as_ref().and_then(|r| r.modified) {
                        crate::preserve_remote_mtime_dt(Path::new(&entry.local_path), Some(mtime));
                    }
                    self.note_landed(config, &comparison.relative_path);
                    Ok(SyncAction::Download)
                }
                Err(message) => Err(format!("Download failed: {message}")),
            };
            ahead.settled.insert(*index, outcome);
        }
    }
}

#[cfg(test)]
#[path = "bulk_tests.rs"]
mod tests;
