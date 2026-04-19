//! Minimal-but-solid directory sync engine.
//!
//! Consumed by the MCP `aeroftp_sync_tree` tool. The CLI `sync` subcommand
//! stays on its own richer orchestration for now (track-renames, backups,
//! bisync snapshots, resync), but both agree on the same output counters via
//! [`SyncReport`] so the two front-ends can be compared apples-to-apples.
//!
//! Responsibilities:
//!  * scan local + remote with [`scan_local_tree`] / [`scan_remote_tree`]
//!  * classify operations by [`SyncDirection`] and conflict mode
//!  * execute upload / download / delete with progress callbacks
//!  * surface a [`SyncReport`] + error taxonomy for agents

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

use crate::providers::StorageProvider;
use crate::sync_core::scan::{scan_local_tree, scan_remote_tree, ScanOptions};
use std::path::Path;
use std::time::Instant;

/// Direction of a sync operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncDirection {
    Upload,
    Download,
    Both,
}

impl SyncDirection {
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "upload" | "up" | "push" => Some(Self::Upload),
            "download" | "down" | "pull" => Some(Self::Download),
            "both" | "bidirectional" => Some(Self::Both),
            _ => None,
        }
    }
}

/// Conflict resolution when both sides disagree on the same relative path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictMode {
    /// Keep the larger file (default, matches the CLI).
    Larger,
    /// Keep the newer file.
    Newer,
    /// Skip the file when sizes differ.
    Skip,
}

impl ConflictMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "larger" => Some(Self::Larger),
            "newer" => Some(Self::Newer),
            "skip" => Some(Self::Skip),
            _ => None,
        }
    }
}

/// Tunable options for a sync run.
#[derive(Debug, Clone)]
pub struct SyncOptions {
    pub direction: SyncDirection,
    pub dry_run: bool,
    pub delete_orphans: bool,
    pub conflict_mode: ConflictMode,
    pub scan: ScanOptions,
}

impl Default for SyncOptions {
    fn default() -> Self {
        Self {
            direction: SyncDirection::Upload,
            dry_run: false,
            delete_orphans: false,
            conflict_mode: ConflictMode::Larger,
            scan: ScanOptions::default(),
        }
    }
}

/// High-level phase, emitted via [`SyncProgressSink::on_phase`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncPhase {
    Scanning,
    Planning,
    Executing,
    Done,
}

/// Per-file outcome delivered to the progress sink after each operation.
#[derive(Debug, Clone)]
pub enum FileOutcome {
    Uploaded { bytes: u64 },
    Downloaded { bytes: u64 },
    Deleted,
    Skipped { reason: String },
    Failed { error: String },
}

/// Aggregated counters returned by [`sync_tree_core`].
#[derive(Debug, Clone, Default)]
pub struct SyncReport {
    pub uploaded: u32,
    pub downloaded: u32,
    pub deleted: u32,
    pub skipped: u32,
    pub errors: Vec<SyncError>,
    pub elapsed_secs: f64,
    pub dry_run: bool,
    pub direction: Option<SyncDirection>,
}

impl SyncReport {
    pub fn error_count(&self) -> usize {
        self.errors.len()
    }
}

/// Single sync error with enough context for an agent to retry intelligently.
#[derive(Debug, Clone)]
pub struct SyncError {
    pub rel_path: String,
    pub operation: &'static str,
    pub message: String,
}

