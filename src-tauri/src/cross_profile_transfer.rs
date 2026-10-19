// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Cross-profile transfer engine: MVP backend.
//!
//! Copies files between two remote profiles using a local temp-file bridge.
//! No destructive operations (no delete, no move, no sync).

use crate::delta_sync_rsync::{try_delta_transfer, SyncDirection};
use crate::providers::{ProviderError, StorageProvider};
use crate::sync_core::{refused_transfer, walk_path_key, WalkScope, WalkStep};
use filetime::FileTime;
use serde::{Deserialize, Serialize};
use std::path::Path;
use tempfile::NamedTempFile;
use tokio_util::sync::CancellationToken;

/// Maximum BFS scan depth: consistent with CLI's MAX_SCAN_DEPTH.
const MAX_SCAN_DEPTH: usize = 100;
/// Maximum entries to collect: consistent with CLI's MAX_SCAN_ENTRIES.
const MAX_SCAN_ENTRIES: usize = 500_000;

// ── Request / Plan / Result structs ────────────────────────────────────────

/// Describes a cross-profile transfer request coming from CLI or GUI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossProfileTransferRequest {
    pub source_profile: String,
    pub dest_profile: String,
    pub source_path: String,
    pub dest_path: String,
    pub recursive: bool,
    pub dry_run: bool,
    pub skip_existing: bool,
}

/// A single entry in the transfer plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossProfileTransferEntry {
    pub source_path: String,
    pub dest_path: String,
    pub display_name: String,
    pub size: u64,
    pub modified: Option<String>,
    pub is_dir: bool,
}

/// The full transfer plan built before execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossProfileTransferPlan {
    pub source_profile: String,
    pub dest_profile: String,
    pub entries: Vec<CrossProfileTransferEntry>,
    #[serde(default)]
    pub directories: Vec<CrossProfileTransferEntry>,
    pub total_files: u64,
    pub total_bytes: u64,
}

/// Summary returned after executing a transfer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossProfileTransferResult {
    pub planned_files: u64,
    pub transferred_files: u64,
    pub skipped_files: u64,
    pub failed_files: u64,
    pub total_bytes: u64,
    pub duration_ms: u64,
}

/// One planned file that did not transfer. `source_path` and `dest_path` are
/// empty when the runner can only say that the tally does not add up.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CrossProfileFileFailure {
    pub source_path: String,
    pub dest_path: String,
    pub error: String,
}

#[derive(Debug, Clone)]
pub struct CrossProfileCopyOptions {
    pub source_size: Option<u64>,
    pub download_segments: crate::transfer_settings::DownloadSegmentsRequest,
    pub cancel_token: CancellationToken,
}

impl Default for CrossProfileCopyOptions {
    fn default() -> Self {
        Self {
            source_size: None,
            download_segments: crate::transfer_settings::DownloadSegmentsRequest::Single {
                reason: "standalone copy has no source-size hint for segmented download"
                    .to_string(),
            },
            cancel_token: CancellationToken::new(),
        }
    }
}

// ── Core: single-file copy ─────────────────────────────────────────────────

/// Copy a single file from `source` to `dest` using a local temp-file bridge.
///
/// Flow: source.download() -> temp file -> dest.upload()
///
/// The temp file is automatically cleaned up when `NamedTempFile` is dropped.
/// Parent directories on the destination are created if missing.
///
/// When `dest` is an SFTP provider with key-based auth and a remote rsync
/// helper, the upload step is replaced by AeroRsync delta transfer: only
/// the bytes that differ from any pre-existing file at `dest_path` go on the
/// wire. Hard errors (host-key mismatch, protocol invariant violation) are
/// propagated unchanged; soft fallbacks ("file too small", "no key on disk")
/// transparently route back to the classic upload.
pub async fn copy_one_file(
    source: &mut dyn StorageProvider,
    dest: &mut dyn StorageProvider,
    source_path: &str,
    dest_path: &str,
    source_modified: Option<&str>,
) -> Result<(), ProviderError> {
    copy_one_file_with_options(
        source,
        dest,
        source_path,
        dest_path,
        source_modified,
        CrossProfileCopyOptions::default(),
    )
    .await
}

