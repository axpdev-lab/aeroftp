// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Keep a folder pair owned until every native command of its run settles.
//! OS locks exclude other windows/processes and release on process exit.
//! Lock files are never unlinked while in use.

use std::collections::HashMap;
use std::fs::File;
use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex};

struct Lease {
    pair: String,
    file: Arc<File>,
    /// Label of the window whose page began the run.
    window: String,
}

static LEASES: LazyLock<Mutex<HashMap<String, Lease>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Leases whose page is gone. Their tokens no longer open the intent writer,
/// and the folder pair stays locked until [`release_orphans`].
static ORPHANS: LazyLock<Mutex<HashMap<String, Lease>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn acquire_at(dir: &Path, pair: String, window: &str) -> Result<String, String> {
    let mut leases = LEASES
        .lock()
        .map_err(|_| "Sync lease registry unavailable")?;
    if leases.len() >= 256 {
        return Err("Too many active sync runs; finish them or restart the application".into());
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(dir.join(&pair).with_extension("run-lock"))
        .map_err(|_| "Cannot open sync run lock")?;
    file.try_lock().map_err(|_| "SYNC_RUN_BUSY: a sync still owns this folder pair; wait for it to finish or restart the application")?;
    let token = uuid::Uuid::new_v4().to_string();
    leases.insert(
        token.clone(),
        Lease {
            pair,
            file: Arc::new(file),
            window: window.to_owned(),
        },
    );
    Ok(token)
}

pub(crate) fn acquire(local: &str, remote: &str, window: &str) -> Result<String, String> {
    acquire_at(
        &crate::sync::sync_index_dir()?,
        crate::sync::index_filename(local, remote),
        window,
    )
}

/// The page of `window` was replaced (a reload, or a new web process after a
/// crash), so the script that would end its runs no longer exists. Take their
/// leases out of the registry without unlocking the pairs, and return the
/// tokens to hand to [`release_orphans`].
pub(crate) fn orphan_window(window: &str) -> Vec<String> {
    let (Ok(mut leases), Ok(mut orphans)) = (LEASES.lock(), ORPHANS.lock()) else {
        return Vec::new();
    };
    let tokens: Vec<String> = leases
        .iter()
        .filter(|(_, lease)| lease.window == window)
        .map(|(token, _)| token.clone())
        .collect();
    for token in &tokens {
        if let Some(lease) = leases.remove(token) {
            orphans.insert(token.clone(), lease);
        }
    }
    tokens
}

pub(crate) fn release_orphans(tokens: &[String]) {
    if let Ok(mut orphans) = ORPHANS.lock() {
        for token in tokens {
            orphans.remove(token);
        }
    }
}

/// Unlock orphaned pairs once `settled` says the native work the lost page
/// had started is over. Until then a new run of the same pair is refused.
pub(crate) async fn release_orphans_once_settled(tokens: Vec<String>, settled: impl Fn() -> bool) {
    while !settled() {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    release_orphans(&tokens);
}

pub(crate) fn guard(local: &str, remote: &str, token: &str) -> Result<Arc<File>, String> {
    let pair = crate::sync::index_filename(local, remote);
    let leases = LEASES
        .lock()
        .map_err(|_| "Sync lease registry unavailable")?;
    let lease = leases
        .get(token)
        .filter(|lease| lease.pair == pair)
        .ok_or("SYNC_RUN_BUSY: this run no longer owns the folder pair")?;
    Ok(lease.file.clone())
}

pub(crate) fn release(token: &str) -> Result<(), String> {
    LEASES
        .lock()
        .map_err(|_| "Sync lease registry unavailable")?
        .remove(token);
    Ok(())
}

/// Validate before calling the native intent writer, keeping the lock through it.
pub(crate) fn with_guard<T>(
    local: &str,
    remote: &str,
    token: &str,
    write: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let _lease = guard(local, remote, token)?;
    write()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_and_foreign_tokens_cannot_enter_the_native_intent_writer() {
        let dir = tempfile::tempdir().unwrap();
        let pair = crate::sync::index_filename("/protected-local", "/protected-remote");
        let valid = acquire_at(dir.path(), pair, "main").unwrap();
        let foreign = acquire_at(
            dir.path(),
            crate::sync::index_filename("/other", "/other"),
            "main",
        )
        .unwrap();
        let intent = dir.path().join("intent");
        std::fs::write(&intent, b"original durable intent").unwrap();
        for token in ["", "unknown", foreign.as_str()] {
            assert!(
                with_guard("/protected-local", "/protected-remote", token, || {
                    std::fs::write(&intent, b"overwritten intent").map_err(|e| e.to_string())
                })
                .is_err()
            );
            assert_eq!(std::fs::read(&intent).unwrap(), b"original durable intent");
        }
        with_guard("/protected-local", "/protected-remote", &valid, || {
            std::fs::write(&intent, b"authorized intent").map_err(|e| e.to_string())
        })
        .unwrap();
        assert_eq!(std::fs::read(&intent).unwrap(), b"authorized intent");
        release(&valid).unwrap();
        release(&foreign).unwrap();
    }

    #[test]
    fn replacement_cannot_start_before_the_previous_native_work_finishes() {
        let dir = tempfile::tempdir().unwrap();
        let pair = crate::sync::index_filename("/local", "/remote");
        let first = acquire_at(dir.path(), pair.clone(), "main").unwrap();
        assert!(acquire_at(dir.path(), pair.clone(), "main").is_err());
        assert!(guard("/other", "/remote", &first).is_err());
        let destination = dir.path().join("destination");
        std::fs::write(&destination, b"old upload completed").unwrap();
        release(&first).unwrap();
        let second = acquire_at(dir.path(), pair, "main").unwrap();
        std::fs::write(&destination, b"replacement upload completed").unwrap();
        assert_eq!(
            std::fs::read(&destination).unwrap(),
            b"replacement upload completed"
        );
        release(&second).unwrap();
    }

    #[test]
    fn an_intent_transaction_keeps_its_lock_even_if_the_handle_is_released() {
        let dir = tempfile::tempdir().unwrap();
        let pair = crate::sync::index_filename("/local2", "/remote2");
        let first = acquire_at(dir.path(), pair.clone(), "main").unwrap();
        let transaction = guard("/local2", "/remote2", &first).unwrap();
        release(&first).unwrap();
        assert!(acquire_at(dir.path(), pair.clone(), "main").is_err());
        drop(transaction);
        let second = acquire_at(dir.path(), pair, "main").unwrap();
        release(&second).unwrap();
    }
    #[tokio::test]
    async fn a_lost_page_keeps_its_pair_locked_until_native_work_settles() {
        let dir = tempfile::tempdir().unwrap();
        let pair = crate::sync::index_filename("/lost-local", "/lost-remote");
        let lost = acquire_at(dir.path(), pair.clone(), "lost-window").unwrap();
        let other_window = acquire_at(
            dir.path(),
            crate::sync::index_filename("/kept-local", "/kept-remote"),
            "kept-window",
        )
        .unwrap();
        // An accepted native command of the lost run still holds its pin.
        let state = std::sync::Arc::new(crate::provider_commands::ProviderState::new());
        let pin = state.pin_gui_target(Some(0)).await.unwrap();
        assert!(!state.gui_work_settled());

        let orphans = orphan_window("lost-window");
        assert_eq!(orphans, std::slice::from_ref(&lost));
        assert!(
            guard("/lost-local", "/lost-remote", &lost).is_err(),
            "the lost page can no longer open the intent writer"
        );
        assert!(guard("/kept-local", "/kept-remote", &other_window).is_ok());

        let waiter = {
            let state = std::sync::Arc::clone(&state);
            tokio::spawn(release_orphans_once_settled(orphans, move || {
                state.gui_work_settled()
            }))
        };
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        assert!(!waiter.is_finished());
        assert!(
            acquire_at(dir.path(), pair.clone(), "new-window").is_err(),
            "a replacement run must wait for the pending native work"
        );

        drop(pin);
        waiter.await.unwrap();
        let replacement = acquire_at(dir.path(), pair, "new-window").unwrap();
        release(&replacement).unwrap();
        release(&other_window).unwrap();
    }
}
