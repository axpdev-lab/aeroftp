// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! The documents a portable update installs beside the exes: the marker,
//! README, LICENSE and third-party notices.
//!
//! A batch of documents is a folder beside the exes holding the documents and
//! the SHA-256 of the app exe they belong to. Two places hold one each:
//!
//! - the next batch (`.aeroftp-pending-docs.next`): staged by the running app
//!   for the update it is about to hand to the helper, before the exes swap;
//! - the current batch (`.aeroftp-pending-docs`): the running exe's documents
//!   not installed yet (one an editor held). The About dialog opens its
//!   notices first.
//!
//! Staging writes the next batch only, under a temporary name renamed into
//! place, so it is whole or absent, and never touches the current batch. At
//! its start an app promotes a next batch with its own key to the current
//! batch, then renames each document over the installed one, an operation on
//! that exact path that Windows refuses while another process holds the file
//! and that never writes into a directory with the document's name. A
//! document still held stays in the current batch: the held ones share one
//! retry budget, and what is left is installed at a later start. A batch of
//! another exe is removed (an update whose swap failed, a folder replaced by
//! hand), except a next batch the running app staged itself: its update is
//! waiting for the swap. Staging, discarding and each install pass hold one
//! lock per exe folder, so none of them sees another half done. Every
//! outcome is logged as what it is.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

/// The running exe's batch, beside the exes.
pub(crate) const PENDING_DIR: &str = ".aeroftp-pending-docs";
/// The batch staged for an update waiting for the exes to swap.
pub(crate) const NEXT_DIR: &str = ".aeroftp-pending-docs.next";
/// The key of the exe a batch belongs to.
const KEY_FILE: &str = "exe.key";
/// The name a staging folder has until it is whole.
const TMP_PREFIX: &str = ".aeroftp-pending-docs.tmp-";
/// The documents, in the order they are installed.
pub(crate) const DOCS: [&str; 4] = [
    "portable.marker",
    "README.txt",
    "LICENSE.txt",
    "THIRD-PARTY-NOTICES.md",
];

/// Windows error codes of a rename refused because another process holds
/// the target (`ERROR_SHARING_VIOLATION`, `ERROR_LOCK_VIOLATION`) or because
/// access is denied (`ERROR_ACCESS_DENIED`: a handle without delete sharing,
/// a read-only file, an ACL). The module runs on Windows only.
const ERROR_ACCESS_DENIED: i32 = 5;
const ERROR_SHARING_VIOLATION: i32 = 32;
const ERROR_LOCK_VIOLATION: i32 = 33;

/// What one attempt did with a pending document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// The new document replaced the installed one (or created it).
    Installed,
    /// A directory has the document's name: nothing was written into it,
    /// and the pending copy is dropped.
    Directory,
    /// Another process holds the installed document: it is kept, and the
    /// new one stays pending.
    InUse,
    /// Access to the installed document was denied: it is kept, and the new
    /// one stays pending.
    AccessDenied,
    /// The replace failed for another reason, named here: the installed
    /// document is kept and the pending copy is dropped.
    Failed(String),
}

impl Outcome {
    /// Whether the document is still pending after this attempt.
    pub(crate) fn waits(&self) -> bool {
        matches!(self, Outcome::InUse | Outcome::AccessDenied)
    }
}

/// What an install pass found.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Pending {
    /// No batch of this exe, and nothing removed.
    None,
    /// Only batches of another exe, or folders that are no batch: removed.
    Stale,
    /// The outcome of each document of this exe's batch, in install order.
    Docs(Vec<(&'static str, Outcome)>),
    /// The lock of the exe folder could not be taken: nothing was touched.
    Unavailable(String),
}

/// The key of an exe: the hex SHA-256 of its bytes.
pub(crate) fn exe_key(exe: &Path) -> io::Result<String> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(exe)?;
    let mut hasher = Sha256::new();
    io::copy(&mut file, &mut hasher)?;
    Ok(hex::encode(hasher.finalize()))
}

/// The exe folders this process staged a next batch in: their next batch
/// belongs to the update this process hands over, not to a failed swap.
static STAGED_HERE: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

fn staged_here(exe_dir: &Path) -> bool {
    STAGED_HERE
        .lock()
        .map(|dirs| dirs.iter().any(|d| d == exe_dir))
        .unwrap_or(false)
}

fn mark_staged_here(exe_dir: &Path) {
    if let Ok(mut dirs) = STAGED_HERE.lock() {
        if !dirs.iter().any(|d| d == exe_dir) {
            dirs.push(exe_dir.to_path_buf());
        }
    }
}

/// A name for the lock of one exe folder, the same in every process.
fn folder_id(exe_dir: &Path) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(
        exe_dir.to_string_lossy().to_lowercase().as_bytes(),
    ))
}