/// Copy a single file with transfer-phase options used by the GUI DAG path.
///
/// Keeps the same temp-file bridge and delta upload behavior as
/// [`copy_one_file`], while allowing the source download leg to opt into the
/// shared segmented helper when the provider honestly supports it.
pub async fn copy_one_file_with_options(
    source: &mut dyn StorageProvider,
    dest: &mut dyn StorageProvider,
    source_path: &str,
    dest_path: &str,
    source_modified: Option<&str>,
    options: CrossProfileCopyOptions,
) -> Result<(), ProviderError> {
    // Create a temp path that auto-deletes on drop. `into_temp_path` closes
    // the file handle first, so the segmented helper can atomically rename
    // over the path on every platform that supports the provider path.
    let tmp = NamedTempFile::new()
        .map_err(|e| ProviderError::TransferFailed(format!("temp file creation failed: {e}")))?;
    let tmp = tmp.into_temp_path();
    let tmp_path = tmp.to_string_lossy().to_string();

    download_source_to_temp(source, source_path, &tmp_path, &options).await?;

    preserve_temp_mtime(&tmp, source_modified);

    // Ensure every ancestor exists. One mkdir of the immediate parent fails
    // when that parent is itself missing (SFTP, FTPS, Filen, Koofr).
    check_copy_cancel(&options.cancel_token)?;
    ensure_parent_dir(dest, dest_path, &options.cancel_token).await?;

    // Try delta transfer first (SFTP-only today). Returns None for non-SFTP
    // destinations or when downcast/probe declines: in both cases we proceed
    // to the classic upload below.
    check_copy_cancel(&options.cancel_token)?;
    if let Some(result) =
        try_delta_transfer(dest, SyncDirection::Upload, tmp.as_ref(), dest_path).await
    {
        if result.used_delta {
            // Delta path completed the transfer; confirm the file is there.
            return confirm_destination_has_file(dest, dest_path).await;
        }
        if let Some(err) = result.hard_error {
            // Hard error must surface: never silently retry the classic path
            // when the delta layer rejected for a security/protocol reason.
            return Err(ProviderError::TransferFailed(format!(
                "delta transfer rejected: {err}"
            )));
        }
        // fallback_reason set: declined gracefully, fall through to classic.
    }

    // Classic upload: from temp file to destination
    check_copy_cancel(&options.cancel_token)?;
    dest.upload(&tmp_path, dest_path, None).await?;
    confirm_destination_has_file(dest, dest_path).await
}

/// Refuse to start a new destination operation after Stop.
fn check_copy_cancel(cancel: &CancellationToken) -> Result<(), ProviderError> {
    if cancel.is_cancelled() {
        Err(ProviderError::TransferFailed(
            "Transfer cancelled by user".to_string(),
        ))
    } else {
        Ok(())
    }
}

async fn download_source_to_temp(
    source: &mut dyn StorageProvider,
    source_path: &str,
    tmp_path: &str,
    options: &CrossProfileCopyOptions,
) -> Result<(), ProviderError> {
    if options.cancel_token.is_cancelled() {
        return Err(ProviderError::TransferFailed(
            "Transfer cancelled by user".to_string(),
        ));
    }

    let resolved = crate::transfer_settings::resolve_download_segments(
        &options.download_segments,
        Some(crate::transfer_settings::download_segments_preference_for(
            source.provider_type(),
        )),
    );
    tracing::info!(
        "cross-profile source download streams requested: {}",
        resolved
    );
    if resolved.count() > 1 {
        if let Some(source_size) = options.source_size {
            if let Some(segments) =
                crate::provider_transfer_executor::provider_segmented_download_eligible(
                    source,
                    source_size,
                    resolved.count(),
                    resolved.count() as usize,
                    crate::provider_transfer_executor::SegmentCutoff::Default,
                )
            {
                tracing::info!(
                    "cross-profile: segmented source download (remote={}, bytes={}, segments={})",
                    source_path,
                    source_size,
                    segments
                );
                match crate::provider_transfer_executor::run_provider_segmented_download(
                    source,
                    source_path,
                    tmp_path,
                    source_size,
                    segments,
                    None,
                    options.cancel_token.clone(),
                )
                .await
                {
                    // This is a staging file, not the destination commit: Stop
                    // must still prevent the subsequent upload.
                    Ok(()) if options.cancel_token.is_cancelled() => {
                        return Err(ProviderError::TransferFailed(
                            "Transfer cancelled by user".to_string(),
                        ));
                    }
                    Ok(()) => return Ok(()),
                    Err(err) if err.is_cancelled() => {
                        return Err(ProviderError::TransferFailed(err.to_string()));
                    }
                    Err(err) => {
                        err.report_fallback();
                        tracing::warn!(
                            "cross-profile: segmented source download failed, falling back to single-stream (remote={}, error={})",
                            source_path,
                            err
                        );
                    }
                }
            }
        }
    }

    check_copy_cancel(&options.cancel_token)?;
    source.download(source_path, tmp_path, None).await
}

// ── Planning: collect + filter + plan ──────────────────────────────────────

/// Recursively collect source entries using BFS, respecting depth and entry limits.
///
/// If `recursive` is false and the path is a directory, returns an error.
/// If `recursive` is true, performs a bounded BFS scan.
/// If the path is a file, returns a single entry.
///
/// The scan lists a folder only when [`WalkScope`] allows it, and plans each
/// file once ([`collect_tree`]): it used to queue every folder at the path its
/// listing gave, so a folder listed inside itself or as `/` planned the same
/// files again, or the account root's, and a folder outside the source was
/// copied as part of it.
pub async fn collect_source_entries(
    source: &mut dyn StorageProvider,
    root: &str,
    recursive: bool,
) -> Result<Vec<CrossProfileTransferEntry>, ProviderError> {
    Ok(collect_source_tree(source, root, recursive)
        .await?
        .into_iter()
        .filter(|entry| !entry.is_dir)
        .collect())
}

