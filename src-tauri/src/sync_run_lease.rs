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
}

static LEASES: LazyLock<Mutex<HashMap<String, Lease>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn acquire_at(dir: &Path, pair: String) -> Result<String, String> {
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
        },
    );
    Ok(token)
}

pub(crate) fn acquire(local: &str, remote: &str) -> Result<String, String> {
    acquire_at(
        &crate::sync::sync_index_dir()?,
        crate::sync::index_filename(local, remote),
    )
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
        let valid = acquire_at(dir.path(), pair).unwrap();
        let foreign =
            acquire_at(dir.path(), crate::sync::index_filename("/other", "/other")).unwrap();
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
        let first = acquire_at(dir.path(), pair.clone()).unwrap();
        assert!(acquire_at(dir.path(), pair.clone()).is_err());
        assert!(guard("/other", "/remote", &first).is_err());
        let destination = dir.path().join("destination");
        std::fs::write(&destination, b"old upload completed").unwrap();
        release(&first).unwrap();
        let second = acquire_at(dir.path(), pair).unwrap();
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
        let first = acquire_at(dir.path(), pair.clone()).unwrap();
        let transaction = guard("/local2", "/remote2", &first).unwrap();
        release(&first).unwrap();
        assert!(acquire_at(dir.path(), pair.clone()).is_err());
        drop(transaction);
        let second = acquire_at(dir.path(), pair).unwrap();
        release(&second).unwrap();
    }
}