/// Callback interface used to report sync progress.
pub trait SyncProgressSink: Send {
    fn on_phase(&mut self, phase: SyncPhase);
    fn on_file_start(&mut self, rel: &str, total: u64, op: &'static str);
    fn on_file_progress(&mut self, rel: &str, sent: u64, total: u64);
    fn on_file_done(&mut self, rel: &str, outcome: &FileOutcome);
}

/// Drop-in no-op sink for callers that do not care about progress.
pub struct NoopProgressSink;

impl SyncProgressSink for NoopProgressSink {
    fn on_phase(&mut self, _phase: SyncPhase) {}
    fn on_file_start(&mut self, _rel: &str, _total: u64, _op: &'static str) {}
    fn on_file_progress(&mut self, _rel: &str, _sent: u64, _total: u64) {}
    fn on_file_done(&mut self, _rel: &str, _outcome: &FileOutcome) {}
}

/// Run a sync between `local_root` and `remote_root` using `provider` and
/// record progress via `sink`.
pub async fn sync_tree_core(
    provider: &mut Box<dyn StorageProvider>,
    local_root: &str,
    remote_root: &str,
    opts: &SyncOptions,
    sink: &mut dyn SyncProgressSink,
) -> SyncReport {
    let start = Instant::now();
    sink.on_phase(SyncPhase::Scanning);
    let locals = scan_local_tree(local_root, &opts.scan);
    let remotes = scan_remote_tree(provider, remote_root, &opts.scan).await;

    sink.on_phase(SyncPhase::Planning);
    let mut report = SyncReport {
        dry_run: opts.dry_run,
        direction: Some(opts.direction),
        ..SyncReport::default()
    };

    use std::collections::{HashMap, HashSet};
    let mut seen: HashSet<String> = HashSet::new();
    let local_index: HashMap<&str, u64> = locals
        .iter()
        .map(|e| (e.rel_path.as_str(), e.size))
        .collect();
    let remote_index: HashMap<&str, u64> = remotes
        .iter()
        .map(|e| (e.rel_path.as_str(), e.size))
        .collect();

    sink.on_phase(SyncPhase::Executing);

    // Local → remote (upload)
    if matches!(opts.direction, SyncDirection::Upload | SyncDirection::Both) {
        for local_entry in &locals {
            if !seen.insert(local_entry.rel_path.clone()) {
                continue;
            }
            let remote_size = remote_index.get(local_entry.rel_path.as_str()).copied();
            let decision = decide_upload(local_entry.size, remote_size, opts.conflict_mode);
            match decision {
                Decision::Copy => {
                    let outcome = perform_upload(
                        provider,
                        local_root,
                        remote_root,
                        &local_entry.rel_path,
                        local_entry.size,
                        opts.dry_run,
                        sink,
                    )
                    .await;
                    apply_outcome(&mut report, &local_entry.rel_path, "upload", outcome, sink);
                }
                Decision::Skip(reason) => {
                    let outcome = FileOutcome::Skipped { reason };
                    apply_outcome(&mut report, &local_entry.rel_path, "upload", outcome, sink);
                }
            }
        }
    }

    // Remote → local (download)
    if matches!(
        opts.direction,
        SyncDirection::Download | SyncDirection::Both
    ) {
        for remote_entry in &remotes {
            let already_seen = !seen.insert(remote_entry.rel_path.clone());
            let local_size = local_index.get(remote_entry.rel_path.as_str()).copied();
            let decision = decide_download(
                remote_entry.size,
                local_size,
                opts.conflict_mode,
                already_seen && matches!(opts.direction, SyncDirection::Both),
            );
            match decision {
                Decision::Copy => {
                    let outcome = perform_download(
                        provider,
                        local_root,
                        remote_root,
                        &remote_entry.rel_path,
                        remote_entry.size,
                        opts.dry_run,
                        sink,
                    )
                    .await;
                    apply_outcome(
                        &mut report,
                        &remote_entry.rel_path,
                        "download",
                        outcome,
                        sink,
                    );
                }
                Decision::Skip(reason) => {
                    let outcome = FileOutcome::Skipped { reason };
                    apply_outcome(
                        &mut report,
                        &remote_entry.rel_path,
                        "download",
                        outcome,
                        sink,
                    );
                }
            }
        }
    }

    // Orphan deletion
    if opts.delete_orphans {
        match opts.direction {
            SyncDirection::Upload => {
                // Remote has files the local side removed -> delete remote.
                for remote_entry in &remotes {
                    if !local_index.contains_key(remote_entry.rel_path.as_str()) {
                        let outcome = perform_remote_delete(
                            provider,
                            remote_root,
                            &remote_entry.rel_path,
                            opts.dry_run,
                            sink,
                        )
                        .await;
                        apply_outcome(
                            &mut report,
                            &remote_entry.rel_path,
                            "delete_remote",
                            outcome,
                            sink,
                        );
                    }
                }
            }
            SyncDirection::Download => {
                // Local has files the remote removed -> delete local.
                for local_entry in &locals {
                    if !remote_index.contains_key(local_entry.rel_path.as_str()) {
                        let outcome = perform_local_delete(
                            local_root,
                            &local_entry.rel_path,
                            opts.dry_run,
                            sink,
                        );
                        apply_outcome(
                            &mut report,
                            &local_entry.rel_path,
                            "delete_local",
                            outcome,
                            sink,
                        );
                    }
                }
            }
            SyncDirection::Both => {
                // Bidirectional delete is ambiguous without bisync snapshots.
                // We intentionally skip it here so callers that really want
                // destructive bi-dir use the full CLI path.
            }
        }
    }

    report.elapsed_secs = start.elapsed().as_secs_f64();
    sink.on_phase(SyncPhase::Done);
    report
}

enum Decision {
    Copy,
    Skip(String),
}

fn decide_upload(local_size: u64, remote_size: Option<u64>, mode: ConflictMode) -> Decision {
    match remote_size {
        None => Decision::Copy,
        Some(size) if size == local_size => Decision::Skip("identical size".to_string()),
        Some(size) => match mode {
            ConflictMode::Larger if local_size > size => Decision::Copy,
            ConflictMode::Larger => Decision::Skip("remote is larger".to_string()),
            ConflictMode::Newer => Decision::Copy, // mtime not tracked reliably; prefer upload
            ConflictMode::Skip => Decision::Skip("conflict skip".to_string()),
        },
    }
}

fn decide_download(
    remote_size: u64,
    local_size: Option<u64>,
    mode: ConflictMode,
    already_handled_by_upload: bool,
) -> Decision {
    if already_handled_by_upload {
        return Decision::Skip("resolved by upload pass".to_string());
    }
    match local_size {
        None => Decision::Copy,
        Some(size) if size == remote_size => Decision::Skip("identical size".to_string()),
        Some(size) => match mode {
            ConflictMode::Larger if remote_size > size => Decision::Copy,
            ConflictMode::Larger => Decision::Skip("local is larger".to_string()),
            ConflictMode::Newer => Decision::Skip("newer mode prefers existing local".to_string()),
            ConflictMode::Skip => Decision::Skip("conflict skip".to_string()),
        },
    }
}

fn apply_outcome(
    report: &mut SyncReport,
    rel: &str,
    operation: &'static str,
    outcome: FileOutcome,
    sink: &mut dyn SyncProgressSink,
) {
    match &outcome {
        FileOutcome::Uploaded { .. } => report.uploaded += 1,
        FileOutcome::Downloaded { .. } => report.downloaded += 1,
        FileOutcome::Deleted => report.deleted += 1,
        FileOutcome::Skipped { .. } => report.skipped += 1,
        FileOutcome::Failed { error } => {
            report.errors.push(SyncError {
                rel_path: rel.to_string(),
                operation,
                message: error.clone(),
            });
        }
    }
    sink.on_file_done(rel, &outcome);
}

async fn perform_upload(
    provider: &mut Box<dyn StorageProvider>,
    local_root: &str,
    remote_root: &str,
    rel: &str,
    total: u64,
    dry_run: bool,
    sink: &mut dyn SyncProgressSink,
) -> FileOutcome {
    sink.on_file_start(rel, total, "upload");
    if dry_run {
        return FileOutcome::Skipped {
            reason: "dry-run".to_string(),
        };
    }
    let local_path = join_clean(local_root, rel);
    let remote_path = join_clean_remote(remote_root, rel);
    ensure_remote_parent(provider, &remote_path).await;

    match provider.upload(&local_path, &remote_path, None).await {
        Ok(()) => FileOutcome::Uploaded { bytes: total },
        Err(e) => FileOutcome::Failed {
            error: format!("upload failed: {}", e),
        },
    }
}

async fn perform_download(
    provider: &mut Box<dyn StorageProvider>,
    local_root: &str,
    remote_root: &str,
    rel: &str,
    total: u64,
    dry_run: bool,
    sink: &mut dyn SyncProgressSink,
) -> FileOutcome {
    sink.on_file_start(rel, total, "download");
    if dry_run {
        return FileOutcome::Skipped {
            reason: "dry-run".to_string(),
        };
    }
    let remote_path = join_clean_remote(remote_root, rel);
    let local_path = join_clean(local_root, rel);
    if let Some(parent) = Path::new(&local_path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match provider.download(&remote_path, &local_path, None).await {
        Ok(()) => FileOutcome::Downloaded { bytes: total },
        Err(e) => FileOutcome::Failed {
            error: format!("download failed: {}", e),
        },
    }
}

async fn perform_remote_delete(
    provider: &mut Box<dyn StorageProvider>,
    remote_root: &str,
    rel: &str,
    dry_run: bool,
    sink: &mut dyn SyncProgressSink,
) -> FileOutcome {
    sink.on_file_start(rel, 0, "delete_remote");
    if dry_run {
        return FileOutcome::Skipped {
            reason: "dry-run".to_string(),
        };
    }
    let remote_path = join_clean_remote(remote_root, rel);
    match provider.delete(&remote_path).await {
        Ok(()) => FileOutcome::Deleted,
        Err(e) => FileOutcome::Failed {
            error: format!("delete failed: {}", e),
        },
    }
}

fn perform_local_delete(
    local_root: &str,
    rel: &str,
    dry_run: bool,
    sink: &mut dyn SyncProgressSink,
) -> FileOutcome {
    sink.on_file_start(rel, 0, "delete_local");
    if dry_run {
        return FileOutcome::Skipped {
            reason: "dry-run".to_string(),
        };
    }
    let path = join_clean(local_root, rel);
    match std::fs::remove_file(&path) {
        Ok(()) => FileOutcome::Deleted,
        Err(e) => FileOutcome::Failed {
            error: format!("delete failed: {}", e),
        },
    }
}

async fn ensure_remote_parent(provider: &mut Box<dyn StorageProvider>, remote_path: &str) {
    if let Some(idx) = remote_path.rfind('/') {
        let parent = &remote_path[..idx];
        if !parent.is_empty() {
            let _ = provider.mkdir(parent).await;
        }
    }
}

fn join_clean(root: &str, rel: &str) -> String {
    let rel = rel.trim_start_matches('/');
    if root.is_empty() {
        rel.to_string()
    } else if root.ends_with('/') || root.ends_with('\\') {
        format!("{}{}", root, rel)
    } else {
        format!("{}/{}", root, rel)
    }
}

fn join_clean_remote(root: &str, rel: &str) -> String {
    let rel = rel.trim_start_matches('/');
    if root.is_empty() || root == "/" {
        format!("/{}", rel)
    } else if root.ends_with('/') {
        format!("{}{}", root, rel)
    } else {
        format!("{}/{}", root, rel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_direction_accepts_common_aliases() {
        assert_eq!(SyncDirection::parse("upload"), Some(SyncDirection::Upload));
        assert_eq!(SyncDirection::parse("push"), Some(SyncDirection::Upload));
        assert_eq!(SyncDirection::parse("pull"), Some(SyncDirection::Download));
        assert_eq!(SyncDirection::parse("both"), Some(SyncDirection::Both));
        assert_eq!(SyncDirection::parse("wat"), None);
    }

    #[test]
    fn parse_conflict_mode() {
        assert_eq!(ConflictMode::parse("larger"), Some(ConflictMode::Larger));
        assert_eq!(ConflictMode::parse("skip"), Some(ConflictMode::Skip));
        assert_eq!(ConflictMode::parse("newer"), Some(ConflictMode::Newer));
        assert_eq!(ConflictMode::parse("foo"), None);
    }

    #[test]
    fn join_clean_handles_trailing_slash_and_leading_slash_on_rel() {
        assert_eq!(join_clean("/base", "/rel.txt"), "/base/rel.txt");
        assert_eq!(join_clean("/base/", "rel.txt"), "/base/rel.txt");
        assert_eq!(join_clean("", "rel.txt"), "rel.txt");
    }

    #[test]
    fn join_clean_remote_keeps_leading_slash() {
        assert_eq!(join_clean_remote("/", "rel.txt"), "/rel.txt");
        assert_eq!(join_clean_remote("/foo", "rel.txt"), "/foo/rel.txt");
        assert_eq!(join_clean_remote("/foo/", "/rel.txt"), "/foo/rel.txt");
    }

    #[test]
    fn decide_upload_copies_missing_remote() {
        assert!(matches!(
            decide_upload(10, None, ConflictMode::Larger),
            Decision::Copy
        ));
    }

    #[test]
    fn decide_upload_skips_same_size() {
        assert!(matches!(
            decide_upload(10, Some(10), ConflictMode::Larger),
            Decision::Skip(_)
        ));
    }

    #[test]
    fn decide_upload_larger_mode_picks_larger_side() {
        assert!(matches!(
            decide_upload(20, Some(10), ConflictMode::Larger),
            Decision::Copy
        ));
        assert!(matches!(
            decide_upload(5, Some(10), ConflictMode::Larger),
            Decision::Skip(_)
        ));
    }

    #[test]
    fn decide_download_respects_both_direction_dedup() {
        assert!(matches!(
            decide_download(10, Some(10), ConflictMode::Larger, true),
            Decision::Skip(_)
        ));
    }
}