async fn collect_source_tree(
    source: &mut dyn StorageProvider,
    root: &str,
    recursive: bool,
) -> Result<Vec<CrossProfileTransferEntry>, ProviderError> {
    if is_virtual_root_path(root) {
        return collect_virtual_root_entries(source, root, recursive).await;
    }

    // Check if root is a file (stat it). A prefix-only directory stats as
    // missing and is recovered from its listing.
    let root_stat = stat_for_plan(source, root).await?;

    if !root_stat.is_dir {
        // Single file
        return Ok(vec![CrossProfileTransferEntry {
            source_path: root.to_string(),
            dest_path: String::new(), // filled by plan_transfer
            display_name: fallback_display_name(&root_stat.name, &root_stat.path),
            size: root_stat.size,
            modified: root_stat.modified.clone(),
            is_dir: false,
        }]);
    }

    // It's a directory: recursive must be true
    if !recursive {
        return Err(ProviderError::InvalidPath(format!(
            "'{}' is a directory; use --recursive to transfer directories",
            root
        )));
    }

    collect_tree(source, &root_stat.path).await
}

async fn collect_virtual_root_entries(
    source: &mut dyn StorageProvider,
    root: &str,
    recursive: bool,
) -> Result<Vec<CrossProfileTransferEntry>, ProviderError> {
    if !recursive {
        return Err(ProviderError::InvalidPath(format!(
            "'{}' is a directory; use --recursive to transfer directories",
            root
        )));
    }

    collect_tree(source, root).await
}

/// The files under the folder `root`, for a recursive copy: a bounded
/// depth-first walk shared by a folder and the virtual root. Each folder is
/// listed only when [`WalkScope`] allows it (strictly under `root`, not the
/// folder listed or above it, not listed already), the root's spelling
/// learned from its own listing, and each file is planned once. A folder the
/// walk does not follow fails the plan: skipping it would copy part of the
/// folder and report success, following it would copy what is not in it.
async fn collect_tree(
    source: &mut dyn StorageProvider,
    root: &str,
) -> Result<Vec<CrossProfileTransferEntry>, ProviderError> {
    let mut entries = Vec::new();
    let mut scope = WalkScope::new(root);
    let mut planned = std::collections::HashSet::new();
    let mut queue: Vec<(String, usize)> = vec![(root.to_string(), 0)];

    while let Some((dir, depth)) = queue.pop() {
        if depth >= MAX_SCAN_DEPTH {
            return Err(ProviderError::TransferFailed(format!(
                "cannot plan the complete tree: depth limit reached at {dir}"
            )));
        }
        // An unreadable folder is not an empty folder and cannot produce a
        // successful partial plan.
        let listing = source.list(&dir).await?;
        if depth == 0 {
            scope.learn_root_listing(&listing);
        }

        for e in listing {
            if entries.len() >= MAX_SCAN_ENTRIES {
                return Err(ProviderError::TransferFailed(
                    "cannot plan the complete tree: entry limit reached".into(),
                ));
            }
            match scope.place(&dir, &e.path, e.is_dir) {
                WalkStep::Follow => {}
                WalkStep::AlreadyListed => continue,
                step => {
                    return Err(ProviderError::ServerError(refused_transfer(
                        root, &dir, &e.path, step,
                    )))
                }
            }
            if e.is_dir {
                queue.push((e.path.clone(), depth + 1));
            }
            if planned.insert(walk_path_key(&e.path)) {
                let display_name = fallback_display_name(&e.name, &e.path);
                entries.push(CrossProfileTransferEntry {
                    source_path: e.path,
                    dest_path: String::new(), // filled by plan_transfer
                    display_name,
                    size: e.size,
                    modified: e.modified,
                    is_dir: e.is_dir,
                });
            }
        }
    }

    Ok(entries)
}

/// Check whether a file should be skipped because it already exists on the
/// destination with matching size and a time not older than the source,
/// within the shared modification window. This is the one-way sync rule:
/// an upload-time-only backend must not copy the same bytes on every run.
/// Missing or incomparable times fall back to size, as in one-way sync.
///
/// Returns `Ok(true)` to skip, `Ok(false)` to transfer, or propagates errors
/// (except NotFound, which means "not present → transfer").
pub async fn should_skip_existing(
    source_precision: Option<std::time::Duration>,
    dest: &mut dyn StorageProvider,
    dest_path: &str,
    source_entry: &CrossProfileTransferEntry,
) -> Result<bool, ProviderError> {
    match dest.stat(dest_path).await {
        Ok(existing) => {
            let window = crate::sync_core::mtime::ModifyWindow::resolve(
                None,
                source_precision,
                dest.mtime_precision(),
                crate::sync_core::mtime::SizeOnlyReason::for_provider(dest.provider_type()),
            );
            Ok(!existing.is_dir
                && !existing.is_symlink
                && existing.size == source_entry.size
                && !matches!(
                    window.order_text(
                        source_entry.modified.as_deref(),
                        existing.modified.as_deref()
                    ),
                    Some(std::cmp::Ordering::Greater)
                ))
        }
        Err(ProviderError::NotFound(_)) => Ok(false),
        Err(err) => Err(err),
    }
}

