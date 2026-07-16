//! MTP / WPD portable-device provider (APPENDIX-MTP, tracker #422 Requested #4).
//!
//! Phase 1: types, virtual paths, Null backend, honest TransferCapabilities.
//! Phase 2/3: libmtp (Linux) and WPD (Windows) backends.
//! Phase 4: PLACES Portable devices UI.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

pub mod backend;
pub mod path;
pub mod provider;

pub use backend::{list_mtp_devices, MtpBackend, MtpDeviceInfo, NullMtpBackend};
pub use path::{
    join_virtual, leaf_name, normalize_virtual_path, parent_path, sanitize_leaf_for_download,
    split_segments,
};
pub use provider::MtpProvider;
