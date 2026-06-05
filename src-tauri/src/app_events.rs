// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Bridge for emitting frontend events from deep, Tauri-agnostic code paths
//! (for example storage providers) without threading an `AppHandle` through
//! every call. The handle is registered once during `setup`; in the CLI it is
//! never registered, so every emitter here is a silent no-op there.

use std::sync::OnceLock;

use tauri::{AppHandle, Emitter};

static APP_HANDLE: OnceLock<AppHandle> = OnceLock::new();

/// Register the global `AppHandle` once, during Tauri `setup`. Idempotent: a
/// second call is ignored.
pub fn register_app_handle(handle: AppHandle) {
    let _ = APP_HANDLE.set(handle);
}

/// Frontend event name for the MEGAcmd warmup notice (Ehud wishlist #275).
pub const MEGA_WARMUP_EVENT: &str = "mega-warmup-notice";

/// User-visible wording shared with the CLI stderr line. Keep identical across
/// CLI and GUI so the cause of the first-call delay reads the same everywhere.
pub const MEGA_WARMUP_MESSAGE: &str = "MEGAcmd server not running, starting in the background...";

/// Emit the MEGAcmd warmup notice to the frontend when running under the GUI.
/// No-op in the CLI (no handle registered), where the stderr line covers it.
pub fn emit_mega_warmup_notice() {
    if let Some(handle) = APP_HANDLE.get() {
        let _ = handle.emit(MEGA_WARMUP_EVENT, MEGA_WARMUP_MESSAGE);
    }
}
