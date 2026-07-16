//! MTP / WPD portable-device provider (APPENDIX-MTP, tracker #422 Requested #4).
//!
//! Phase 1: types, virtual paths, Null backend, honest TransferCapabilities.
//! Phase 2: libmtp (Linux) backend + Tauri list/open/close commands.
//! Phase 3: WPD (Windows) backend + `mtp-devices-changed` wake.
//! Phase 4: PLACES Portable devices UI.
//! Phase 5: ProviderState browse session + whole-file transfer polish.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

pub mod backend;
pub mod commands;
pub mod path;
pub mod provider;

#[cfg(all(target_os = "linux", mtp_libmtp))]
pub mod linux_libmtp;

#[cfg(windows)]
pub mod windows_wpd;

pub use backend::{
    list_mtp_devices, mtp_backend_linked, platform_backend, FakeMtpBackend, MtpBackend,
    MtpDeviceInfo, NullMtpBackend,
};
pub use path::{
    join_virtual, leaf_name, normalize_virtual_path, parent_path, sanitize_leaf_for_download,
    split_segments,
};
pub use provider::MtpProvider;

/// Start platform hotplug wake for portable devices (`mtp-devices-changed`).
///
/// Windows: WM_DEVICECHANGE -> debounced event. Other OS: no-op for now
/// (Phase 4 FE may poll; Linux udev wake is a later polish).
pub fn start_mtp_device_watcher(app_handle: tauri::AppHandle) {
    #[cfg(windows)]
    {
        windows_wpd::start_mtp_device_watcher(app_handle);
    }
    #[cfg(not(windows))]
    {
        let _ = app_handle;
    }
}
