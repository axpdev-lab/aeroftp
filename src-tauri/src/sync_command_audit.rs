#![cfg(test)]
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Class-level pin: a `#[tauri::command]` that is not `async` runs on the main
//! thread, and on Linux the main thread is the GTK thread.
//!
//! Tauri decides this at expansion time. In `tauri-macros`, `WrapperAttributes`
//! starts at `ExecutionContext::Blocking` and only moves to `Async` if the
//! function is declared `async` or carries `#[tauri::command(async)]`; a
//! `Blocking` command is invoked inline on the thread that dispatches IPC.
//! So a synchronous command that blocks -- on a syscall, on a D-Bus round trip,
//! on a lock somebody else holds while blocking -- freezes the entire window
//! for that duration, with no spinner and nothing the user can tell apart from
//! a crash.
//!
//! Individual commands are pinned where they live, by tests that `block_on`
//! them: `block_on` takes a future, so turning one back into a `pub fn` fails
//! to compile rather than failing an assertion somebody can delete. That form
//! only defends the commands that already have such a test. It does nothing
//! about the failure mode that actually recurs, which is **addition**: someone
//! writes a new command months from now, writes it synchronous because that is
//! the shorter spelling, and the count of main-thread commands climbs back.
//!
//! This test is the answer to that. It reads the sources, collects every
//! synchronous command, and compares the set against two explicit lists:
//! `MAIN_THREAD_ALLOWED`, the ones that are justified, and
//! `MAIN_THREAD_NOT_YET_MOVED`, the ones that are not and are being drained.
//! A new synchronous command matches neither and is red. Same shape as
//! `pickPathIsTheOnlyPicker.test.ts` on the frontend side, which stops a new
//! call site from reintroducing the silent picker.
//!
//! It is deliberately a set equality and not a "no more than N" check, in both
//! directions: making an allowlisted command `async` fails too, and so does
//! converting a pending one without deleting its line. Neither list can rot
//! into a record of what used to be true, and neither can grow.
//!
//! The counting is worth a line of its own, because the number that started
//! this work was wrong. It was taken with a regex anchored at `pub fn`
//! immediately after the attribute, which silently skipped every command
//! declared inside a nested `mod` -- that is, most of `lib.rs`. It reported 38
//! synchronous commands. There are 82, out of 853.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Commands that are allowed to run on the main thread, with the reason.
///
/// The bar is: the work is bounded by our own code and cannot wait on anything
/// outside the process. Disk, keystore, D-Bus, clipboard, a child process, a
/// lock that some other command holds across such an operation, or a payload
/// whose size the caller chooses -- none of those qualify.
const MAIN_THREAD_ALLOWED: &[(&str, &str)] = &[
    (
        "aeroshare_notify",
        "must be on the main thread: it marshals the notification through \
         AppHandle::run_on_main_thread, and tauri-runtime-wry's send_user_message \
         runs the closure inline when the caller already is the main thread. \
         Making it async would only add a trip through the event loop without \
         moving any work off that thread.",
    ),
    (
        "compare_hashes",
        "constant-time compare of two hex strings, no I/O.",
    ),
    (
        "generate_password",
        "pure CPU; the command rejects length outside 8..=128 and clamps count \
         to 10 before doing any work, so the duration is bounded by its own \
         validation rather than by the caller.",
    ),
    (
        "generate_passphrase",
        "pure CPU; word_count is rejected outside 3..=24 and count is clamped \
         to 10.",
    ),
    (
        "calculate_entropy",
        "arithmetic over a character-group set the command builds itself.",
    ),
    (
        "native_rsync_feature_compiled",
        "returns cfg!(feature = \"aerorsync\"), a compile-time constant.",
    ),
    (
        "provider_arm_crypt_capability",
        "two atomic stores on already-managed state.",
    ),
    (
        "local_sync_cancel",
        "one atomic store; the cancellation is observed by the worker itself.",
    ),
];

