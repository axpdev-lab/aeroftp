//! MTP / WPD portable-device provider (APPENDIX-MTP, tracker #422 Requested #4).
//!
//! Phase 1: types, virtual paths, Null backend, honest TransferCapabilities.
//! Phase 2: libmtp (Linux) backend + Tauri list/open/close commands.
//! Phase 3: WPD (Windows) backend.
//! Phase 4: PLACES Portable devices UI.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

pub mod backend;
pub mod commands;
pub mod path;
pub mod provider;

#[cfg(all(target_os = "linux", mtp_libmtp))]
pub mod linux_libmtp;

pub use backend::{
    list_mtp_devices, mtp_backend_linked, platform_backend, MtpBackend, MtpDeviceInfo,
    NullMtpBackend,
};
pub use path::{
    join_virtual, leaf_name, normalize_virtual_path, parent_path, sanitize_leaf_for_download,
    split_segments,
};
pub use provider::MtpProvider;
