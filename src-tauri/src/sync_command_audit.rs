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
//! synchronous command, and asserts the set is **exactly** `MAIN_THREAD_ALLOWED`
//! below. A new synchronous command is not in it and is red until somebody
//! either moves the work off the main thread or writes down why it belongs
//! there. Same shape as `pickPathIsTheOnlyPicker.test.ts` on the frontend side,
//! which stops a new call site from reintroducing the silent picker.
//!
//! Set equality in both directions, so the list cannot rot: an entry whose
//! command has become `async`, been renamed or been deleted also fails, which
//! keeps the list a description of the present rather than a history.
//!
//! The counting is worth a line of its own, because the number that started
//! this work was wrong. It was taken with a regex anchored at `pub fn`
//! immediately after the attribute, which silently skipped every command
//! declared inside a nested `mod` -- that is, most of `lib.rs`. It reported 38
//! synchronous commands. There were 82, out of 853. There are now 23, and every
//! one of them is here with its reason.
//!
//! Getting from 82 to 23 took two passes. The first shipped the commands that
//! do filesystem, keystore and clipboard work outside `lib.rs` and froze the
//! remainder in a second list that could only shrink; the second drained that
//! list and deleted it. The ratchet is gone because it is empty, which is the
//! only good reason to delete one.

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
        "constant-time compare of two hex strings the caller already sent over \
         IPC; no I/O and linear in a string the UI itself produced.",
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
        "arithmetic over a character-group set the command builds itself, so \
         its size is bounded by this file rather than by the caller.",
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
    (
        "restart_app",
        "must be on the main thread: it tears down the single-instance plugin \
         and calls AppHandle::restart, which drives the event loop.",
    ),
    (
        "toggle_menu_bar",
        "must be on the main thread: set_menu / remove_menu are GTK window \
         operations on Linux.",
    ),
    (
        "rebuild_menu",
        "must be on the main thread, and already knows it: MenuItem creation \
         and Drop touch GTK, so the body marshals the build through a channel \
         onto the main thread. tauri-runtime-wry's send_user_message runs that \
         closure inline when the caller already is the main thread, so making \
         the command async would add an event-loop round trip and move no work.",
    ),
    (
        "speech_model_status",
        "the macOS variant, which returns a constant struct because local STT \
         is not built there; the platforms that have a real implementation \
         already answer asynchronously.",
    ),
    (
        "log_update_detection",
        "writes one line to the log and returns; the tracing subscriber is \
         non-blocking.",
    ),
    (
        "is_running_as_snap",
        "reads the SNAP environment variable of this process; no syscall, and \
         the value cannot change while we run.",
    ),
    (
        "is_autostart_launch",
        "scans this process's own argv for --autostart.",
    ),
    (
        "set_close_to_tray",
        "one atomic store into a static the window-close handler reads; there \
         is nothing to wait for.",
    ),
    (
        "is_background_sync_running",
        "one atomic load from a static the background worker maintains; the \
         frontend polls it and must not pay a thread hop for a bool.",
    ),
    (
        "get_compare_options_default",
        "returns CompareOptions::default(), a struct of literals.",
    ),
    (
        "get_default_retry_policy",
        "returns RetryPolicy::default(), a struct of literals.",
    ),
    (
        "get_dependencies",
        "builds a Vec from version strings that build.rs baked in at compile \
         time; nothing is read at runtime.",
    ),
    (
        "generate_server_share_link",
        "string work only: prefix checks, trims, and percent-encoding of path \
         segments.",
    ),
    (
        "preview_provider_totp",
        "one HMAC over the current time step, on a secret already held in \
         memory; bounded and microseconds long.",
    ),
    (
        "classify_transfer_error",
        "lowercases an error string we produced ourselves and substring-matches \
         it against a fixed table.",
    ),
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
    let allowed = &justified;

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
             MAIN_THREAD_ALLOWED in this file together with the reason. A bare name with \
             no reason fails the next test down."
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