/// Commands that are still synchronous and should not be.
///
/// This list is a **ratchet, frozen at the commit that introduced it**, and the
/// test below asserts the synchronous set is exactly `MAIN_THREAD_ALLOWED` plus
/// this one. That has three consequences, and they are the point:
///
///   * a **new** synchronous command is red immediately, so the count cannot
///     climb again by addition, which is how it got here;
///   * converting one of these without deleting its line is **also** red, so
///     the list has to shrink as the work lands and cannot drift into fiction;
///   * the list can never grow. Adding to it is not a way to get green.
///
/// Everything here does real blocking work on the main thread -- most of it is
/// the sync index / journal / profile / snapshot / cloud-config family, which
/// is JSON read and write against the config directory -- except for a handful
/// that touch GTK and will end up in `MAIN_THREAD_ALLOWED` instead once each
/// has been read and the reason written down. Neither outcome is "leave it".
const MAIN_THREAD_NOT_YET_MOVED: &[&str] = &[
    "speech_model_status",              // lib.rs:245
    "preview_provider_totp",            // lib.rs:467
    "portable_info",                    // lib.rs:2444
    "copy_to_clipboard",                // lib.rs:2463
    "log_update_detection",             // lib.rs:2607
    "restart_app",                      // lib.rs:3017
    "is_running_as_snap",               // lib.rs:5370
    "get_dependencies",                 // lib.rs:5406
    "get_system_info",                  // lib.rs:6059
    "safe_picker_start_dir",            // lib.rs:6458
    "set_close_to_tray",                // lib.rs:10906
    "is_autostart_launch",              // lib.rs:10916
    "toggle_menu_bar",                  // lib.rs:11159
    "rebuild_menu",                     // lib.rs:11170
    "get_compare_options_default",      // lib.rs:12532
    "load_sync_index_cmd",              // lib.rs:12537
    "save_sync_index_cmd",              // lib.rs:12547
    "load_sync_journal_cmd",            // lib.rs:12556
    "save_sync_journal_cmd",            // lib.rs:12566
    "delete_sync_journal_cmd",          // lib.rs:12573
    "list_sync_journals_cmd",           // lib.rs:12580
    "cleanup_old_journals_cmd",         // lib.rs:12585
    "clear_all_journals_cmd",           // lib.rs:12590
    "save_transfer_queue_journal_cmd",  // lib.rs:12597
    "load_transfer_queue_journal_cmd",  // lib.rs:12604
    "clear_transfer_queue_journal_cmd", // lib.rs:12610
    "load_sync_profiles_cmd",           // lib.rs:12615
    "save_sync_profile_cmd",            // lib.rs:12620
    "delete_sync_profile_cmd",          // lib.rs:12625
    "get_sync_schedule_cmd",            // lib.rs:12651
    "save_sync_schedule_cmd",           // lib.rs:12656
    "get_watcher_status_cmd",           // lib.rs:12661
    "get_multi_path_config",            // lib.rs:13062
    "save_multi_path_config_cmd",       // lib.rs:13067
    "add_path_pair",                    // lib.rs:13072
    "remove_path_pair",                 // lib.rs:13080
    "export_sync_template_cmd",         // lib.rs:13092
    "import_sync_template_cmd",         // lib.rs:13124
    "export_sync_script_cmd",           // lib.rs:13149
    "import_sync_script_cmd",           // lib.rs:13170
    "aerosync_export_script_cmd",       // lib.rs:13209
    "aerosync_import_script_cmd",       // lib.rs:13312
    "create_sync_snapshot_cmd",         // lib.rs:13350
    "list_sync_snapshots_cmd",          // lib.rs:13359
    "delete_sync_snapshot_cmd",         // lib.rs:13372
    "load_sync_snapshot_cmd",           // lib.rs:13385
    "get_default_retry_policy",         // lib.rs:14380
    "verify_local_transfer",            // lib.rs:14385
    "classify_transfer_error",          // lib.rs:14407
    "get_cloud_config",                 // lib.rs:14805
    "save_cloud_config_cmd",            // lib.rs:14810
    "get_cloud_pairs_config",           // lib.rs:14818
    "save_cloud_pairs_config_cmd",      // lib.rs:14823
    "add_cloud_pair",                   // lib.rs:14828
    "remove_cloud_pair",                // lib.rs:14841
    "update_cloud_pair",                // lib.rs:14853
    "update_excluded_folders",          // lib.rs:14868
    "list_file_versions",               // lib.rs:14875
    "list_all_file_versions",           // lib.rs:14882
    "restore_file_version",             // lib.rs:14889
    "cleanup_versions",                 // lib.rs:14919
    "versions_disk_usage",              // lib.rs:14926
    "archive_before_sync_delete",       // lib.rs:14935
    "get_cloud_status",                 // lib.rs:15080
    "enable_aerocloud",                 // lib.rs:15098
    "generate_share_link",              // lib.rs:15198
    "generate_share_link_remote",       // lib.rs:15236
    "generate_server_share_link",       // lib.rs:15272
    "get_default_cloud_folder",         // lib.rs:15317
    "update_conflict_strategy",         // lib.rs:15323
    "is_background_sync_running",       // lib.rs:16012
    "flatpak_config_import_status",     // lib.rs:16171
    "flatpak_config_import_apply",      // lib.rs:16186
    "mount_autostart_blocked",          // lib.rs:17516
];

