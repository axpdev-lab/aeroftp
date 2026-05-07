// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Windows portable auto-updater: when the previous AeroFTP swapped its
    // .exe in place, it relaunches the new build with
    // `--post-update-cleanup <path-to-old-exe>`. Pick that up before Tauri
    // boots and run the cleanup in a detached thread so the UI starts
    // immediately. The arg is ignored on non-Windows builds.
    #[cfg(windows)]
    ftp_client_gui_lib::windows_update_helper::try_handle_post_update_cleanup_arg();

    ftp_client_gui_lib::run();
}
