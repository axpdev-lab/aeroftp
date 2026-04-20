//! Compare a local and a remote tree and emit a structured diff report.
//!
//! Used by `aeroftp-cli check`, `aeroftp-cli reconcile`, and the MCP
//! `aeroftp_check_tree` tool. The output is categorized into four buckets
//! (`match`, `differ`, `missing_local`, `missing_remote`) so every consumer
//! can project it into its own wire format (flat list, grouped JSON, CSV,
//! progress counters, etc.).

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

use crate::sync_core::scan::{LocalEntry, RemoteEntry};

/// A single compared entry.
#[derive(Debug, Clone)]
pub struct DiffEntry {
    pub rel_path: String,
    pub local_size: Option<u64>,
    pub remote_size: Option<u64>,
    pub local_sha256: Option<String>,
    /// The server-side checksum observed on the remote side (if any). Carried
    /// through so MCP consumers can surface both hashes alongside `differ`
    /// entries when `checksum=true` was requested.
    pub remote_checksum_alg: Option<String>,
    pub remote_checksum_hex: Option<String>,
    /// How this entry was classified. `"checksum"` means the two checksums
    /// disagree; `"size"` means only size-level evidence was available.
    /// `None` for `missing_local` / `missing_remote`.
    pub compare_method: Option<&'static str>,
}

/// Categorized diff report produced by [`compare_trees`].
#[derive(Debug, Clone, Default)]
pub struct DiffReport {
    pub matches: Vec<DiffEntry>,
    pub differ: Vec<DiffEntry>,
    pub missing_local: Vec<DiffEntry>,
    pub missing_remote: Vec<DiffEntry>,
}

impl DiffReport {
    pub fn match_count(&self) -> usize {
        self.matches.len()
    }
    pub fn differ_count(&self) -> usize {
        self.differ.len()
    }
    pub fn missing_local_count(&self) -> usize {
        self.missing_local.len()
    }
    pub fn missing_remote_count(&self) -> usize {
        self.missing_remote.len()
    }
    pub fn has_differences(&self) -> bool {
        !self.differ.is_empty() || !self.missing_local.is_empty() || !self.missing_remote.is_empty()
    }
}