/// Build a transfer plan by collecting source entries and computing destination paths.
///
/// This function does NOT execute any transfer and does NOT filter skip-existing
/// (filtering happens at execution time so the caller can track skip counts).
/// The full plan is always returned, suitable for both dry-run display and execution.
pub async fn plan_transfer(
    source: &mut dyn StorageProvider,
    _dest: &mut dyn StorageProvider,
    request: &CrossProfileTransferRequest,
) -> Result<CrossProfileTransferPlan, ProviderError> {
    let source_path = resolved_source_path(source, &request.source_path).await?;
    let root_stat = effective_root_stat(source, &source_path).await?;

    // 1. Collect source entries
    let collected = collect_source_tree(source, &source_path, request.recursive).await?;
    let (mut directories, mut entries): (Vec<_>, Vec<_>) =
        collected.into_iter().partition(|entry| entry.is_dir);

    // 2. Compute destination paths
    if !root_stat.is_dir {
        // Single file: dest_path is the request's dest_path directly
        entries[0].dest_path = request.dest_path.clone();
    } else {
        // Recursive: strip source root and append to dest_path.
        // Some providers return canonical paths that differ from the caller input,
        // so we accept both the request path and the provider-reported root path.
        let source_roots = source_root_candidates(&source_path, &root_stat.path);
        for entry in &mut entries {
            entry.dest_path = map_dest_path(&source_roots, &entry.source_path, &request.dest_path);
        }
        for directory in &mut directories {
            directory.dest_path =
                map_dest_path(&source_roots, &directory.source_path, &request.dest_path);
        }
        // Even an entirely empty source needs its destination root.
        directories.insert(
            0,
            CrossProfileTransferEntry {
                source_path: source_path.clone(),
                dest_path: request.dest_path.clone(),
                display_name: fallback_display_name(&root_stat.name, &root_stat.path),
                size: 0,
                modified: root_stat.modified.clone(),
                is_dir: true,
            },
        );
    }

    if plan_includes_raw_crypt_marker(&entries) {
        return Err(ProviderError::InvalidConfig(
            "overlay not applied: the source plan contains the AeroCrypt marker .aerocrypt.tsv, so this transfer would copy ciphertext. Connect the source through its crypt overlay.".into(),
        ));
    }

    let total_files = entries.len() as u64;
    let total_bytes = entries.iter().map(|e| e.size).sum();

    Ok(CrossProfileTransferPlan {
        source_profile: request.source_profile.clone(),
        dest_profile: request.dest_profile.clone(),
        entries,
        directories,
        total_files,
        total_bytes,
    })
}

// ── Helpers ────────────────────────────────────────────────────────────────

/// Materialize the directory part of an approved plan before copying files.
/// Any failed mkdir aborts execution, including a tree containing only empty
/// directories. Cancellation is checked before each remote mutation.
pub async fn ensure_planned_directories(
    dest: &mut dyn StorageProvider,
    directories: &[CrossProfileTransferEntry],
    cancelled: impl Fn() -> bool + Send + Sync,
) -> Result<(), ProviderError> {
    let check_cancel = || {
        if cancelled() {
            Err(ProviderError::TransferFailed(
                "Transfer cancelled by user".into(),
            ))
        } else {
            Ok(())
        }
    };
    // Precreating encrypted folders would bypass upload's phantom-parent
    // guard. Check the entire directory plan before creating any of it.
    if let Some(overlay) = dest
        .as_any_mut()
        .downcast_mut::<crate::crypt_overlay_provider::CryptOverlayProvider>()
    {
        let mut checked = std::collections::HashSet::new();
        for directory in directories {
            check_cancel()?;
            overlay
                .check_directory_creation(&directory.dest_path, &mut checked)
                .await?;
        }
    }
    let mut created = std::collections::HashSet::new();
    for directory in directories {
        for ancestor in crate::community_benchmark::benchmark_mkdir_ladder(&directory.dest_path) {
            check_cancel()?;
            if !created.insert(ancestor.clone()) {
                continue;
            }
            match dest.mkdir(&ancestor).await {
                Ok(()) => {}
                // Existing ancestors can return generic or permission errors
                // on mkdir. Only a successful stat proving a directory permits
                // execution to continue; preserve the mkdir failure otherwise.
                Err(error) => match dest.stat(&ancestor).await {
                    Ok(entry) if entry.is_dir => {}
                    Ok(_) => return Err(ProviderError::AlreadyExists(ancestor)),
                    Err(_) => {
                        return Err(ProviderError::TransferFailed(format!(
                            "cannot create destination directory {ancestor}: {error}"
                        )))
                    }
                },
            }
        }
    }
    check_cancel()
}

/// Create every missing ancestor of `dest_path` (mkdir -p).
///
/// A single mkdir of the immediate parent fails on path-rooted protocols
/// when that parent is itself missing (`nested/deep/deeper/tiny.txt` needs
/// `nested` and `nested/deep` first). `AlreadyExists` is success. Any other
/// mkdir error is ignored here: an existing folder often answers with a
/// protocol code that is not `AlreadyExists`, and the upload that follows
/// is what reports a parent that is really missing.
async fn ensure_parent_dir(
    dest: &mut dyn StorageProvider,
    dest_path: &str,
    cancel: &CancellationToken,
) -> Result<(), ProviderError> {
    check_copy_cancel(cancel)?;
    // The overlay's upload owns parent creation and its plaintext-folder
    // refusal. Precreating through mkdir would bypass that refusal by making
    // a parallel encrypted tree before upload can inspect the missing parent.
    if dest
        .as_any_mut()
        .is::<crate::crypt_overlay_provider::CryptOverlayProvider>()
    {
        return Ok(());
    }
    let Some(parent) = remote_parent_dir(dest_path) else {
        return Ok(());
    };
    for ancestor in crate::community_benchmark::benchmark_mkdir_ladder(&parent) {
        check_copy_cancel(cancel)?;
        match dest.mkdir(&ancestor).await {
            Ok(()) | Err(ProviderError::AlreadyExists(_)) => {}
            Err(err) => {
                tracing::debug!("cross-profile: mkdir {}: {}", ancestor, err);
            }
        }
    }
    check_copy_cancel(cancel)
}

