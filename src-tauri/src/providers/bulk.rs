// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Optional multi-file wire operation. A bulk is bounded, contains files only,
//! and never authorizes a recursive directory transfer behind the caller's plan.

use super::ProviderError;

#[derive(Debug, Clone)]
pub struct BulkTransferItem {
    pub local_path: String,
    pub remote_path: String,
    pub size: u64,
}

/// Results are positional, exactly one per submitted file. An uncertain item
/// may already have reached the server: callers must never replay the bulk or
/// automatically retry that item (which could create duplicate revisions).
#[derive(Debug)]
pub enum BulkTransferOutcome {
    Completed,
    Failed(ProviderError),
    Uncertain(String),
    /// Nothing reached the wire for this item: the adapter never handed it to
    /// the native program, or the program stopped before it transferred
    /// anything. The caller runs its established single-file path instead.
    NotAttempted(String),
}

pub type BulkProgress = Box<dyn Fn(usize, u64, u64) + Send + Sync>;

/// Conservative bounds shared by planners and provider adapters. Path bytes
/// bound command lines as well as file counts; payload stays streamed on disk.
/// Windows caps a whole command line at 32,767 UTF-16 units; Unix systems
/// allow far more (Linux 128 KiB per argument and 2 MiB in all), so the
/// budget there stops being the reason a deep tree needs several launches.
pub const MAX_BULK_FILES: usize = 256;
pub const MAX_BULK_PATH_BYTES: usize = if cfg!(windows) { 24 * 1024 } else { 96 * 1024 };
pub const MAX_BULK_BYTES: u64 = 128 * 1024 * 1024;
/// Command-line bytes a planner keeps aside for the program, its verb, its
/// options and the destination argument of one bulk.
pub const BULK_COMMAND_RESERVE_BYTES: usize = 1024;
/// A download source may be addressed by a node identifier instead of its
/// name (Proton: `/my-files/<volume>~<link>`), so it is never counted below
/// this length.
pub const NODE_ID_ARG_BYTES: usize = 64;

/// Command-line bytes one file adds to a native bulk: its local path as an
/// upload source, or its remote path (at least [`NODE_ID_ARG_BYTES`]) as a
/// download source, plus quoting and a separator. The direction-free maximum
/// lets one planner serve mixed upload/download plans.
pub fn command_arg_bytes(absolute_local: &str, remote: &str) -> usize {
    absolute_local
        .len()
        .max(remote.len().max(NODE_ID_ARG_BYTES))
        .saturating_add(3)
}

/// The absolute spelling of a local path, without touching the filesystem.
/// The native program resolves a relative argument against the same working
/// directory, so planning on this spelling counts its command-line bytes
/// exactly and lets relative CLI arguments (`put -r ./dir`) share a bulk.
pub fn absolute_local_path(path: &str) -> Option<String> {
    if path.is_empty() {
        return None;
    }
    std::path::absolute(path)
        .ok()?
        .into_os_string()
        .into_string()
        .ok()
}

