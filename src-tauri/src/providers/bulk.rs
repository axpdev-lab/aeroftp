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
}

pub type BulkProgress = Box<dyn Fn(usize, u64, u64) + Send + Sync>;

/// Conservative bounds shared by planners and provider adapters. Path bytes
/// bound command lines as well as file counts; payload stays streamed on disk.
pub const MAX_BULK_FILES: usize = 256;
pub const MAX_BULK_PATH_BYTES: usize = 24 * 1024;
pub const MAX_BULK_BYTES: u64 = 128 * 1024 * 1024;

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
        .map(|item| crate::transfer_domain::TransferEntry {
            id: String::new(),
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
}