/// The folder a remote file lives in, split on `/` so a Windows host does
/// not rewrite the remote path. `None` when the file sits at the root.
fn remote_parent_dir(dest_path: &str) -> Option<String> {
    let trimmed = dest_path.trim_end_matches('/');
    let (parent, _) = trimmed.rsplit_once('/')?;
    if parent.trim_start_matches('/').is_empty() {
        None
    } else {
        Some(parent.to_string())
    }
}

/// After an upload that returned success, the destination must actually
/// hold the file. A provider that answers OK and then drops the object
/// (or renames it away from `dest_path`) used to be counted as transferred.
/// A stat that fails for any reason other than `NotFound` is not proof the
/// file is missing, so the upload result stands.
async fn confirm_destination_has_file(
    dest: &mut dyn StorageProvider,
    dest_path: &str,
) -> Result<(), ProviderError> {
    match dest.stat(dest_path).await {
        Ok(_) => Ok(()),
        Err(ProviderError::NotFound(_)) => Err(ProviderError::TransferFailed(format!(
            "upload reported success but {dest_path} is not on the destination"
        ))),
        Err(err) => {
            tracing::debug!(
                "cross-profile: destination stat after upload failed ({}); keeping the upload result",
                err
            );
            Ok(())
        }
    }
}

/// `stat` of an object-store prefix is `NotFound` when the folder is only a
/// common prefix (S3 HEAD of a key that was never a marker). Listing that
/// prefix is how `ls` sees it. An empty listing is not proof: a missing
/// prefix lists as empty too, and stays `NotFound`.
async fn stat_for_plan(
    source: &mut dyn StorageProvider,
    path: &str,
) -> Result<crate::providers::types::RemoteEntry, ProviderError> {
    match source.stat(path).await {
        Ok(entry) => Ok(entry),
        Err(ProviderError::NotFound(missing)) => match source.list(path).await {
            Ok(listing) if !listing.is_empty() => {
                let name = path
                    .rsplit('/')
                    .find(|segment| !segment.is_empty())
                    .unwrap_or(path)
                    .to_string();
                Ok(crate::providers::types::RemoteEntry::directory(
                    name,
                    path.to_string(),
                ))
            }
            _ => Err(ProviderError::NotFound(missing)),
        },
        Err(err) => Err(err),
    }
}

/// Planned files with no transferred, skipped, or failed outcome. The runner
/// adds this gap to `failed` so a result can be reconciled:
/// `transferred + skipped + failed == planned`.
pub fn transfer_tally_gap(planned: u64, transferred: u64, skipped: u64, failed: u64) -> u64 {
    let accounted = transferred.saturating_add(skipped).saturating_add(failed);
    planned.saturating_sub(accounted)
}

/// Named file failures already counted in `failed`, plus one unnamed row when
/// `gap` files never received an outcome. A zero gap leaves the list as it is,
/// so a file that already has a row is not recorded twice.
pub fn failures_with_tally_gap(
    mut failures: Vec<CrossProfileFileFailure>,
    gap: u64,
) -> Vec<CrossProfileFileFailure> {
    if gap > 0 {
        failures.push(CrossProfileFileFailure {
            source_path: String::new(),
            dest_path: String::new(),
            error: format!("{gap} planned file(s) left the run without a result"),
        });
    }
    failures
}

/// A headed AeroCrypt source that was read raw puts `.aerocrypt.tsv` in the
/// plan. Copying that plan writes ciphertext and reports success.
pub fn plan_includes_raw_crypt_marker(entries: &[CrossProfileTransferEntry]) -> bool {
    entries.iter().any(|entry| {
        entry.display_name.trim() == ".aerocrypt.tsv"
            || entry.source_path == ".aerocrypt.tsv"
            || entry.source_path.ends_with("/.aerocrypt.tsv")
    })
}

/// Ensure a directory path ends with '/'.
fn normalize_dir(path: &str) -> String {
    if path.ends_with('/') {
        path.to_string()
    } else {
        format!("{}/", path)
    }
}

fn is_virtual_root_path(path: &str) -> bool {
    let trimmed = path.trim();
    trimmed.is_empty() || trimmed == "/" || trimmed == "."
}

fn fallback_display_name(name: &str, path: &str) -> String {
    let trimmed = name.trim();
    if !trimmed.is_empty() {
        return trimmed.to_string();
    }

    Path::new(path)
        .file_name()
        .map(|segment| segment.to_string_lossy().to_string())
        .filter(|segment| !segment.trim().is_empty())
        .unwrap_or_else(|| path.trim_matches('/').to_string())
}