/// Only names that retain their spelling through the native CLI's portable
/// download-name mapping enter a bulk. Other names retain the single-file
/// adapter, including its collision-safe temporary destination and rename.
pub fn plain_bulk_name(path: &str) -> Option<&str> {
    let name = path.rsplit('/').next()?;
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = cfg!(windows)
        && (matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || ((stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.len() == 4
                && matches!(stem.as_bytes()[3], b'1'..=b'9')));
    (!reserved
        && !name.is_empty()
        && name != "."
        && name != ".."
        && !name.ends_with(['.', ' '])
        && !name
            .chars()
            .any(|c| c.is_control() || "<>:\"|?*\\".contains(c)))
    .then_some(name)
}

pub fn remote_parent(path: &str) -> Option<&str> {
    let (parent, _) = path.rsplit_once('/')?;
    Some(if parent.is_empty() { "/" } else { parent })
}

/// Whether the caller runs its single-file path once for this outcome: an
/// item that never reached the wire, or one the transport failed in a way a
/// second try can answer differently. Every other failure is final, also a
/// download whose size is not the claimed one: the single-file path must not
/// become a way around a bulk check.
pub fn goes_back_to_single_file(outcome: &BulkTransferOutcome) -> bool {
    match outcome {
        BulkTransferOutcome::NotAttempted(_) => true,
        BulkTransferOutcome::Failed(error) => {
            let failure = crate::transfer_multipart::transfer_failure_from_provider(error, None);
            failure.retryable && transport_is_transient(failure.kind)
        }
        BulkTransferOutcome::Completed | BulkTransferOutcome::Uncertain(_) => false,
    }
}

fn transport_is_transient(kind: crate::transfer_domain::TransferFailureKind) -> bool {
    use crate::transfer_domain::TransferFailureKind as Kind;
    matches!(
        kind,
        Kind::RateLimited
            | Kind::ServiceUnavailable
            | Kind::Timeout
            | Kind::ConnectionLost
            | Kind::ConnectionReset
            | Kind::MaxConnections
    )
}

pub fn uncertain_items(count: usize, message: &str) -> Vec<BulkTransferOutcome> {
    (0..count)
        .map(|_| BulkTransferOutcome::Uncertain(message.to_string()))
        .collect()
}

/// CLI compatibility guard: a single-lease native bulk must reduce process
/// launches enough to justify replacing the CLI's independent worker lane.
/// Mixed spellings, isolated directories and large files can otherwise turn
/// a parallel per-file workload into a slower serial fallback. GUI/sync were
/// already single-lease and do not need this admission guard.
pub fn reduces_cli_invocations(items: &[BulkTransferItem], upload: bool, workers: usize) -> bool {
    let entries: Vec<_> = items
        .iter()
        .enumerate()
        .map(|(index, item)| crate::transfer_domain::TransferEntry {
            id: index.to_string(),
            display_name: String::new(),
            local_path: item.local_path.clone(),
            remote_path: item.remote_path.clone(),
            size: item.size,
            modified: None,
        })
        .collect();
    let groups = crate::transfer_bulk::plan_groups_by_key(&entries, |entry| {
        crate::transfer_bulk::group_key(entry, upload)
    });
    groups.len() < items.len() && groups.len() <= items.len().div_ceil(workers.max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_admission_keeps_parallel_fallback_for_isolated_or_large_files() {
        let mut items: Vec<_> = (0..200)
            .map(|i| BulkTransferItem {
                local_path: format!("/tmp/file-{i}.bin"),
                remote_path: format!("/bulk/file-{i}.bin"),
                size: 4096,
            })
            .collect();
        assert!(reduces_cli_invocations(&items, true, 4));
        for (i, item) in items.iter_mut().enumerate() {
            item.remote_path = format!("/folder-{i}/file-{i}.bin");
        }
        assert!(!reduces_cli_invocations(&items, true, 4));
        for item in &mut items {
            item.remote_path = format!(
                "/bulk/{}",
                std::path::Path::new(&item.local_path)
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
            );
            item.size = MAX_BULK_BYTES + 1;
        }
        assert!(!reduces_cli_invocations(&items, true, 4));
    }

    /// v4.2.4 kept every relative local path on the per-file lane, so the
    /// documented `put -r ./dir` and `get -r ... .` forms launched one
    /// process per file (live: 200 launches, 116 s against 43 s).
    #[test]
    fn cli_admission_accepts_relative_local_paths() {
        let items: Vec<_> = (0..200)
            .map(|i| BulkTransferItem {
                local_path: format!("flat/file-{i:03}.bin"),
                remote_path: format!("/my-files/flat/file-{i:03}.bin"),
                size: 4096,
            })
            .collect();
        assert!(reduces_cli_invocations(&items, true, 4));
        assert!(reduces_cli_invocations(&items, false, 4));
    }

    #[test]
    fn absolute_local_path_resolves_against_the_working_directory() {
        let cwd = std::env::current_dir().unwrap();
        let resolved = absolute_local_path("flat/a.bin").unwrap();
        assert_eq!(std::path::Path::new(&resolved), cwd.join("flat/a.bin"));
        assert_eq!(absolute_local_path(""), None);
    }

    #[test]
    fn command_arg_bytes_counts_the_longer_side_and_node_ids() {
        assert_eq!(command_arg_bytes("/a", "/b"), NODE_ID_ARG_BYTES + 3);
        let long = "x".repeat(300);
        assert_eq!(command_arg_bytes(&long, "/b"), 303);
    }
}