/// One `#[tauri::command]` found in the sources.
#[derive(Debug)]
struct FoundCommand {
    name: String,
    is_async: bool,
    file: String,
    line: usize,
}

fn source_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .flatten();
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
    out.sort();
}

/// Collect the commands by reading the source text.
///
/// Reading the text rather than reflecting over the crate is on purpose: it
/// sees commands behind `#[cfg(...)]` that this build did not compile, which is
/// where a platform-specific or feature-gated command would otherwise hide. It
/// costs us a hand-rolled scan, which is why the scan is written to be
/// pedantic about what it accepts and to panic on anything it does not expect,
/// instead of silently skipping it.
fn collect_commands() -> Vec<FoundCommand> {
    let mut files = Vec::new();
    rust_sources(&source_root(), &mut files);

    let mut found = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", file.display()));
        let lines: Vec<&str> = text.lines().collect();
        let short = file
            .strip_prefix(source_root())
            .unwrap_or(file)
            .display()
            .to_string();

        for (idx, raw) in lines.iter().enumerate() {
            let line = raw.trim_start();
            if !line.starts_with("#[tauri::command") {
                continue;
            }
            // `#[tauri::command(async)]` is the other way to get off the main
            // thread, and it counts even when the function itself is sync.
            let attr_forces_async = line.contains("async");

            // Walk forward past any further attributes, doc comments and blank
            // lines to the signature this attribute belongs to.
            let mut cursor = idx + 1;
            let signature = loop {
                let Some(next) = lines.get(cursor) else {
                    panic!(
                        "{short}:{}: #[tauri::command] with no function after it",
                        idx + 1
                    );
                };
                let next = next.trim_start();
                if next.starts_with("#[")
                    || next.starts_with("//")
                    || next.starts_with("#!")
                    || next.is_empty()
                {
                    cursor += 1;
                    continue;
                }
                break next;
            };

            let (is_async, after_fn) = if let Some(rest) = signature.strip_prefix("pub async fn ") {
                (true, rest)
            } else if let Some(rest) = signature.strip_prefix("pub fn ") {
                (false, rest)
            } else if let Some(rest) = signature.strip_prefix("async fn ") {
                (true, rest)
            } else if let Some(rest) = signature.strip_prefix("fn ") {
                (false, rest)
            } else {
                panic!(
                    "{short}:{}: #[tauri::command] is followed by `{signature}`, which this \
                     scan does not recognise as a function signature. Either the command was \
                     written in a new shape or the scan needs teaching -- do not delete the \
                     check to make this go away.",
                    cursor + 1
                );
            };

            let name: String = after_fn
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            assert!(
                !name.is_empty(),
                "{short}:{}: could not read a command name out of `{signature}`",
                cursor + 1
            );

            found.push(FoundCommand {
                name,
                is_async: is_async || attr_forces_async,
                file: short.clone(),
                line: cursor + 1,
            });
        }
    }
    found
}