async fn resolved_source_path(
    source: &mut dyn StorageProvider,
    requested_path: &str,
) -> Result<String, ProviderError> {
    if is_virtual_root_path(requested_path) {
        source.pwd().await
    } else {
        Ok(requested_path.to_string())
    }
}

async fn effective_root_stat(
    source: &mut dyn StorageProvider,
    source_path: &str,
) -> Result<crate::providers::types::RemoteEntry, ProviderError> {
    if is_virtual_root_path(source_path) {
        return Ok(crate::providers::types::RemoteEntry {
            name: "/".to_string(),
            path: source_path.to_string(),
            is_dir: true,
            size: 0,
            modified: None,
            permissions: None,
            owner: None,
            group: None,
            is_symlink: false,
            link_target: None,
            mime_type: None,
            metadata: std::collections::HashMap::new(),
        });
    }

    let stat = stat_for_plan(source, source_path).await?;
    if stat.name.trim().is_empty() && is_virtual_root_path(&stat.path) {
        Ok(crate::providers::types::RemoteEntry {
            name: "/".to_string(),
            path: source_path.to_string(),
            is_dir: true,
            size: 0,
            modified: None,
            permissions: None,
            owner: None,
            group: None,
            is_symlink: false,
            link_target: None,
            mime_type: None,
            metadata: std::collections::HashMap::new(),
        })
    } else {
        Ok(stat)
    }
}

fn preserve_temp_mtime(path: &Path, source_modified: Option<&str>) {
    let Some(source_modified) = source_modified else {
        return;
    };

    let Some(file_time) =
        crate::parse_remote_mtime(source_modified).map(|secs| FileTime::from_unix_time(secs, 0))
    else {
        tracing::debug!(
            "cross-profile: unsupported mtime format '{}'",
            source_modified
        );
        return;
    };

    if let Err(err) = filetime::set_file_mtime(path, file_time) {
        tracing::debug!("cross-profile: failed to preserve temp-file mtime: {}", err);
    }
}

/// Map a source file path to its corresponding destination path.
///
/// For single files: dest_path is used directly.
/// For recursive: strips the source root prefix and appends to dest_path.
fn map_dest_path(source_roots: &[String], source_file: &str, dest_base: &str) -> String {
    let relative = source_roots
        .iter()
        .find_map(|root| source_file.strip_prefix(root))
        .unwrap_or(source_file)
        .trim_start_matches('/');

    if relative.is_empty() {
        // Single file case: source_file == source_root (without trailing /)
        dest_base.to_string()
    } else {
        let dest_base = if dest_base.ends_with('/') {
            dest_base.to_string()
        } else {
            format!("{}/", dest_base)
        };
        format!("{}{}", dest_base, relative)
    }
}

