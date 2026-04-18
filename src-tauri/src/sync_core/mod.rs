//! Sync core — helpers shared between the `aeroftp-cli` binary and the MCP
//! server.
//!
//! Scope: pure orchestration of local/remote directory scans, tree
//! comparison, and simple sync execution. Exposes no Tauri types, no Clap
//! types, and no MCP types — so both front-ends can drive it without
//! cross-dependencies.
//!
//! The module is feature-flag friendly: it only depends on `StorageProvider`
//! plus `walkdir`, `globset`, `sha2`, `chrono`, giving a tight `cargo check
//! --lib` footprint (no extra build time).

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

pub mod compare;
pub mod scan;
pub mod sync;

pub use compare::{compare_trees, DiffEntry, DiffReport};
pub use scan::{scan_local_tree, scan_remote_tree, LocalEntry, RemoteEntry, ScanOptions};
pub use sync::{
    sync_tree_core, ConflictMode, FileOutcome, NoopProgressSink, SyncDirection, SyncError,
    SyncOptions, SyncPhase, SyncProgressSink, SyncReport,
};