/// The scan has to actually find things; a regression that made it match
/// nothing would turn every assertion below into a silent pass.
#[test]
fn the_scan_finds_the_commands() {
    let found = collect_commands();
    assert!(
        found.len() > 500,
        "the command scan found only {} commands, which means it stopped matching, \
         not that the crate shrank",
        found.len()
    );
    assert!(
        found.iter().any(|c| c.name == "list_subdirectories"),
        "the scan did not find list_subdirectories"
    );
    assert!(
        found.iter().any(|c| c.name == "aeroshare_notify"),
        "the scan did not find aeroshare_notify"
    );
}

/// The pin itself.
#[test]
fn no_command_blocks_the_main_thread_unless_it_is_on_the_list() {
    let found = collect_commands();

    let justified: BTreeSet<&str> = MAIN_THREAD_ALLOWED.iter().map(|(name, _)| *name).collect();
    assert_eq!(
        justified.len(),
        MAIN_THREAD_ALLOWED.len(),
        "MAIN_THREAD_ALLOWED contains a duplicate name"
    );
    let not_yet: BTreeSet<&str> = MAIN_THREAD_NOT_YET_MOVED.iter().copied().collect();
    assert_eq!(
        not_yet.len(),
        MAIN_THREAD_NOT_YET_MOVED.len(),
        "MAIN_THREAD_NOT_YET_MOVED contains a duplicate name"
    );
    let overlap: Vec<&&str> = justified.intersection(&not_yet).collect();
    assert!(
        overlap.is_empty(),
        "{overlap:?} is both justified and pending, which cannot both be true"
    );
    let allowed: BTreeSet<&str> = justified.union(&not_yet).copied().collect();

    let sync_commands: Vec<&FoundCommand> = found.iter().filter(|c| !c.is_async).collect();
    let sync_names: BTreeSet<&str> = sync_commands.iter().map(|c| c.name.as_str()).collect();

    let unexpected: Vec<&&FoundCommand> = sync_commands
        .iter()
        .filter(|c| !allowed.contains(c.name.as_str()))
        .collect();
    if !unexpected.is_empty() {
        let listing = unexpected
            .iter()
            .map(|c| format!("  {}:{}  {}", c.file, c.line, c.name))
            .collect::<Vec<_>>()
            .join("\n");
        panic!(
            "these #[tauri::command]s are synchronous, so they run on the main thread \
             (the GTK thread on Linux) and block the whole window for as long as they \
             take:\n{listing}\n\n\
             Make each one `pub async fn` and put the blocking part inside \
             `tokio::task::spawn_blocking`, the way `filesystem::list_subdirectories` and \
             `portal_chooser::chooser_unavailable` do. Take any `std::sync::Mutex` guard \
             *inside* the closure: holding one across an `.await` is \
             `clippy::await_holding_lock`.\n\n\
             If a command genuinely has to stay on the main thread, or its work is bounded \
             by its own validation and touches nothing outside the process, add it to \
             MAIN_THREAD_ALLOWED in this file together with the reason.\n\n\
             MAIN_THREAD_NOT_YET_MOVED is not the place for it: that list is frozen and \
             only shrinks."
        );
    }

    let stale_justified: Vec<&str> = justified
        .iter()
        .filter(|name| !sync_names.contains(*name))
        .copied()
        .collect();
    assert!(
        stale_justified.is_empty(),
        "MAIN_THREAD_ALLOWED still lists {stale_justified:?}, but no synchronous command by \
         that name exists any more. Either it was made async -- in which case drop the entry, \
         the list is not a history -- or it was renamed or removed and the entry went stale."
    );

    let drained: Vec<&str> = not_yet
        .iter()
        .filter(|name| !sync_names.contains(*name))
        .copied()
        .collect();
    assert!(
        drained.is_empty(),
        "{drained:?} are no longer synchronous, which is the point -- now delete them from \
         MAIN_THREAD_NOT_YET_MOVED. The list has to shrink as the work lands, otherwise it \
         stops describing anything."
    );
}

/// Every entry has to carry a reason, because a bare name is how an allowlist
/// turns into a place to put things.
#[test]
fn every_allowlist_entry_explains_itself() {
    for (name, reason) in MAIN_THREAD_ALLOWED {
        assert!(
            reason.len() > 30,
            "MAIN_THREAD_ALLOWED entry `{name}` has no real reason attached"
        );
    }
}