fn source_root_candidates(request_path: &str, provider_root: &str) -> Vec<String> {
    let mut roots = vec![normalize_dir(request_path)];
    let provider_root = normalize_dir(provider_root);
    if !roots.iter().any(|root| root == &provider_root) {
        roots.push(provider_root);
    }
    roots
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn recursive_plan_preserves_empty_root_and_nested_directories_without_writes() {
        use crate::providers::RemoteEntry;
        use crate::sync_core::scan::tests::WalkTreeProvider;
        use std::collections::HashMap;
        let request = CrossProfileTransferRequest {
            source_profile: "source".into(),
            dest_profile: "dest".into(),
            source_path: "/src".into(),
            dest_path: "/out".into(),
            recursive: true,
            dry_run: true,
            skip_existing: false,
        };
        let mut dest = WalkTreeProvider::new(HashMap::new(), false);
        let mut source = WalkTreeProvider::new(
            HashMap::from([
                (
                    "/src".into(),
                    vec![
                        RemoteEntry::directory("empty".into(), "/src/empty".into()),
                        RemoteEntry::file("data".into(), "/src/data".into(), 7),
                    ],
                ),
                (
                    "/src/empty".into(),
                    vec![RemoteEntry::directory(
                        "nested".into(),
                        "/src/empty/nested".into(),
                    )],
                ),
                ("/src/empty/nested".into(), vec![]),
            ]),
            false,
        );
        let plan = plan_transfer(&mut source, &mut dest, &request)
            .await
            .unwrap();
        assert_eq!(plan.total_files, 1);
        assert_eq!(plan.total_bytes, 7);
        assert_eq!(plan.entries[0].dest_path, "/out/data");
        assert_eq!(
            plan.directories
                .iter()
                .map(|entry| entry.dest_path.as_str())
                .collect::<Vec<_>>(),
            ["/out", "/out/empty", "/out/empty/nested"]
        );
        assert_eq!(dest.writes.load(std::sync::atomic::Ordering::Relaxed), 0);

        let mut empty = WalkTreeProvider::new(HashMap::from([("/src".into(), vec![])]), false);
        let plan = plan_transfer(&mut empty, &mut dest, &request)
            .await
            .unwrap();
        assert!(plan.entries.is_empty());
        assert_eq!(plan.directories.len(), 1);
        assert_eq!(plan.directories[0].dest_path, "/out");

        source.unlistable.insert("/src/empty".into());
        assert!(
            plan_transfer(&mut source, &mut dest, &request)
                .await
                .is_err(),
            "unreadable is not empty"
        );
    }

    #[test]
    fn request_serialization_roundtrip() {
        let req = CrossProfileTransferRequest {
            source_profile: "Source SFTP".into(),
            dest_profile: "Dest S3".into(),
            source_path: "/data/file.txt".into(),
            dest_path: "/backup/file.txt".into(),
            recursive: true,
            dry_run: false,
            skip_existing: true,
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: CrossProfileTransferRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.source_profile, "Source SFTP");
        assert!(back.recursive);
        assert!(back.skip_existing);
    }

    #[test]
    fn plan_empty_entries() {
        let plan = CrossProfileTransferPlan {
            source_profile: "A".into(),
            dest_profile: "B".into(),
            entries: vec![],
            directories: vec![],
            total_files: 0,
            total_bytes: 0,
        };
        assert_eq!(plan.entries.len(), 0);
        assert_eq!(plan.total_files, 0);
    }

    #[test]
    fn plan_with_entries() {
        let entry = CrossProfileTransferEntry {
            source_path: "/src/a.txt".into(),
            dest_path: "/dst/a.txt".into(),
            display_name: "a.txt".into(),
            size: 1024,
            modified: Some("2026-01-01T00:00:00Z".into()),
            is_dir: false,
        };
        let plan = CrossProfileTransferPlan {
            source_profile: "A".into(),
            dest_profile: "B".into(),
            entries: vec![entry.clone()],
            directories: vec![],
            total_files: 1,
            total_bytes: 1024,
        };
        assert_eq!(plan.entries.len(), 1);
        assert_eq!(plan.entries[0].size, 1024);
        assert!(!plan.entries[0].is_dir);
    }

    #[test]
    fn result_defaults() {
        let res = CrossProfileTransferResult {
            planned_files: 10,
            transferred_files: 8,
            skipped_files: 1,
            failed_files: 1,
            total_bytes: 2048,
            duration_ms: 500,
        };
        assert_eq!(
            res.planned_files,
            res.transferred_files + res.skipped_files + res.failed_files
        );
    }

    // ── Phase 2 tests ──────────────────────────────────────────────────

    #[test]
    fn normalize_dir_adds_slash() {
        assert_eq!(normalize_dir("/data"), "/data/");
        assert_eq!(normalize_dir("/data/"), "/data/");
        assert_eq!(normalize_dir("/"), "/");
    }

    #[test]
    fn map_dest_path_root_file() {
        // File directly under source root
        assert_eq!(
            map_dest_path(&["/data/".to_string()], "/data/file.txt", "/backup/"),
            "/backup/file.txt"
        );
    }

    #[test]
    fn map_dest_path_recursive() {
        let source_roots = vec!["/data/".to_string()];
        assert_eq!(
            map_dest_path(&source_roots, "/data/a.txt", "/backup/"),
            "/backup/a.txt"
        );
        assert_eq!(
            map_dest_path(&source_roots, "/data/sub/b.txt", "/backup/"),
            "/backup/sub/b.txt"
        );
        assert_eq!(
            map_dest_path(&source_roots, "/data/sub/deep/c.txt", "/backup"),
            "/backup/sub/deep/c.txt"
        );
    }

    #[test]
    fn map_dest_path_dest_without_trailing_slash() {
        let dest = map_dest_path(&["/src/".to_string()], "/src/file.txt", "/dst");
        assert_eq!(dest, "/dst/file.txt");
    }

    #[test]
    fn map_dest_path_accepts_provider_canonical_root() {
        let roots = source_root_candidates("cli_test/copilot", "/home/ftp/cli_test/copilot");
        let dest = map_dest_path(&roots, "/home/ftp/cli_test/copilot/sub/file.txt", "/backup");
        assert_eq!(dest, "/backup/sub/file.txt");
    }

    /// A cross-profile copy plans the files its walk of the source folder
    /// finds and copies each one to the destination. The walk queued every
    /// folder a listing named: through a folder listed inside itself it
    /// planned `b.txt` once per level, through one listed as `/` it planned
    /// the account root's files, a file named twice was copied twice, and
    /// `/elsewhere/big.bin` was copied as one of the folder's. It plans each
    /// file under the folder once, and a listing that names a folder it
    /// cannot follow fails the plan: a copy that leaves part of the folder
    /// behind must not report success.
    #[tokio::test]
    async fn a_copy_plans_each_file_under_the_folder_once_or_fails() {
        for shape in crate::sync_core::scan::tests::stray_listings() {
            let case = shape.case;
            let mut source = shape.provider();
            let planned = collect_source_entries(&mut source, "/root", true).await;
            if shape.refused > 0 {
                let err = planned.expect_err(case).to_string();
                assert!(err.contains("cannot transfer /root whole"), "{case}: {err}");
                continue;
            }
            let mut paths: Vec<String> = planned
                .unwrap_or_else(|e| panic!("{case}: {e}"))
                .into_iter()
                .map(|entry| entry.source_path)
                .collect();
            paths.sort();
            assert_eq!(paths, shape.file_paths(), "{case}");
        }
    }

    /// A copy of the account root (`/`, the virtual root) walks it the same
    /// way: a folder listed inside itself is refused, not walked to the cap.
    #[tokio::test]
    async fn a_copy_of_the_account_root_fails_on_a_folder_listed_inside_itself() {
        let shape = crate::sync_core::scan::tests::stray_listings()
            .into_iter()
            .next()
            .expect("the first shape");
        let mut dirs = shape.dirs.clone();
        let root = dirs.remove("/root").expect("the root listing");
        dirs.insert("/".to_string(), root);
        let mut source = crate::sync_core::scan::tests::WalkTreeProvider::new(dirs, false);
        let err = collect_source_entries(&mut source, "/", true)
            .await
            .expect_err("a folder listed inside itself")
            .to_string();
        assert!(err.contains("cannot transfer / whole"), "{err}");
    }

    #[test]
    fn deep_file_parent_chain_includes_every_ancestor() {
        let parent = remote_parent_dir("/nested/deep/deeper/tiny.txt").expect("parent");
        assert_eq!(
            crate::community_benchmark::benchmark_mkdir_ladder(&parent),
            vec![
                "/nested".to_string(),
                "/nested/deep".to_string(),
                "/nested/deep/deeper".to_string(),
            ]
        );
        assert!(remote_parent_dir("/tiny.txt").is_none());
    }

    #[test]
    fn tally_gap_is_the_planned_files_with_no_outcome() {
        assert_eq!(transfer_tally_gap(9, 8, 0, 0), 1);
        assert_eq!(transfer_tally_gap(9, 7, 1, 1), 0);
        assert_eq!(transfer_tally_gap(3, 4, 0, 0), 0);
    }

    #[test]
    fn named_failures_stay_and_the_gap_adds_one_unnamed_row() {
        let named = CrossProfileFileFailure {
            source_path: "/src/a".to_string(),
            dest_path: "/dst/a".to_string(),
            error: "write failed".to_string(),
        };
        assert_eq!(
            failures_with_tally_gap(vec![named.clone()], 0),
            vec![named.clone()]
        );
        let with_gap = failures_with_tally_gap(vec![named.clone()], 2);
        assert_eq!(with_gap.len(), 2);
        assert_eq!(with_gap[0], named);
        assert_eq!(with_gap[1].source_path, "");
        assert_eq!(with_gap[1].dest_path, "");
        assert_eq!(
            with_gap[1].error,
            "2 planned file(s) left the run without a result"
        );
    }

    #[test]
    fn raw_crypt_marker_names_the_overlay_file() {
        let marker = CrossProfileTransferEntry {
            source_path: "/vault/.aerocrypt.tsv".into(),
            dest_path: "/dst/.aerocrypt.tsv".into(),
            display_name: ".aerocrypt.tsv".into(),
            size: 12,
            modified: None,
            is_dir: false,
        };
        let plain = CrossProfileTransferEntry {
            display_name: "notes.txt".into(),
            source_path: "/vault/notes.txt".into(),
            ..marker.clone()
        };
        assert!(plan_includes_raw_crypt_marker(&[marker]));
        assert!(!plan_includes_raw_crypt_marker(&[plain]));
    }

    /// S3 (and any prefix store) answers `stat` of a folder with NotFound
    /// when the folder is only a common prefix. `ls` lists it. The plan must
    /// follow the listing.
    #[tokio::test]
    async fn a_prefix_that_lists_but_does_not_stat_is_still_planned() {
        use crate::providers::types::RemoteEntry;
        use crate::sync_core::scan::tests::WalkTreeProvider;

        let mut source = WalkTreeProvider::new(
            std::collections::HashMap::from([(
                "/data".to_string(),
                vec![RemoteEntry::file(
                    "tiny.txt".into(),
                    "/data/tiny.txt".into(),
                    4,
                )],
            )]),
            false,
        );
        source.stat_misses.insert("/data".to_string());

        let planned = collect_source_entries(&mut source, "/data", true)
            .await
            .expect("prefix listing is a directory");
        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].source_path, "/data/tiny.txt");
    }

    #[tokio::test]
    async fn a_plan_that_contains_the_aerocrypt_marker_is_refused() {
        use crate::providers::types::RemoteEntry;
        use crate::sync_core::scan::tests::WalkTreeProvider;

        let mut source = WalkTreeProvider::new(
            std::collections::HashMap::from([(
                "/vault".to_string(),
                vec![RemoteEntry::file(
                    ".aerocrypt.tsv".into(),
                    "/vault/.aerocrypt.tsv".into(),
                    20,
                )],
            )]),
            false,
        );
        let mut dest = WalkTreeProvider::new(std::collections::HashMap::new(), false);
        let request = CrossProfileTransferRequest {
            source_profile: "crypt".into(),
            dest_profile: "plain".into(),
            source_path: "/vault".into(),
            dest_path: "/out".into(),
            recursive: true,
            dry_run: true,
            skip_existing: false,
        };
        let err = plan_transfer(&mut source, &mut dest, &request)
            .await
            .expect_err("raw marker")
            .to_string();
        assert!(err.contains("overlay not applied"), "{err}");
    }
}