/// The lock of the batches of one exe folder, held until dropped. On
/// Windows a named mutex, which leaves no file in the portable folder.
struct BatchLock {
    #[cfg(windows)]
    handle: windows::Win32::Foundation::HANDLE,
    #[cfg(not(windows))]
    _file: std::fs::File,
}

#[cfg(windows)]
fn lock_batches(exe_dir: &Path) -> io::Result<BatchLock> {
    use windows::core::HSTRING;
    use windows::Win32::Foundation::{CloseHandle, BOOL, WAIT_ABANDONED, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{CreateMutexW, WaitForSingleObject, INFINITE};
    let name = HSTRING::from(format!(
        "Local\\AeroFTP-pending-docs-{}",
        folder_id(exe_dir)
    ));
    // SAFETY: a named mutex with default security; the handle is owned by
    // the returned lock, which releases and closes it.
    let handle =
        unsafe { CreateMutexW(None, BOOL::from(false), &name) }.map_err(io::Error::other)?;
    // SAFETY: `handle` is the mutex just opened. An abandoned mutex (a
    // process that died holding it) is owned by this thread as well.
    let wait = unsafe { WaitForSingleObject(handle, INFINITE) };
    if wait == WAIT_OBJECT_0 || wait == WAIT_ABANDONED {
        Ok(BatchLock { handle })
    } else {
        let error = io::Error::last_os_error();
        // SAFETY: closes the handle this function opened and does not own.
        let _ = unsafe { CloseHandle(handle) };
        Err(error)
    }
}

#[cfg(windows)]
impl Drop for BatchLock {
    fn drop(&mut self) {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::ReleaseMutex;
        // SAFETY: this thread owns the mutex since `lock_batches`.
        unsafe {
            let _ = ReleaseMutex(self.handle);
            let _ = CloseHandle(self.handle);
        }
    }
}

/// The tests on other systems lock a file in the temporary folder, by name.
#[cfg(not(windows))]
fn lock_batches(exe_dir: &Path) -> io::Result<BatchLock> {
    let path =
        std::env::temp_dir().join(format!("aeroftp-pending-docs-{}.lock", folder_id(exe_dir)));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)?;
    file.lock()?;
    Ok(BatchLock { _file: file })
}

/// What a batch folder holds.
enum Slot {
    Missing,
    /// A file, or a folder without a key: no batch.
    Invalid,
    Key(String),
}

fn slot(dir: &Path) -> Slot {
    match std::fs::symlink_metadata(dir) {
        Err(_) => Slot::Missing,
        Ok(meta) if !meta.is_dir() => Slot::Invalid,
        Ok(_) => match std::fs::read_to_string(dir.join(KEY_FILE)) {
            Ok(key) => Slot::Key(key.trim().to_string()),
            Err(_) => Slot::Invalid,
        },
    }
}