/// Compare scanned local and remote entries.
///
/// `one_way` restricts the comparison to local → remote direction (skipping
/// `missing_local` detection), matching the semantics of the CLI
/// `check --one-way` flag.
pub fn compare_trees(local: &[LocalEntry], remote: &[RemoteEntry], one_way: bool) -> DiffReport {
    use std::collections::HashMap;

    let mut report = DiffReport::default();
    let local_map: HashMap<&str, &LocalEntry> =
        local.iter().map(|e| (e.rel_path.as_str(), e)).collect();
    let remote_map: HashMap<&str, &RemoteEntry> =
        remote.iter().map(|e| (e.rel_path.as_str(), e)).collect();

    for (rel, local_entry) in &local_map {
        match remote_map.get(rel) {
            Some(remote_entry) => {
                // Checksum comparison only fires when both sides produced a
                // hash using the SAME algorithm. Any mismatched algorithm
                // (e.g. local SHA-256, remote MD5) is downgraded to size-only
                // — comparing different hashes is a category error.
                let checksum_method = match (
                    local_entry.sha256.as_ref(),
                    remote_entry.checksum_alg.as_deref(),
                    remote_entry.checksum_hex.as_ref(),
                ) {
                    (Some(local_hex), Some("sha256"), Some(remote_hex)) => {
                        Some((local_hex.eq_ignore_ascii_case(remote_hex), "checksum"))
                    }
                    _ => None,
                };
                let (is_match, method) = match checksum_method {
                    Some((matches, m)) => (matches, m),
                    None => (local_entry.size == remote_entry.size, "size"),
                };
                let entry = DiffEntry {
                    rel_path: (*rel).to_string(),
                    local_size: Some(local_entry.size),
                    remote_size: Some(remote_entry.size),
                    local_sha256: local_entry.sha256.clone(),
                    remote_checksum_alg: remote_entry.checksum_alg.clone(),
                    remote_checksum_hex: remote_entry.checksum_hex.clone(),
                    compare_method: Some(method),
                };
                if is_match {
                    report.matches.push(entry);
                } else {
                    report.differ.push(entry);
                }
            }
            None => {
                report.missing_remote.push(DiffEntry {
                    rel_path: (*rel).to_string(),
                    local_size: Some(local_entry.size),
                    remote_size: None,
                    local_sha256: local_entry.sha256.clone(),
                    remote_checksum_alg: None,
                    remote_checksum_hex: None,
                    compare_method: None,
                });
            }
        }
    }

    if !one_way {
        for (rel, remote_entry) in &remote_map {
            if !local_map.contains_key(rel) {
                report.missing_local.push(DiffEntry {
                    rel_path: (*rel).to_string(),
                    local_size: None,
                    remote_size: Some(remote_entry.size),
                    local_sha256: None,
                    remote_checksum_alg: remote_entry.checksum_alg.clone(),
                    remote_checksum_hex: remote_entry.checksum_hex.clone(),
                    compare_method: None,
                });
            }
        }
    }

    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(rel: &str, size: u64) -> LocalEntry {
        LocalEntry {
            rel_path: rel.to_string(),
            size,
            mtime: None,
            sha256: None,
        }
    }

    fn remote(rel: &str, size: u64) -> RemoteEntry {
        RemoteEntry {
            rel_path: rel.to_string(),
            size,
            mtime: None,
            checksum_alg: None,
            checksum_hex: None,
        }
    }

    fn remote_sha256(rel: &str, size: u64, hex: &str) -> RemoteEntry {
        RemoteEntry {
            rel_path: rel.to_string(),
            size,
            mtime: None,
            checksum_alg: Some("sha256".to_string()),
            checksum_hex: Some(hex.to_string()),
        }
    }

    fn local_sha256(rel: &str, size: u64, hex: &str) -> LocalEntry {
        LocalEntry {
            rel_path: rel.to_string(),
            size,
            mtime: None,
            sha256: Some(hex.to_string()),
        }
    }

    #[test]
    fn compare_trees_detects_checksum_mismatch_with_identical_size() {
        // Same-size file with swapped content — the scenario Legacy CMS
        // raised: one-byte change inside a config, size stays constant.
        let locals = vec![local_sha256("conf.json", 100, "aa")];
        let remotes = vec![remote_sha256("conf.json", 100, "bb")];
        let report = compare_trees(&locals, &remotes, false);
        assert_eq!(report.differ_count(), 1);
        assert_eq!(report.differ[0].compare_method, Some("checksum"));
    }

    #[test]
    fn compare_trees_matches_on_equal_checksum() {
        let locals = vec![local_sha256("conf.json", 100, "aa")];
        let remotes = vec![remote_sha256("conf.json", 100, "aa")];
        let report = compare_trees(&locals, &remotes, false);
        assert_eq!(report.match_count(), 1);
        assert_eq!(report.matches[0].compare_method, Some("checksum"));
    }

    #[test]
    fn compare_trees_falls_back_to_size_when_checksum_unavailable() {
        let locals = vec![local("f.bin", 10)];
        let remotes = vec![remote("f.bin", 10)];
        let report = compare_trees(&locals, &remotes, false);
        assert_eq!(report.match_count(), 1);
        assert_eq!(report.matches[0].compare_method, Some("size"));
    }

    #[test]
    fn compare_trees_categorizes_entries() {
        let locals = vec![
            local("keep.txt", 10),
            local("changed.txt", 5),
            local("local_only.txt", 3),
        ];
        let remotes = vec![
            remote("keep.txt", 10),
            remote("changed.txt", 8),
            remote("remote_only.txt", 4),
        ];
        let report = compare_trees(&locals, &remotes, false);
        assert_eq!(report.match_count(), 1);
        assert_eq!(report.differ_count(), 1);
        assert_eq!(report.missing_remote_count(), 1);
        assert_eq!(report.missing_local_count(), 1);
        assert!(report.has_differences());
    }

    #[test]
    fn compare_trees_one_way_skips_remote_only_entries() {
        let locals = vec![local("a.txt", 5)];
        let remotes = vec![remote("a.txt", 5), remote("extra.txt", 2)];
        let report = compare_trees(&locals, &remotes, true);
        assert_eq!(report.match_count(), 1);
        assert_eq!(report.missing_local_count(), 0);
    }

    #[test]
    fn compare_trees_empty_inputs_yield_no_differences() {
        let report = compare_trees(&[], &[], false);
        assert!(!report.has_differences());
        assert_eq!(report.match_count(), 0);
    }
}