/// Removes staging folders a stopped update left (`TMP_PREFIX*`). Called
/// under the lock only, so no staging is in progress.
fn remove_staging_leftovers(exe_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(exe_dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with(TMP_PREFIX) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Removes whatever is at `path`: a folder with its content, or a file.
/// Whether something was there.
fn remove_any(path: &Path) -> bool {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => {
            let _ = std::fs::remove_dir_all(path);
            true
        }
        Ok(_) => {
            let _ = std::fs::remove_file(path);
            true
        }
        Err(_) => false,
    }
}

/// Stages `docs` (source, document name) as the next batch of `exe_dir`, for
/// the exe at `new_exe`. The current batch is never touched; a next batch of
/// another exe (a staging whose swap never happened) is replaced.
pub(crate) fn stage_pending_docs(
    exe_dir: &Path,
    new_exe: &Path,
    docs: &[(&Path, &'static str)],
    suffix: &str,
) -> Result<(), String> {
    stage_with(exe_dir, new_exe, docs, suffix, |from, to| {
        std::fs::rename(from, to)
    })
}

/// `stage_pending_docs` with the publishing rename given.
fn stage_with(
    exe_dir: &Path,
    new_exe: &Path,
    docs: &[(&Path, &'static str)],
    suffix: &str,
    publish: impl Fn(&Path, &Path) -> io::Result<()>,
) -> Result<(), String> {
    if docs.is_empty() {
        return Ok(());
    }
    let fail = |e: io::Error| format!("Failed to stage the portable documents: {e}");
    let key = exe_key(new_exe).map_err(fail)?;
    let _lock = lock_batches(exe_dir).map_err(fail)?;
    mark_staged_here(exe_dir);
    remove_staging_leftovers(exe_dir);
    let next = exe_dir.join(NEXT_DIR);
    if matches!(slot(&next), Slot::Key(k) if k == key) {
        // Already staged for this exe, whole.
        return Ok(());
    }
    let tmp = exe_dir.join(format!("{TMP_PREFIX}{suffix}"));
    let staged = (|| -> io::Result<()> {
        // A folder this call creates: nothing else is written into.
        std::fs::create_dir(&tmp)?;
        for (source, name) in docs {
            std::fs::copy(source, tmp.join(name))?;
        }
        std::fs::write(tmp.join(KEY_FILE), &key)?;
        remove_any(&next);
        publish(&tmp, &next)
    })();
    staged.map_err(|e| {
        let _ = std::fs::remove_dir_all(&tmp);
        fail(e)
    })
}

/// Removes the next batch of `exe_dir` and any staging leftover: the update
/// did not go through. The current batch stays.
pub(crate) fn discard_pending_docs(exe_dir: &Path) {
    let Ok(_lock) = lock_batches(exe_dir) else {
        return;
    };
    remove_staging_leftovers(exe_dir);
    remove_any(&exe_dir.join(NEXT_DIR));
}

/// The outcome of a failed replace of `target`.
fn classify(target: &Path, e: &io::Error) -> Outcome {
    if target.is_dir() {
        return Outcome::Directory;
    }
    match e.raw_os_error() {
        Some(ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION) => Outcome::InUse,
        Some(ERROR_ACCESS_DENIED) => Outcome::AccessDenied,
        _ => Outcome::Failed(e.to_string()),
    }
}

/// One install pass, under the lock, for the exe whose key is `own_key`.
/// Returns what it found and the folder it installed from.
fn install_pass(
    exe_dir: &Path,
    own_key: &str,
    replace: impl Fn(&Path, &Path) -> io::Result<()>,
) -> (Pending, Option<PathBuf>) {
    let _lock = match lock_batches(exe_dir) {
        Ok(lock) => lock,
        Err(e) => return (Pending::Unavailable(e.to_string()), None),
    };
    remove_staging_leftovers(exe_dir);
    let current = exe_dir.join(PENDING_DIR);
    let next = exe_dir.join(NEXT_DIR);
    let mut stale = false;
    let mut source = current.clone();
    match slot(&next) {
        Slot::Missing => {}
        Slot::Key(k) if k == own_key => {
            // This exe's documents supersede an older exe's.
            stale |= remove_any(&current);
            if std::fs::rename(&next, &current).is_err() {
                source = next.clone();
            }
        }
        // The update this process staged, waiting for the swap.
        Slot::Key(_) if staged_here(exe_dir) => {}
        Slot::Key(_) | Slot::Invalid => stale |= remove_any(&next),
    }
    match slot(&source) {
        Slot::Missing => {
            let found = if stale { Pending::Stale } else { Pending::None };
            return (found, None);
        }
        Slot::Key(k) if k == own_key => {}
        Slot::Key(_) | Slot::Invalid => {
            remove_any(&source);
            return (Pending::Stale, None);
        }
    }
    let mut outcomes = Vec::new();
    for name in DOCS {
        let pending = source.join(name);
        if !pending.is_file() {
            continue;
        }
        let target = exe_dir.join(name);
        // A directory found first is never touched; one that appears later
        // makes the rename fail, without writing into it.
        let outcome = if target.is_dir() {
            Outcome::Directory
        } else {
            match replace(&pending, &target) {
                Ok(()) => Outcome::Installed,
                Err(e) => classify(&target, &e),
            }
        };
        if matches!(outcome, Outcome::Directory | Outcome::Failed(_)) {
            let _ = std::fs::remove_file(&pending);
        }
        outcomes.push((name, outcome));
    }
    if !DOCS.iter().any(|name| source.join(name).is_file()) {
        let _ = std::fs::remove_dir_all(&source);
    }
    (Pending::Docs(outcomes), Some(source))
}

/// One install pass over the batches of `exe_dir` for the exe whose key is
/// `own_key`. `replace` moves a pending document over the installed one
/// (`std::fs::rename` in production).
pub(crate) fn install_pending_docs(
    exe_dir: &Path,
    own_key: &str,
    replace: impl Fn(&Path, &Path) -> io::Result<()>,
) -> Pending {
    install_pass(exe_dir, own_key, replace).0
}

/// After a first pass that found `first`, the held documents share `rounds`
/// more passes, `pause` apart (`sleep`), however many of them are held.
/// Returns the last outcome of each document; a held one that another pass
/// installed meanwhile is not reported.
pub(crate) fn retry_held(
    exe_dir: &Path,
    own_key: &str,
    first: Pending,
    rounds: u32,
    pause: Duration,
    replace: impl Fn(&Path, &Path) -> io::Result<()>,
    sleep: impl Fn(Duration),
) -> Pending {
    let Pending::Docs(mut last) = first else {
        return first;
    };
    let mut left = rounds;
    while left > 0 && last.iter().any(|(_, outcome)| outcome.waits()) {
        left -= 1;
        sleep(pause);
        let (retried, source) = install_pass(exe_dir, own_key, &replace);
        if let Pending::Docs(retried) = &retried {
            for (name, outcome) in retried {
                if let Some(entry) = last.iter_mut().find(|(n, _)| n == name) {
                    entry.1 = outcome.clone();
                }
            }
        }
        // A waiting document no longer in this exe's batch was handled by
        // another pass: this one has nothing true to say about it.
        last.retain(|(name, outcome)| {
            !outcome.waits() || source.as_ref().is_some_and(|s| s.join(name).is_file())
        });
        if !matches!(retried, Pending::Docs(_)) {
            break;
        }
    }
    Pending::Docs(last)
}

/// A first pass, then `retry_held`: what a start does, in one call for the
/// tests (the start runs the first pass before the window opens).
#[cfg(test)]
pub(crate) fn install_with_shared_budget(
    exe_dir: &Path,
    own_key: &str,
    rounds: u32,
    pause: Duration,
    replace: impl Fn(&Path, &Path) -> io::Result<()>,
    sleep: impl Fn(Duration),
) -> Pending {
    let first = install_pending_docs(exe_dir, own_key, &replace);
    retry_held(exe_dir, own_key, first, rounds, pause, replace, sleep)
}

/// The log line of an outcome, as it happened.
pub(crate) fn outcome_line(name: &str, outcome: &Outcome) -> String {
    match outcome {
        Outcome::Installed => format!("portable update: {name} installed"),
        Outcome::Directory => {
            format!("portable update: {name} is a directory, nothing was written into it")
        }
        Outcome::InUse => format!(
            "portable update: {name} is in use, kept the installed one; the new one is installed at a later start"
        ),
        Outcome::AccessDenied => format!(
            "portable update: access to {name} was denied, kept the installed one; the new one is tried again at a later start"
        ),
        Outcome::Failed(e) => {
            format!("portable update: {name} could not be replaced ({e}), kept the installed one")
        }
    }
}

/// The pending notices beside `exe_dir`, which the About dialog opens first.
#[cfg(test)]
pub(crate) fn pending_notices(exe_dir: &Path) -> PathBuf {
    exe_dir.join(PENDING_DIR).join("THIRD-PARTY-NOTICES.md")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    const KEY: &str = "new-exe-key";

    /// An app folder with installed documents and a pending folder for the
    /// exe whose key is `key` (none: no key file).
    fn folder(key: Option<&str>) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp dir");
        let pending = dir.path().join(PENDING_DIR);
        std::fs::create_dir(&pending).expect("pending dir");
        for name in DOCS {
            std::fs::write(dir.path().join(name), format!("old {name}")).expect("installed");
            std::fs::write(pending.join(name), format!("new {name}")).expect("pending");
        }
        if let Some(key) = key {
            std::fs::write(pending.join(KEY_FILE), format!("{key}\r\n")).expect("key");
        }
        dir
    }

    fn read(dir: &Path, name: &str) -> String {
        std::fs::read_to_string(dir.join(name)).expect("document")
    }

    fn rename(from: &Path, to: &Path) -> io::Result<()> {
        std::fs::rename(from, to)
    }

    /// A replace that Windows refuses with `code` for the documents in `held`.
    fn held<'a>(
        held: &'a RefCell<Vec<&'static str>>,
        code: i32,
    ) -> impl Fn(&Path, &Path) -> io::Result<()> + 'a {
        move |from: &Path, to: &Path| {
            let name = to.file_name().unwrap().to_string_lossy().to_string();
            if held.borrow().iter().any(|h| *h == name) {
                Err(io::Error::from_raw_os_error(code))
            } else {
                rename(from, to)
            }
        }
    }

    #[test]
    fn every_pending_document_is_installed_and_the_folder_goes() {
        let dir = folder(Some(KEY));
        let outcome = install_pending_docs(dir.path(), KEY, rename);
        assert_eq!(
            outcome,
            Pending::Docs(DOCS.iter().map(|n| (*n, Outcome::Installed)).collect())
        );
        for name in DOCS {
            assert_eq!(read(dir.path(), name), format!("new {name}"));
        }
        assert!(!dir.path().join(PENDING_DIR).exists());
        assert_eq!(install_pending_docs(dir.path(), KEY, rename), Pending::None);
    }

    #[test]
    fn a_held_document_waits_until_it_is_released_and_the_others_install() {
        let dir = folder(Some(KEY));
        let busy = RefCell::new(vec!["THIRD-PARTY-NOTICES.md"]);
        let Pending::Docs(first) =
            install_pending_docs(dir.path(), KEY, held(&busy, ERROR_SHARING_VIOLATION))
        else {
            panic!("docs");
        };
        assert_eq!(first[3], ("THIRD-PARTY-NOTICES.md", Outcome::InUse));
        assert_eq!(
            read(dir.path(), "THIRD-PARTY-NOTICES.md"),
            "old THIRD-PARTY-NOTICES.md"
        );
        assert_eq!(read(dir.path(), "README.txt"), "new README.txt");
        assert!(
            pending_notices(dir.path()).is_file(),
            "About opens the pending notices meanwhile"
        );
        busy.borrow_mut().clear();
        assert_eq!(
            install_pending_docs(dir.path(), KEY, held(&busy, ERROR_SHARING_VIOLATION)),
            Pending::Docs(vec![("THIRD-PARTY-NOTICES.md", Outcome::Installed)])
        );
        assert_eq!(
            read(dir.path(), "THIRD-PARTY-NOTICES.md"),
            "new THIRD-PARTY-NOTICES.md"
        );
        assert!(!dir.path().join(PENDING_DIR).exists());
    }

    #[test]
    fn held_documents_share_one_retry_budget_and_a_writable_one_installs_at_once() {
        let dir = folder(Some(KEY));
        // README and LICENSE stay held; the notices are writable.
        let busy = RefCell::new(vec!["README.txt", "LICENSE.txt"]);
        let pauses = Cell::new(0);
        let outcome = install_with_shared_budget(
            dir.path(),
            KEY,
            3,
            Duration::from_secs(1),
            held(&busy, ERROR_SHARING_VIOLATION),
            |_| pauses.set(pauses.get() + 1),
        );
        assert_eq!(
            pauses.get(),
            3,
            "two held documents use one budget, not one each"
        );
        assert_eq!(
            outcome,
            Pending::Docs(vec![
                ("portable.marker", Outcome::Installed),
                ("README.txt", Outcome::InUse),
                ("LICENSE.txt", Outcome::InUse),
                ("THIRD-PARTY-NOTICES.md", Outcome::Installed),
            ])
        );
        assert_eq!(
            read(dir.path(), "THIRD-PARTY-NOTICES.md"),
            "new THIRD-PARTY-NOTICES.md"
        );
        // Held forever: they stay pending for the next start, which installs
        // them once they are released.
        assert!(dir.path().join(PENDING_DIR).join("README.txt").is_file());
        busy.borrow_mut().clear();
        assert_eq!(
            install_pending_docs(dir.path(), KEY, rename),
            Pending::Docs(vec![
                ("README.txt", Outcome::Installed),
                ("LICENSE.txt", Outcome::Installed)
            ])
        );
    }

    #[test]
    fn the_budget_stops_as_soon_as_the_lock_clears() {
        let dir = folder(Some(KEY));
        let tries = Cell::new(0);
        let replace = |from: &Path, to: &Path| {
            if to.ends_with("LICENSE.txt") {
                tries.set(tries.get() + 1);
                if tries.get() < 3 {
                    return Err(io::Error::from_raw_os_error(ERROR_LOCK_VIOLATION));
                }
            }
            rename(from, to)
        };
        let pauses = Cell::new(0);
        let outcome =
            install_with_shared_budget(dir.path(), KEY, 10, Duration::ZERO, replace, |_| {
                pauses.set(pauses.get() + 1)
            });
        assert_eq!(pauses.get(), 2);
        let Pending::Docs(outcomes) = outcome else {
            panic!("docs")
        };
        assert!(outcomes.iter().all(|(_, o)| *o == Outcome::Installed));
        assert!(!dir.path().join(PENDING_DIR).exists());
    }

    #[test]
    fn a_held_document_another_start_installed_is_not_reported_by_this_one() {
        // Another start of the app installs the held document during this
        // one's pause: this pass stops waiting for it and does not call it in
        // use.
        let dir = folder(Some(KEY));
        let busy = RefCell::new(vec!["LICENSE.txt"]);
        let other_start = || {
            let pending = dir.path().join(PENDING_DIR);
            if pending.join("LICENSE.txt").is_file() {
                rename(
                    &pending.join("LICENSE.txt"),
                    &dir.path().join("LICENSE.txt"),
                )
                .unwrap();
            }
        };
        let pauses = Cell::new(0);
        let outcome = install_with_shared_budget(
            dir.path(),
            KEY,
            5,
            Duration::ZERO,
            held(&busy, ERROR_SHARING_VIOLATION),
            |_| {
                pauses.set(pauses.get() + 1);
                other_start()
            },
        );
        assert_eq!(pauses.get(), 1, "the budget ends once nothing waits");
        let Pending::Docs(outcomes) = outcome else {
            panic!("docs")
        };
        assert_eq!(
            outcomes.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
            vec!["portable.marker", "README.txt", "THIRD-PARTY-NOTICES.md"]
        );
        assert!(outcomes.iter().all(|(_, o)| *o == Outcome::Installed));
        assert_eq!(read(dir.path(), "LICENSE.txt"), "new LICENSE.txt");
        assert!(!dir.path().join(PENDING_DIR).exists());
    }

    #[test]
    fn a_directory_at_a_document_path_is_never_written_into() {
        // Found before the attempt, empty or not.
        for nested in [true, false] {
            let dir = folder(Some(KEY));
            let target = dir.path().join("THIRD-PARTY-NOTICES.md");
            std::fs::remove_file(&target).unwrap();
            std::fs::create_dir(&target).unwrap();
            if nested {
                std::fs::write(target.join("THIRD-PARTY-NOTICES.md"), b"sentinel").unwrap();
            }
            let Pending::Docs(outcomes) = install_pending_docs(dir.path(), KEY, rename) else {
                panic!("docs")
            };
            assert_eq!(outcomes[3], ("THIRD-PARTY-NOTICES.md", Outcome::Directory));
            let inside: Vec<_> = std::fs::read_dir(&target)
                .unwrap()
                .flatten()
                .map(|e| e.file_name())
                .collect();
            assert_eq!(inside.len(), usize::from(nested));
            if nested {
                assert_eq!(
                    std::fs::read(target.join("THIRD-PARTY-NOTICES.md")).unwrap(),
                    b"sentinel"
                );
            }
            assert_eq!(read(dir.path(), "README.txt"), "new README.txt");
            assert!(
                !dir.path().join(PENDING_DIR).exists(),
                "nothing is left pending"
            );
        }
    }

    #[test]
    fn a_directory_that_appears_between_attempts_is_reported_and_untouched() {
        let dir = folder(Some(KEY));
        let target = dir.path().join("LICENSE.txt");
        // The first attempt finds the document held; during the pause a
        // directory with a file inside replaces it; the retry finds the
        // directory before writing anything.
        let busy = RefCell::new(vec!["LICENSE.txt"]);
        let replace = held(&busy, ERROR_SHARING_VIOLATION);
        let swap = |_: Duration| {
            if !target.is_dir() {
                std::fs::remove_file(&target).unwrap();
                std::fs::create_dir(&target).unwrap();
                std::fs::write(target.join("LICENSE.txt"), b"sentinel").unwrap();
            }
        };
        let outcome = install_with_shared_budget(dir.path(), KEY, 2, Duration::ZERO, replace, swap);
        let Pending::Docs(outcomes) = outcome else {
            panic!("docs")
        };
        assert_eq!(outcomes[2], ("LICENSE.txt", Outcome::Directory));
        assert_eq!(
            std::fs::read(target.join("LICENSE.txt")).unwrap(),
            b"sentinel"
        );
        assert!(!dir.path().join(PENDING_DIR).exists());
    }

    #[test]
    fn a_directory_that_appears_during_the_rename_is_reported_as_a_directory() {
        let dir = folder(Some(KEY));
        let target = dir.path().join("LICENSE.txt");
        // The rename fails because a directory took the path at that moment
        // (Windows: access denied): the outcome says directory, not denied.
        let replace = |from: &Path, to: &Path| {
            if to == target {
                std::fs::remove_file(to).unwrap();
                std::fs::create_dir(to).unwrap();
                return Err(io::Error::from_raw_os_error(ERROR_ACCESS_DENIED));
            }
            rename(from, to)
        };
        let Pending::Docs(outcomes) = install_pending_docs(dir.path(), KEY, replace) else {
            panic!("docs")
        };
        assert_eq!(outcomes[2], ("LICENSE.txt", Outcome::Directory));
        assert!(target.is_dir());
    }

    #[test]
    fn a_failure_that_is_not_a_lock_is_named_and_the_installed_document_kept() {
        let dir = folder(Some(KEY));
        let replace = |from: &Path, to: &Path| {
            if to.ends_with("README.txt") {
                return Err(io::Error::new(
                    io::ErrorKind::StorageFull,
                    "There is not enough space on the disk.",
                ));
            }
            rename(from, to)
        };
        let Pending::Docs(outcomes) = install_pending_docs(dir.path(), KEY, replace) else {
            panic!("docs")
        };
        assert_eq!(
            outcomes[1],
            (
                "README.txt",
                Outcome::Failed("There is not enough space on the disk.".into())
            )
        );
        assert_eq!(read(dir.path(), "README.txt"), "old README.txt");
        assert_eq!(
            outcome_line("README.txt", &outcomes[1].1),
            "portable update: README.txt could not be replaced (There is not enough space on the disk.), kept the installed one"
        );
        assert!(!outcome_line("README.txt", &outcomes[1].1).contains("in use"));
        // Access denied is said as such, and waits.
        let dir = folder(Some(KEY));
        let busy = RefCell::new(vec!["README.txt"]);
        let Pending::Docs(outcomes) =
            install_pending_docs(dir.path(), KEY, held(&busy, ERROR_ACCESS_DENIED))
        else {
            panic!("docs")
        };
        assert_eq!(outcomes[1], ("README.txt", Outcome::AccessDenied));
        assert!(outcome_line("README.txt", &Outcome::AccessDenied)
            .contains("access to README.txt was denied"));
    }

    #[test]
    fn a_folder_of_another_exe_without_a_key_or_that_is_a_file_is_removed() {
        for key in [Some("other-exe"), None] {
            let dir = folder(key);
            let replace = |_: &Path, _: &Path| -> io::Result<()> {
                panic!("a stale document is never installed")
            };
            assert_eq!(
                install_pending_docs(dir.path(), KEY, replace),
                Pending::Stale
            );
            assert!(!dir.path().join(PENDING_DIR).exists(), "{key:?}");
            assert_eq!(read(dir.path(), "README.txt"), "old README.txt");
        }
        // A next batch of another exe that this process did not stage.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(NEXT_DIR)).unwrap();
        std::fs::write(dir.path().join(NEXT_DIR).join(KEY_FILE), "other-exe").unwrap();
        assert_eq!(
            install_pending_docs(dir.path(), KEY, rename),
            Pending::Stale
        );
        assert!(!dir.path().join(NEXT_DIR).exists());
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(PENDING_DIR), b"not a folder").unwrap();
        assert_eq!(
            install_pending_docs(dir.path(), KEY, rename),
            Pending::Stale
        );
        assert!(!dir.path().join(PENDING_DIR).exists());
    }

    /// An exe in its own folder, and a README to stage for it.
    fn new_exe(bytes: &[u8]) -> (tempfile::TempDir, PathBuf, String, PathBuf) {
        let stage = tempfile::tempdir().unwrap();
        let exe = stage.path().join("AeroFTP.exe");
        std::fs::write(&exe, bytes).unwrap();
        let readme = stage.path().join("README.txt");
        std::fs::write(
            &readme,
            format!("readme of {}", String::from_utf8_lossy(bytes)),
        )
        .unwrap();
        let key = exe_key(&exe).unwrap();
        (stage, exe, key, readme)
    }

    /// A fresh process: it staged nothing in `exe_dir`.
    fn forget_staged(exe_dir: &Path) {
        STAGED_HERE.lock().unwrap().retain(|d| d != exe_dir);
    }

    #[test]
    fn staging_writes_the_next_batch_whole_and_never_touches_the_current_one() {
        // The running exe (A) still has its README pending: an editor held it.
        let dir = folder(Some(KEY));
        let (_stage, exe_b, key_b, readme_b) = new_exe(b"B");
        let current_before = read(&dir.path().join(PENDING_DIR), "README.txt");
        // A source that cannot be read: nothing is staged.
        let missing = dir.path().join("no-such-license");
        assert!(stage_pending_docs(
            dir.path(),
            &exe_b,
            &[(&readme_b, "README.txt"), (&missing, "LICENSE.txt")],
            "a"
        )
        .is_err());
        assert!(!dir.path().join(NEXT_DIR).exists());
        assert!(!dir.path().join(format!("{TMP_PREFIX}a")).exists());
        // A whole stage is the next batch, with B's key.
        stage_pending_docs(dir.path(), &exe_b, &[(&readme_b, "README.txt")], "b").unwrap();
        let next = dir.path().join(NEXT_DIR);
        assert_eq!(read(&next, "README.txt"), "readme of B");
        assert_eq!(read(&next, KEY_FILE), key_b);
        // The current batch is A's, as it was.
        assert_eq!(
            read(&dir.path().join(PENDING_DIR), "README.txt"),
            current_before
        );
        assert_eq!(read(&dir.path().join(PENDING_DIR), KEY_FILE).trim(), KEY);
    }

    #[test]
    fn a_publication_that_fails_after_staging_keeps_the_current_batch_installable() {
        let dir = folder(Some(KEY));
        let (_stage, exe_b, _key_b, readme_b) = new_exe(b"B");
        let failed = stage_with(
            dir.path(),
            &exe_b,
            &[(&readme_b, "README.txt")],
            "p",
            |_, _| Err(io::Error::other("publication refused")),
        );
        assert!(failed.unwrap_err().contains("publication refused"));
        assert!(!dir.path().join(NEXT_DIR).exists());
        assert!(!dir.path().join(format!("{TMP_PREFIX}p")).exists());
        // A's batch still installs.
        assert_eq!(
            install_pending_docs(dir.path(), KEY, rename),
            Pending::Docs(DOCS.iter().map(|n| (*n, Outcome::Installed)).collect())
        );
    }

    #[test]
    fn a_discarded_update_drops_its_next_batch_and_keeps_the_current_one() {
        let dir = folder(Some(KEY));
        let (_stage, exe_b, _key_b, readme_b) = new_exe(b"B");
        stage_pending_docs(dir.path(), &exe_b, &[(&readme_b, "README.txt")], "d").unwrap();
        discard_pending_docs(dir.path());
        assert!(!dir.path().join(NEXT_DIR).exists());
        assert_eq!(
            install_pending_docs(dir.path(), KEY, rename),
            Pending::Docs(DOCS.iter().map(|n| (*n, Outcome::Installed)).collect())
        );
        assert_eq!(read(dir.path(), "README.txt"), "new README.txt");
    }

    #[test]
    fn the_running_app_keeps_the_batch_it_staged_while_it_retries_its_own() {
        // A retries its held README after staging B: B's batch waits for the
        // swap, A's documents install from A's batch.
        let dir = folder(Some(KEY));
        let (_stage, exe_b, key_b, readme_b) = new_exe(b"B");
        stage_pending_docs(dir.path(), &exe_b, &[(&readme_b, "README.txt")], "r").unwrap();
        let Pending::Docs(outcomes) = install_pending_docs(dir.path(), KEY, rename) else {
            panic!("docs")
        };
        assert!(outcomes.iter().all(|(_, o)| *o == Outcome::Installed));
        assert_eq!(read(dir.path(), "README.txt"), "new README.txt");
        assert_eq!(read(&dir.path().join(NEXT_DIR), KEY_FILE), key_b);
        assert_eq!(
            read(&dir.path().join(NEXT_DIR), "README.txt"),
            "readme of B"
        );
    }

    #[test]
    fn the_new_exe_promotes_its_batch_and_drops_the_older_exe_s() {
        let dir = folder(Some(KEY));
        let (_stage, exe_b, key_b, readme_b) = new_exe(b"B");
        stage_pending_docs(dir.path(), &exe_b, &[(&readme_b, "README.txt")], "s").unwrap();
        forget_staged(dir.path());
        assert_eq!(
            install_pending_docs(dir.path(), &key_b, rename),
            Pending::Docs(vec![("README.txt", Outcome::Installed)])
        );
        assert_eq!(read(dir.path(), "README.txt"), "readme of B");
        assert!(!dir.path().join(NEXT_DIR).exists());
        assert!(
            !dir.path().join(PENDING_DIR).exists(),
            "A's batch is superseded"
        );
    }

    #[test]
    fn after_a_failed_swap_the_old_exe_drops_the_update_s_batch_and_installs_its_own() {
        let dir = folder(Some(KEY));
        let (_stage, exe_b, _key_b, readme_b) = new_exe(b"B");
        stage_pending_docs(dir.path(), &exe_b, &[(&readme_b, "README.txt")], "f").unwrap();
        // A starts again in a new process: the swap never happened.
        forget_staged(dir.path());
        assert_eq!(
            install_pending_docs(dir.path(), KEY, rename),
            Pending::Docs(DOCS.iter().map(|n| (*n, Outcome::Installed)).collect())
        );
        assert!(!dir.path().join(NEXT_DIR).exists());
        assert_eq!(read(dir.path(), "README.txt"), "new README.txt");
    }

    #[test]
    fn a_pass_never_sees_a_staging_half_done_and_never_sweeps_a_live_one() {
        // A staging holds the lock while it writes its temporary folder and
        // publishes it; a pass started meanwhile waits, then finds the batch
        // whole, and the temporary folder was never removed under it.
        let dir = tempfile::tempdir().unwrap();
        let exe_dir = dir.path().to_path_buf();
        let (locked_tx, locked_rx) = std::sync::mpsc::channel();
        let staging = {
            let exe_dir = exe_dir.clone();
            std::thread::spawn(move || {
                let _lock = lock_batches(&exe_dir).unwrap();
                let tmp = exe_dir.join(format!("{TMP_PREFIX}live"));
                std::fs::create_dir(&tmp).unwrap();
                locked_tx.send(()).unwrap();
                std::thread::sleep(Duration::from_millis(300));
                std::fs::write(tmp.join("README.txt"), b"whole").unwrap();
                std::fs::write(tmp.join(KEY_FILE), KEY).unwrap();
                std::fs::rename(&tmp, exe_dir.join(NEXT_DIR)).unwrap();
            })
        };
        locked_rx.recv().unwrap();
        let outcome = install_pending_docs(&exe_dir, KEY, rename);
        staging.join().unwrap();
        assert_eq!(
            outcome,
            Pending::Docs(vec![("README.txt", Outcome::Installed)])
        );
        assert_eq!(read(&exe_dir, "README.txt"), "whole");
    }

    #[test]
    fn a_staging_folder_a_stopped_update_left_is_removed_and_never_read() {
        let dir = folder(Some(KEY));
        let tmp = dir.path().join(format!("{TMP_PREFIX}c"));
        std::fs::create_dir(&tmp).unwrap();
        std::fs::write(tmp.join("README.txt"), b"partial").unwrap();
        std::fs::write(tmp.join(KEY_FILE), KEY).unwrap();
        let Pending::Docs(_) = install_pending_docs(dir.path(), KEY, rename) else {
            panic!("docs")
        };
        assert_eq!(read(dir.path(), "README.txt"), "new README.txt");
        assert!(!tmp.exists());
    }

    #[test]
    fn the_key_of_an_exe_is_the_sha256_of_its_bytes() {
        let dir = tempfile::tempdir().expect("temp dir");
        let exe = dir.path().join("AeroFTP.exe");
        std::fs::write(&exe, b"abc").expect("exe");
        assert_eq!(
            exe_key(&exe).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
