// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! The documents a portable update installs beside the exes: the marker,
//! README, LICENSE and third-party notices.
//!
//! A batch of documents is a folder beside the exes holding the documents and
//! the SHA-256 of the app exe they belong to. Three places hold one each:
//!
//! - the next batch (`.aeroftp-pending-docs.next`): staged by the running app
//!   for the update it is about to hand to the helper, before the exes swap;
//! - the current batch (`.aeroftp-pending-docs`): the running exe's documents
//!   not installed yet (one an editor held);
//! - the set-aside batch (`.aeroftp-pending-docs.prev`): a next batch of
//!   another exe that a staging moved out of the way. Its exe still
//!   installs from it until the update replaces that exe; a failed update
//!   puts it back.
//!
//! For the About dialog, `own_pending_notices` checks the batches for the
//! running exe's key in order: next (`.aeroftp-pending-docs.next`), current
//! (`.aeroftp-pending-docs`), then set-aside (`.aeroftp-pending-docs.prev`).
//!
//! Staging writes the next batch only, under a temporary name renamed into
//! place, so it is whole or absent, and never touches the current batch. At
//! its start an app promotes a next batch with its own key to the current
//! batch, then renames each document over the installed one, an operation on
//! that exact path that Windows refuses while another process holds the file
//! and that never writes into a directory with the document's name. A
//! document still held stays in the current batch: the held ones share one
//! retry budget, and what is left is installed at a later start. A current
//! batch of another exe is removed (a folder replaced by hand, an older
//! exe's). A next batch of another exe is never removed at start, by any
//! process: an update another process hands over may be waiting for its
//! swap. A failed swap drops it (the helper's discard), a discarded update
//! drops it, and the next staging sets it aside. Staging, discarding and each
//! install pass hold one
//! lock per exe folder, so none of them sees another half done. Every
//! outcome is logged as what it is.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

/// The running exe's batch, beside the exes.
pub(crate) const PENDING_DIR: &str = ".aeroftp-pending-docs";
/// The batch staged for an update waiting for the exes to swap.
pub(crate) const NEXT_DIR: &str = ".aeroftp-pending-docs.next";
/// A next batch of another exe, set aside by a staging until the update it
/// staged for replaces that exe: a failed update puts it back, and its exe
/// still installs from it meanwhile.
pub(crate) const PREV_DIR: &str = ".aeroftp-pending-docs.prev";
/// The key of the exe a batch belongs to.
pub(crate) const KEY_FILE: &str = "exe.key";
/// The identity of a staging folder: a random value a staging writes in the
/// folder it creates and keeps in its record, so the record names that very
/// folder and never a folder created later at the same path.
pub(crate) const STAGING_ID_FILE: &str = "staging.id";
/// The durable ledger of AeroFTP-owned batch identities kept beside the
/// batches inside `exe_dir` so ownership survives temp cleanup and moving a
/// portable folder; removed automatically once empty.
const OWNED_RECORD_FILE: &str = ".aeroftp-pending-docs.owned";
/// The key of a batch superseded while one of its documents could not be
/// removed (an editor holds it): no exe has it (an exe key is hex), so the
/// batch is never installed or offered, and any later pass removes it.
const RETIRED_KEY: &str = "retired";
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
#[cfg(windows)]
const ERROR_FILE_NOT_FOUND: i32 = 2;
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
    /// Only a batch of another exe, superseded by this exe's: removed.
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

/// A name for the lock of one exe folder, the same in every process.
fn folder_id(exe_dir: &Path) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(
        exe_dir.to_string_lossy().to_lowercase().as_bytes(),
    ))
}

/// The lock of the batches of one exe folder, held until dropped. On
/// Windows a named mutex, which leaves no file in the portable folder.
pub(crate) struct BatchLock {
    #[cfg(windows)]
    handle: windows::Win32::Foundation::HANDLE,
    #[cfg(not(windows))]
    _file: std::fs::File,
}

/// The lock of the batches of `exe_dir`, waited for as long as it takes.
pub(crate) fn lock_batches(exe_dir: &Path) -> io::Result<BatchLock> {
    lock_batches_within(exe_dir, None)
}

/// The lock, waited for at most `wait` (none: as long as it takes); past it,
/// an error of kind `TimedOut`, and the batches are left as they are.
#[cfg(windows)]
fn lock_batches_within(exe_dir: &Path, wait: Option<Duration>) -> io::Result<BatchLock> {
    use windows::core::HSTRING;
    use windows::Win32::Foundation::{
        CloseHandle, BOOL, WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT,
    };
    use windows::Win32::System::Threading::{
        CreateMutexW, OpenMutexW, WaitForSingleObject, INFINITE, MUTEX_MODIFY_STATE,
        SYNCHRONIZATION_SYNCHRONIZE,
    };
    // Global: one lock for every Windows session that reaches the folder
    // (a Local name would give each session a lock of its own).
    let name = HSTRING::from(format!(
        "Global\\AeroFTP-pending-docs-{}",
        folder_id(exe_dir)
    ));
    let os_error = |e: windows::core::Error| {
        // A Win32 error as its HRESULT (0x8007xxxx): the code itself, so
        // ERROR_ACCESS_DENIED is recognized.
        let code = e.code().0 as u32;
        if code & 0xFFFF_0000 == 0x8007_0000 {
            io::Error::from_raw_os_error((code & 0xFFFF) as i32)
        } else {
            io::Error::other(e)
        }
    };
    let deadline = wait.map(|d| std::time::Instant::now() + d);
    // SAFETY: a named mutex with default security, or the one another
    // process of this user created; the handle is owned by the returned
    // lock, which releases and closes it.
    let handle = open_named_lock(
        deadline,
        || unsafe {
            // A mutex that exists with less than full access for this
            // process: opened for what the lock needs (wait and release).
            CreateMutexW(None, BOOL::from(false), &name)
                .map_err(os_error)
                .or_else(|e| match e.raw_os_error() {
                    Some(ERROR_ACCESS_DENIED) => OpenMutexW(
                        SYNCHRONIZATION_SYNCHRONIZE | MUTEX_MODIFY_STATE,
                        BOOL::from(false),
                        &name,
                    )
                    .map_err(os_error)
                    .or_else(|oe| match oe.raw_os_error() {
                        // Closed by the other session between CreateMutexW and OpenMutexW.
                        Some(ERROR_FILE_NOT_FOUND) => {
                            CreateMutexW(None, BOOL::from(false), &name).map_err(os_error)
                        }
                        _ => Err(oe),
                    }),
                    _ => Err(e),
                })
        },
        std::time::Instant::now,
        std::thread::sleep,
    )?;
    let millis = deadline.map_or(INFINITE, |d| {
        let left = d.saturating_duration_since(std::time::Instant::now());
        u32::try_from(left.as_millis()).map_or(INFINITE - 1, |ms| ms.min(INFINITE - 1))
    });
    // SAFETY: `handle` is the mutex just opened. An abandoned mutex (a
    // process that died holding it) is owned by this thread as well.
    let waited = unsafe { WaitForSingleObject(handle, millis) };
    if waited == WAIT_OBJECT_0 || waited == WAIT_ABANDONED {
        Ok(BatchLock { handle })
    } else {
        let error = if waited == WAIT_TIMEOUT {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "another AeroFTP process holds the lock of the portable documents",
            )
        } else {
            io::Error::last_os_error()
        };
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
fn lock_batches_within(exe_dir: &Path, wait: Option<Duration>) -> io::Result<BatchLock> {
    let path =
        std::env::temp_dir().join(format!("aeroftp-pending-docs-{}.lock", folder_id(exe_dir)));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)?;
    let Some(wait) = wait else {
        file.lock()?;
        return Ok(BatchLock { _file: file });
    };
    let deadline = std::time::Instant::now() + wait;
    wait_for_lock(
        Some(deadline),
        || match file.try_lock() {
            Ok(()) => Ok(true),
            Err(std::fs::TryLockError::WouldBlock) => Ok(false),
            Err(std::fs::TryLockError::Error(e)) => Err(e),
        },
        std::time::Instant::now,
        std::thread::sleep,
    )?;
    Ok(BatchLock { _file: file })
}

/// Opens the named mutex of the lock with `open` (Windows). An
/// `ERROR_ACCESS_DENIED` means the mutex exists and another user's session
/// created it, with security this one cannot open: that session holds the
/// lock or waits for it, so the lock is held, never free and never a
/// failure. It is probed again like a held lock until `deadline` (none: as
/// long as it takes), then `TimedOut`. Any other error is returned.
#[cfg(any(windows, test))]
fn open_named_lock<H>(
    deadline: Option<std::time::Instant>,
    mut open: impl FnMut() -> io::Result<H>,
    now: impl Fn() -> std::time::Instant,
    sleep: impl Fn(Duration),
) -> io::Result<H> {
    let mut opened = None;
    wait_for_lock(
        deadline,
        || match open() {
            Ok(handle) => {
                opened = Some(handle);
                Ok(true)
            }
            Err(e) if e.raw_os_error() == Some(ERROR_ACCESS_DENIED) => Ok(false),
            Err(e) => Err(e),
        },
        now,
        sleep,
    )?;
    opened.ok_or_else(|| io::Error::other("the lock of the portable documents was not opened"))
}

/// Probes `try_lock` until it takes the lock or `deadline` passes (none: as
/// long as it takes): a first probe at once, then never one past the
/// deadline (a pause is cut to the time left, and the deadline is checked
/// again before the next probe), so a lock released after the deadline is
/// not taken. `now` and `sleep` are the clock, given by the tests.
fn wait_for_lock(
    deadline: Option<std::time::Instant>,
    mut try_lock: impl FnMut() -> io::Result<bool>,
    now: impl Fn() -> std::time::Instant,
    sleep: impl Fn(Duration),
) -> io::Result<()> {
    loop {
        if try_lock()? {
            return Ok(());
        }
        let Some(deadline) = deadline else {
            sleep(Duration::from_millis(5));
            continue;
        };
        let left = deadline.saturating_duration_since(now());
        if !left.is_zero() {
            sleep(left.min(Duration::from_millis(5)));
        }
        if now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "another AeroFTP process holds the lock of the portable documents",
            ));
        }
    }
}

/// What a batch folder holds.
enum Slot {
    Missing,
    /// A file, a link, or a folder without a key: no batch, and nobody's
    /// to remove (it may be the user's): left as it is.
    Invalid,
    /// A folder whose key exists but cannot be read now (another process
    /// holds it): a batch, left whole for a later pass.
    Unreadable,
    Key(String),
}

fn slot(dir: &Path) -> Slot {
    match std::fs::symlink_metadata(dir) {
        Err(_) => Slot::Missing,
        Ok(meta) if !meta.is_dir() => Slot::Invalid,
        Ok(_) => match std::fs::read_to_string(dir.join(KEY_FILE)) {
            Ok(key) => Slot::Key(key.trim().to_string()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Slot::Invalid,
            Err(_) => Slot::Unreadable,
        },
    }
}

/// Whether `path` is a batch a staging wrote: a folder (not a link) that
/// holds a key file. A staging writes its key first, so a folder that only
/// has a batch's name, or a file or a link there, is never one.
fn is_batch(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir())
        && std::fs::symlink_metadata(path.join(KEY_FILE)).is_ok_and(|m| m.is_file())
}

/// The record of the staging folders a staging created in `exe_dir`, one
/// name and its identity (STAGING_ID_FILE) a line, kept outside the folder (in the temporary folder, as the
/// lock of other systems is): a folder is a staging's only when a staging
/// created it, which nothing in the folder itself can prove. A record that
/// is lost (a cleaned temporary folder) leaves a stopped staging where it
/// is, never removes anything else.
fn staging_record(exe_dir: &Path) -> PathBuf {
    // The folder as written, not folded to lower case as the lock name is:
    // on a case-sensitive volume `App` and `app` are two folders, and the
    // stagings of one are never the other's (a lock they share only
    // serializes them).
    use sha2::{Digest, Sha256};
    let exact = hex::encode(Sha256::digest(exe_dir.to_string_lossy().as_bytes()));
    std::env::temp_dir().join(format!("aeroftp-pending-docs-{exact}.staging"))
}

fn recorded_stagings(exe_dir: &Path) -> Vec<(String, String)> {
    std::fs::read_to_string(staging_record(exe_dir))
        .map(|text| {
            text.lines()
                .filter_map(|line| line.split_once(' '))
                .filter(|(name, id)| {
                    name.starts_with(TMP_PREFIX) && !name.contains(['/', '\\']) && !id.is_empty()
                })
                .map(|(name, id)| (name.to_string(), id.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

fn write_stagings(exe_dir: &Path, names: &[(String, String)]) -> io::Result<()> {
    let record = staging_record(exe_dir);
    if names.is_empty() {
        return match std::fs::remove_file(&record) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    std::fs::write(
        record,
        names
            .iter()
            .map(|(name, id)| format!("{name} {id}\n"))
            .collect::<String>(),
    )
}

/// Records `name`, a folder this staging just created, with a new identity,
/// then writes that identity in the folder and returns it. Under the lock. A
/// staging stopped between the two leaves an empty folder, which the sweep
/// removes (only an empty folder: `remove_dir`).
fn begin_staging(exe_dir: &Path, name: &str) -> io::Result<String> {
    let id = format!("{:032x}", rand::random::<u128>());
    let mut names = recorded_stagings(exe_dir);
    names.retain(|(n, _)| n != name);
    names.push((name.to_string(), id.clone()));
    write_stagings(exe_dir, &names)?;
    std::fs::write(exe_dir.join(name).join(STAGING_ID_FILE), &id)?;
    Ok(id)
}

/// The durable ownership record of batch identities (`STAGING_ID_FILE`) that
/// AeroFTP staged for `exe_dir`, stored beside the batches inside `exe_dir`
/// so it survives temp-folder cleanup and moving a portable installation;
/// removed automatically once empty.
fn owned_batch_record(exe_dir: &Path) -> PathBuf {
    exe_dir.join(OWNED_RECORD_FILE)
}

/// Secondary mirror of batch identities in `temp_dir()`, so even an
/// in-place deletion of `.aeroftp-pending-docs.owned` does not lose
/// ownership of existing AeroFTP batches.
fn temp_batch_record(exe_dir: &Path) -> PathBuf {
    use sha2::{Digest, Sha256};
    let exact = hex::encode(Sha256::digest(exe_dir.to_string_lossy().as_bytes()));
    std::env::temp_dir().join(format!("aeroftp-pending-docs-{exact}.batches"))
}

fn read_ids_from(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .map(|text| {
            text.lines()
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn recorded_batches(exe_dir: &Path) -> Vec<String> {
    let mut ids = read_ids_from(&owned_batch_record(exe_dir));
    for id in read_ids_from(&temp_batch_record(exe_dir)) {
        if !ids.iter().any(|existing| existing == &id) {
            ids.push(id);
        }
    }
    ids
}

fn replace_record_file(tmp: &Path, target: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        use windows::core::HSTRING;
        use windows::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_REPLACE_EXISTING};
        let from = HSTRING::from(tmp.as_os_str());
        let to = HSTRING::from(target.as_os_str());
        // SAFETY: both paths are valid UTF-16 HSTRINGs in the same directory.
        unsafe { MoveFileExW(&from, &to, MOVEFILE_REPLACE_EXISTING) }.map_err(io::Error::other)
    }
    #[cfg(not(windows))]
    {
        std::fs::rename(tmp, target)
    }
}

/// Atomically replaces `record` with `ids` (or deletes `record` when empty):
/// writes to a sibling temporary file in the same directory via `write_tmp`
/// and replaces the live file only after the write succeeds, so a write
/// failure never truncates or loses previously recorded IDs.
fn write_batches_with(
    record: &Path,
    ids: &[String],
    write_tmp: impl FnOnce(&Path, &str) -> io::Result<()>,
) -> io::Result<()> {
    if ids.is_empty() {
        return match std::fs::remove_file(record) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    let content: String = ids.iter().map(|id| format!("{id}\n")).collect();
    let parent = record
        .parent()
        .ok_or_else(|| io::Error::other("ownership record has no parent directory"))?;
    let name = record
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(OWNED_RECORD_FILE);
    let tmp = parent.join(format!("{name}.tmp-{:016x}", rand::random::<u64>()));
    if let Err(e) = write_tmp(&tmp, &content) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = replace_record_file(&tmp, record) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

fn write_batches_to(record: &Path, ids: &[String]) -> io::Result<()> {
    write_batches_with(record, ids, |path, content| std::fs::write(path, content))
}

fn record_batch(exe_dir: &Path, id: &str) -> io::Result<()> {
    let mut ids = recorded_batches(exe_dir);
    if !ids.iter().any(|existing| existing == id) {
        ids.push(id.to_string());
    }
    write_batches_to(&owned_batch_record(exe_dir), &ids)?;
    let _ = write_batches_to(&temp_batch_record(exe_dir), &ids);
    Ok(())
}

fn forget_batch(exe_dir: &Path, id: &str) -> io::Result<()> {
    let ids: Vec<String> = recorded_batches(exe_dir)
        .into_iter()
        .filter(|existing| existing != id)
        .collect();
    let primary = write_batches_to(&owned_batch_record(exe_dir), &ids);
    let _ = write_batches_to(&temp_batch_record(exe_dir), &ids);
    primary
}

fn batch_id(path: &Path) -> Option<String> {
    std::fs::symlink_metadata(path.join(STAGING_ID_FILE))
        .ok()
        .filter(|m| m.is_file())?;
    let id = std::fs::read_to_string(path.join(STAGING_ID_FILE)).ok()?;
    let id = id.trim();
    (!id.is_empty()).then(|| id.to_string())
}

/// Whether `path` is a batch folder that AeroFTP itself staged for `exe_dir`:
/// a real folder carrying `KEY_FILE` and a `STAGING_ID_FILE` recorded in the
/// durable ownership record (`owned_batch_record` or `temp_batch_record`).
fn is_owned_batch(exe_dir: &Path, path: &Path) -> bool {
    is_batch(path)
        && batch_id(path).is_some_and(|id| recorded_batches(exe_dir).iter().any(|r| r == &id))
}

/// Drops `name` from the record once its folder is gone. Under the lock.
fn forget_staging(exe_dir: &Path, name: &str) {
    let names: Vec<(String, String)> = recorded_stagings(exe_dir)
        .into_iter()
        .filter(|(n, _)| n != name)
        .collect();
    let _ = write_stagings(exe_dir, &names);
}

/// Removes the staging folders a stopped update left: only the ones the
/// record names, which a staging created, and only while the folder still
/// holds the identity the record keeps: a folder created later at that path
/// (the record outlived its folder: a staging stopped after its publishing
/// rename) is not that staging's, and stays whole. Called under the lock
/// only, so no staging is in progress. In a staging's folder only what a
/// staging writes is removed (remove_batch, by name), then the folder once
/// empty; a folder that merely has a staging's name, whatever it holds (a
/// key file and a README too), is not in the record and stays whole.
fn remove_staging_leftovers(exe_dir: &Path) {
    let names = recorded_stagings(exe_dir);
    if names.is_empty() {
        return;
    }
    let left: Vec<(String, String)> = names
        .into_iter()
        .filter(|(name, id)| {
            let path = exe_dir.join(name);
            let id_file = path.join(STAGING_ID_FILE);
            let ours = std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir())
                && std::fs::read_to_string(&id_file).is_ok_and(|found| found == *id);
            if ours {
                remove_batch(&path);
                if !is_batch(&path) {
                    // Stopped before its key: the identity is all it wrote.
                    let _ = std::fs::remove_file(&id_file);
                }
            }
            // Empty when the staging stopped before it wrote its identity;
            // an empty folder holds nothing to lose, whoever made it.
            let _ = std::fs::remove_dir(&path);
            ours && std::fs::symlink_metadata(&path).is_ok()
        })
        .collect();
    let _ = write_stagings(exe_dir, &left);
}

/// Removes the batch at `path`: the documents a staging writes, then, once
/// none is left, its key and the folder if that left it empty. Never
/// recursive, so anything else in the folder, and the folder, stay; a
/// document that cannot be removed keeps the key, so the folder is still
/// known as a batch to a later pass. Anything at `path` that is no batch
/// (is_batch) is left as it is. Whether a batch was there.
fn remove_batch(path: &Path) -> bool {
    if !is_batch(path) {
        return false;
    }
    for name in DOCS {
        let _ = std::fs::remove_file(path.join(name));
    }
    if DOCS
        .iter()
        .all(|name| std::fs::symlink_metadata(path.join(name)).is_err())
    {
        let _ = std::fs::remove_file(path.join(STAGING_ID_FILE));
        let _ = std::fs::remove_file(path.join(KEY_FILE));
        let _ = std::fs::remove_dir(path);
    }
    true
}

/// Removes a superseded batch that AeroFTP owns (`is_owned_batch`); an
/// unowned folder at that path is left unchanged. What cannot be removed now
/// (a document an editor holds) is left in a batch marked retired, so a later
/// pass, of any exe, removes the rest. Without the mark the batch would keep
/// the older exe's key and no pass would ever supersede it again.
fn retire_batch(exe_dir: &Path, path: &Path) -> bool {
    if !is_owned_batch(exe_dir, path) {
        return false;
    }
    let id = batch_id(path);
    let removed = remove_batch(path);
    if removed && std::fs::symlink_metadata(path).is_ok() {
        let _ = std::fs::write(path.join(KEY_FILE), RETIRED_KEY);
    } else if let Some(id) = id {
        let _ = forget_batch(exe_dir, &id);
    }
    removed
}

/// Stages `docs` (source, document name) as the next batch of `exe_dir`, for
/// the exe at `new_exe`. The current batch is never touched; a next batch of
/// another exe (a staging whose swap never happened) is replaced. Anything
/// else at the next batch's name (a folder without a key, a file) is not
/// ours: staging fails and leaves it as it is, so the update stops before
/// its swap. Returns the key the batch carries (None with no documents), the
/// one key a failed update may later discard: read once, here, so a second
/// read cannot name another batch.
pub(crate) fn stage_pending_docs(
    exe_dir: &Path,
    new_exe: &Path,
    docs: &[(&Path, &'static str)],
    suffix: &str,
) -> Result<Option<String>, String> {
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
) -> Result<Option<String>, String> {
    if docs.is_empty() {
        return Ok(None);
    }
    let fail = |e: io::Error| format!("Failed to stage the portable documents: {e}");
    let key = exe_key(new_exe).map_err(fail)?;
    let _lock = lock_batches(exe_dir).map_err(fail)?;
    remove_staging_leftovers(exe_dir);
    let next = exe_dir.join(NEXT_DIR);
    let prev = exe_dir.join(PREV_DIR);
    if is_owned_batch(exe_dir, &next)
        && matches!(slot(&next), Slot::Key(k) if k == key)
        && docs.iter().all(|(_, name)| next.join(name).is_file())
    {
        // Already staged by AeroFTP for this exe, with every document asked
        // for. Transfer ownership under the lock to a fresh per-attempt token
        // so cleanup from an earlier failed attempt of the same exe cannot
        // delete the batch this retry relies on.
        let old_id = batch_id(&next);
        let new_id = format!("{:032x}", rand::random::<u128>());
        record_batch(exe_dir, &new_id).map_err(fail)?;
        let id_path = next.join(STAGING_ID_FILE);
        let id_tmp = next.join(format!(
            "{STAGING_ID_FILE}.tmp-{:016x}",
            rand::random::<u64>()
        ));
        let transfer =
            std::fs::write(&id_tmp, &new_id).and_then(|()| replace_record_file(&id_tmp, &id_path));
        if let Err(e) = transfer {
            let _ = std::fs::remove_file(&id_tmp);
            let _ = forget_batch(exe_dir, &new_id);
            return Err(fail(e));
        }
        if let Some(old_id) = old_id {
            let _ = forget_batch(exe_dir, &old_id);
        }
        return Ok(Some(new_id));
    }
    if std::fs::symlink_metadata(&next).is_ok() && !is_owned_batch(exe_dir, &next) {
        return Err(format!(
            "Failed to stage the portable documents: {} is not an update batch owned by AeroFTP, so it is left as it is; move it away and update again",
            next.display()
        ));
    }
    let tmp_name = format!("{TMP_PREFIX}{suffix}");
    let tmp = exe_dir.join(&tmp_name);
    // A folder this call creates, and only then cleaned up on a failure: a
    // folder already at that name is not ours, and stays as it is. Once
    // created it is recorded, so a staging stopped at any point is found
    // again by the sweep of leftovers, and only then.
    std::fs::create_dir(&tmp).map_err(fail)?;
    let staging_id = match begin_staging(exe_dir, &tmp_name) {
        Ok(id) => id,
        Err(e) => {
            let _ = std::fs::remove_file(tmp.join(STAGING_ID_FILE));
            if std::fs::remove_dir(&tmp).is_ok() {
                forget_staging(exe_dir, &tmp_name);
            }
            return Err(fail(e));
        }
    };
    let staged = (|| -> io::Result<()> {
        std::fs::write(tmp.join(KEY_FILE), &key)?;
        for (source, name) in docs {
            std::fs::copy(source, tmp.join(name))?;
        }
        let aside = make_room(exe_dir, &next, &prev, &key)?;
        record_batch(exe_dir, &staging_id)?;
        let published = publish(&tmp, &next);
        if published.is_err() {
            let _ = forget_batch(exe_dir, &staging_id);
            if aside && std::fs::symlink_metadata(&next).is_err() {
                // Nothing replaced the batch set aside: it is back in its place.
                let _ = std::fs::rename(&prev, &next);
            }
        }
        published
    })();
    if staged.is_err() {
        remove_batch(&tmp);
        // Only the identity is left when the key itself could not be written.
        if !is_batch(&tmp) {
            let _ = std::fs::remove_file(tmp.join(STAGING_ID_FILE));
        }
        let _ = std::fs::remove_dir(&tmp);
    }
    if std::fs::symlink_metadata(&tmp).is_err() {
        forget_staging(exe_dir, &tmp_name);
    }
    staged.map(|()| Some(staging_id)).map_err(fail)
}

/// Makes room at `next` for the batch of the exe `key`. A batch of another
/// exe there (one whose documents an editor held, of the exe that runs) is
/// set aside at `prev`, cleaning up only an already-retired AeroFTP batch
/// there: a live (non-retired) batch in `prev` may still belong to the
/// running exe (an earlier update C stopped before swapping) and is never
/// removed or re-keyed here. If `prev` is still occupied or the rename
/// cannot complete, returns an error without touching `next` or `prev`. A
/// batch of this exe (part of one, a failed attempt left) or a retired one
/// is removed. Returns whether a batch was set aside.
fn make_room(exe_dir: &Path, next: &Path, prev: &Path, key: &str) -> io::Result<bool> {
    if matches!(slot(next), Slot::Key(k) if k != key && k != RETIRED_KEY) {
        if matches!(slot(prev), Slot::Key(k) if k == RETIRED_KEY) {
            retire_batch(exe_dir, prev);
        }
        if std::fs::symlink_metadata(prev).is_err() {
            std::fs::rename(next, prev)?;
            return Ok(true);
        }
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "cannot set aside {} to {}: {} is occupied",
                next.display(),
                prev.display(),
                prev.display()
            ),
        ));
    }
    let id = batch_id(next);
    remove_batch(next);
    if std::fs::symlink_metadata(next).is_err() {
        if let Some(id) = id {
            forget_batch(exe_dir, &id)?;
        }
    }
    Ok(false)
}

/// Removes the next batch of `exe_dir` only when it is an AeroFTP-owned
/// batch whose per-attempt token (`STAGING_ID_FILE`) matches `token`, plus
/// any staging leftover: the update attempt did not go through. An unowned
/// folder (even with a matching key), a batch whose ownership was
/// transferred to a same-exe retry, or another exe's batch stays whole.
pub(crate) fn discard_pending_docs(exe_dir: &Path, token: &str) {
    let Ok(_lock) = lock_batches(exe_dir) else {
        return;
    };
    remove_staging_leftovers(exe_dir);
    let next = exe_dir.join(NEXT_DIR);
    if is_owned_batch(exe_dir, &next) && batch_id(&next).as_deref() == Some(token) {
        remove_batch(&next);
        if std::fs::symlink_metadata(&next).is_err() {
            let _ = forget_batch(exe_dir, token);
        }
    }
    // The AeroFTP-owned batch the staging set aside goes back to its place:
    // nothing replaced its exe (here, or the helper's failed swap before).
    let prev = exe_dir.join(PREV_DIR);
    if std::fs::symlink_metadata(&next).is_err()
        && is_owned_batch(exe_dir, &prev)
        && matches!(slot(&prev), Slot::Key(k) if k != RETIRED_KEY)
    {
        let _ = std::fs::rename(&prev, &next);
    }
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
    wait: Option<Duration>,
    replace: impl Fn(&Path, &Path) -> io::Result<()>,
) -> (Pending, Option<PathBuf>) {
    let _lock = match lock_batches_within(exe_dir, wait) {
        Ok(lock) => lock,
        Err(e) => return (Pending::Unavailable(e.to_string()), None),
    };
    remove_staging_leftovers(exe_dir);
    let current = exe_dir.join(PENDING_DIR);
    let next = exe_dir.join(NEXT_DIR);
    let prev = exe_dir.join(PREV_DIR);
    let mut stale = false;
    for retired in [&current, &prev] {
        if matches!(slot(retired), Slot::Key(k) if k == RETIRED_KEY)
            && retire_batch(exe_dir, retired)
        {
            // Superseded earlier, with a document then held: the rest goes
            // now, and it counts as removed only once it is gone.
            stale |= std::fs::symlink_metadata(retired).is_err();
        }
    }
    let mut source = current.clone();
    match slot(&next) {
        Slot::Missing => {}
        Slot::Key(k) if k == own_key => {
            // This exe's documents supersede an older exe's, and the batch
            // its update set aside.
            stale |= retire_batch(exe_dir, &current);
            stale |= retire_batch(exe_dir, &prev);
            if std::fs::rename(&next, &current).is_err() {
                source = next.clone();
            }
        }
        // A key that cannot be read now may be this exe's: a later pass.
        Slot::Unreadable => {
            let why = format!("the key of {} cannot be read now", next.display());
            return (Pending::Unavailable(why), None);
        }
        // Another exe's: an update, of this process or another, may be
        // waiting for its swap. It is never read here; a failed swap, a
        // discard or the next staging removes it. No batch: not ours.
        Slot::Key(_) | Slot::Invalid => {}
    }
    // This exe's batch, set aside by the staging of an update that has not
    // replaced this exe: its documents install from there.
    if !matches!(slot(&source), Slot::Key(k) if k == own_key) {
        match slot(&prev) {
            Slot::Key(k) if k == own_key => source = prev.clone(),
            Slot::Unreadable => {
                let why = format!("the key of {} cannot be read now", prev.display());
                return (Pending::Unavailable(why), None);
            }
            _ => {}
        }
    }
    match slot(&source) {
        Slot::Missing => {
            let found = if stale { Pending::Stale } else { Pending::None };
            return (found, None);
        }
        Slot::Key(k) if k == own_key => {}
        // Retired and still held: removed by a later pass.
        Slot::Key(k) if k == RETIRED_KEY => {
            let found = if stale { Pending::Stale } else { Pending::None };
            return (found, None);
        }
        // Another exe's batch: this exe may be an older companion the
        // update could not replace, and the batch the new exe's, waiting for
        // documents to be released. Only the exe whose batch supersedes it
        // removes it (above). A key that cannot be read now: a later pass.
        Slot::Key(_) => return (Pending::None, None),
        Slot::Unreadable => {
            let why = format!("the key of {} cannot be read now", source.display());
            return (Pending::Unavailable(why), None);
        }
        // No batch (no key, a file, a link): not ours, left as it is.
        Slot::Invalid => {
            let found = if stale { Pending::Stale } else { Pending::None };
            return (found, None);
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
        let id = batch_id(&source);
        remove_batch(&source);
        if std::fs::symlink_metadata(&source).is_err() {
            if let Some(id) = id {
                let _ = forget_batch(exe_dir, &id);
            }
        }
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
    install_pass(exe_dir, own_key, None, replace).0
}

/// `install_pending_docs` waiting at most `wait` for the lock: the pass a
/// start runs before its window, which another process holding the lock
/// (a stalled staging) must not delay. Past it, `Pending::Unavailable`, the
/// batches untouched; the start runs a full pass in the background then.
pub(crate) fn install_pending_docs_within(
    exe_dir: &Path,
    own_key: &str,
    wait: Duration,
    replace: impl Fn(&Path, &Path) -> io::Result<()>,
) -> Pending {
    install_pass(exe_dir, own_key, Some(wait), replace).0
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
    let mut left = rounds;
    let mut first = first;
    // A first pass that could not look at the batches (a key that cannot be
    // read now, the lock) is tried again within the same budget.
    while left > 0 && matches!(first, Pending::Unavailable(_)) {
        left -= 1;
        sleep(pause);
        first = install_pass(exe_dir, own_key, None, &replace).0;
    }
    let Pending::Docs(mut last) = first else {
        return first;
    };
    while left > 0 && last.iter().any(|(_, outcome)| outcome.waits()) {
        left -= 1;
        sleep(pause);
        let (retried, source) = install_pass(exe_dir, own_key, None, &replace);
        // A pass that could not look at the batch (the lock, a key that
        // cannot be read now) says nothing about the held documents: keep
        // the held result and use the remaining retry budget.
        if matches!(retried, Pending::Unavailable(_)) {
            continue;
        }
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

/// How long the About lookup waits for the lock of the batches.
const NOTICES_LOCK_WAIT: Duration = Duration::from_millis(500);

/// The pending notices of the exe whose key `own_key` gives, which the
/// About dialog opens first: those of the next batch when its key is that
/// exe's (a pass installs from it when the current batch of an older exe
/// could not be removed), else those of the current batch when its key is.
/// A batch of another exe is never exposed, and `own_key` is asked only when
/// a batch holds notices. The path returned is a copy made under the lock
/// (`snapshot_notices`), named after the batch's key and the digest of the
/// notices: a staging that later replaces the batch at the same folder
/// cannot change what About opens, and a copy an editor holds open is
/// reused, never rewritten.
pub(crate) fn own_pending_notices(
    exe_dir: &Path,
    own_key: impl FnOnce() -> Option<String>,
) -> Option<PathBuf> {
    // A stalled process that holds the lock does not keep About waiting:
    // past the wait, no pending notices, and About opens the installed ones.
    let _lock = lock_batches_within(exe_dir, Some(NOTICES_LOCK_WAIT)).ok()?;
    let held: Vec<(PathBuf, String)> = [NEXT_DIR, PENDING_DIR, PREV_DIR]
        .iter()
        .filter_map(|name| {
            let dir = exe_dir.join(name);
            let notices = dir.join("THIRD-PARTY-NOTICES.md");
            match slot(&dir) {
                Slot::Key(k) if notices.is_file() => Some((notices, k)),
                _ => None,
            }
        })
        .collect();
    if held.is_empty() {
        return None;
    }
    let own = own_key()?;
    let (notices, key) = held.into_iter().find(|(_, k)| *k == own)?;
    snapshot_notices(exe_dir, &key, &notices)
}

/// An immutable copy of the notices at `notices` in the temporary folder,
/// named after the folder, the exe key and the digest of the bytes. A copy
/// with those bytes is reused without writing to it, so an editor that holds
/// it open (and denies write sharing) never makes the lookup fail; notices
/// that changed get a copy of their own, written whole and then renamed.
fn snapshot_notices(exe_dir: &Path, key: &str, notices: &Path) -> Option<PathBuf> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(notices).ok()?;
    let digest = hex::encode(Sha256::digest(&bytes));
    let name = format!(
        "aeroftp-pending-notices-{}-{key}-{}.md",
        folder_id(exe_dir),
        &digest[..16]
    );
    let copy = std::env::temp_dir().join(&name);
    if std::fs::read(&copy).is_ok_and(|held| held == bytes) {
        return Some(copy);
    }
    let part = std::env::temp_dir().join(format!("{name}.part-{}", std::process::id()));
    let written = std::fs::write(&part, &bytes).and_then(|()| std::fs::rename(&part, &copy));
    if written.is_err() {
        let _ = std::fs::remove_file(&part);
        return None;
    }
    Some(copy)
}

/// The key of the running exe, read once it can be: a read that fails (a
/// sharing violation while an antivirus scans the exe) is not cached, so a
/// later call reads the exe again.
pub(crate) fn running_exe_key() -> Option<String> {
    static KEY: OnceLock<String> = OnceLock::new();
    cached_key(&KEY, || {
        std::env::current_exe()
            .ok()
            .and_then(|exe| exe_key(&exe).ok())
    })
}

/// The key in `cell`, or the one `compute` gives, kept only when there is one.
fn cached_key(cell: &OnceLock<String>, compute: impl FnOnce() -> Option<String>) -> Option<String> {
    if let Some(key) = cell.get() {
        return Some(key.clone());
    }
    let key = compute()?;
    Some(cell.get_or_init(|| key).clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    const KEY: &str = "new-exe-key";

    /// The About dialog opens `THIRD-PARTY-NOTICES.md` at Tauri's resource
    /// root (thirdPartyLicenses.ts), which must be the running exe's own
    /// folder on Windows, where the portable ZIP puts the notices beside
    /// AeroFTP.exe: an offline portable app then opens the shipped file, not
    /// the online copy. Tauri's executableDir is unsupported on Windows, so
    /// this pins the one lookup the portable build relies on.
    #[cfg(windows)]
    #[test]
    fn the_resource_root_of_a_windows_app_is_the_running_exe_folder() {
        use tauri::Manager;
        let app = tauri::test::mock_app();
        let exe = std::env::current_exe().expect("current exe");
        // Tauri canonicalizes it, so it comes back as an extended-length
        // path (\\?\D:\...); thirdPartyLicenses.ts plainWindowsPath gives
        // the frontend the ordinary form, which is what this compares.
        let root = app.path().resource_dir().expect("resource dir");
        let root = root.to_string_lossy();
        let plain = match root.strip_prefix(r"\\?\UNC\") {
            Some(rest) => format!(r"\\{rest}"),
            None => root.strip_prefix(r"\\?\").unwrap_or(&root).to_string(),
        };
        assert_eq!(
            std::path::Path::new(&plain),
            exe.parent().expect("exe folder")
        );
    }

    #[test]
    fn a_failed_read_of_the_exe_key_is_not_cached_and_the_next_call_reads_it_again() {
        let cell = OnceLock::new();
        let reads = Cell::new(0);
        let read = |result: Option<&str>| {
            reads.set(reads.get() + 1);
            result.map(str::to_string)
        };
        assert_eq!(cached_key(&cell, || read(None)), None);
        assert_eq!(cached_key(&cell, || read(Some(KEY))).as_deref(), Some(KEY));
        // Kept once read: a later failure does not drop it, and nothing is read.
        assert_eq!(cached_key(&cell, || read(None)).as_deref(), Some(KEY));
        assert_eq!(reads.get(), 2);
    }

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
            let id = format!("{:032x}", rand::random::<u128>());
            std::fs::write(pending.join(STAGING_ID_FILE), &id).expect("staging id");
            record_batch(dir.path(), &id).expect("record batch");
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

    /// A snapshot of the pending notices is reused without a write, so one
    /// an editor holds (here read-only, which a write fails on) is offered
    /// again; changed notices get a snapshot of their own, the first stays.
    #[test]
    fn a_snapshot_of_the_pending_notices_is_reused_unwritten_and_changed_notices_get_their_own() {
        let dir = folder(Some(KEY));
        let own = || Some(KEY.to_string());
        let first = own_pending_notices(dir.path(), own).expect("own batch");
        let set_readonly = |path: &Path, readonly: bool| {
            let mut perms = std::fs::metadata(path).unwrap().permissions();
            perms.set_readonly(readonly);
            std::fs::set_permissions(path, perms).unwrap();
        };
        set_readonly(&first, true);
        assert_eq!(own_pending_notices(dir.path(), own).as_ref(), Some(&first));
        std::fs::write(
            dir.path().join(PENDING_DIR).join("THIRD-PARTY-NOTICES.md"),
            "newer notices",
        )
        .unwrap();
        let changed = own_pending_notices(dir.path(), own).expect("changed notices");
        assert_ne!(changed, first);
        assert_eq!(std::fs::read_to_string(&changed).unwrap(), "newer notices");
        assert_eq!(
            std::fs::read_to_string(&first).unwrap(),
            "new THIRD-PARTY-NOTICES.md"
        );
        set_readonly(&first, false);
        for path in [first, changed] {
            std::fs::remove_file(path).unwrap();
        }
    }

    /// The Windows case: an editor holds the first snapshot open and denies
    /// write sharing, and the next lookup still returns this exe's notices.
    #[cfg(windows)]
    #[test]
    fn a_snapshot_an_editor_holds_open_is_offered_again() {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 1;
        let dir = folder(Some(KEY));
        let own = || Some(KEY.to_string());
        let first = own_pending_notices(dir.path(), own).expect("own batch");
        let held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(&first)
            .unwrap();
        assert_eq!(own_pending_notices(dir.path(), own).as_ref(), Some(&first));
        drop(held);
        std::fs::remove_file(first).unwrap();
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
        let offered = own_pending_notices(dir.path(), || Some(KEY.to_string()))
            .expect("About opens the pending notices meanwhile");
        assert_eq!(
            std::fs::read_to_string(offered).unwrap(),
            "new THIRD-PARTY-NOTICES.md"
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
    fn a_folder_without_a_key_or_a_file_is_left_whole_another_exes_batch_is_kept() {
        let replace = |_: &Path, _: &Path| -> io::Result<()> {
            panic!("another exe's or a stale document is never installed")
        };
        // A folder without a key at the batch's name is not ours: nothing in
        // it is installed or removed.
        let dir = folder(None);
        assert_eq!(
            install_pending_docs(dir.path(), KEY, replace),
            Pending::None
        );
        assert_eq!(
            read(&dir.path().join(PENDING_DIR), "README.txt"),
            "new README.txt"
        );
        assert_eq!(read(dir.path(), "README.txt"), "old README.txt");
        // The new build's batch, promoted while its documents were held, and
        // an older companion exe started before the new build again: the
        // batch stays, and the new build installs it.
        let dir = folder(Some("new-build"));
        assert_eq!(
            install_pending_docs(dir.path(), KEY, replace),
            Pending::None
        );
        assert_eq!(
            read(&dir.path().join(PENDING_DIR), "README.txt"),
            "new README.txt"
        );
        install_pending_docs(dir.path(), "new-build", rename);
        assert_eq!(read(dir.path(), "README.txt"), "new README.txt");
        assert!(!dir.path().join(PENDING_DIR).exists());
        // A next folder without a key, or a file there, is no batch, and
        // stays as it is.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(NEXT_DIR)).unwrap();
        std::fs::write(dir.path().join(NEXT_DIR).join("README.txt"), "no key").unwrap();
        assert_eq!(install_pending_docs(dir.path(), KEY, rename), Pending::None);
        assert_eq!(read(&dir.path().join(NEXT_DIR), "README.txt"), "no key");
        assert!(!dir.path().join("README.txt").exists());
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(NEXT_DIR), b"not a folder").unwrap();
        assert_eq!(install_pending_docs(dir.path(), KEY, rename), Pending::None);
        assert_eq!(read(dir.path(), NEXT_DIR), "not a folder");
        // A next batch of another exe is kept for its update, and never read.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(NEXT_DIR)).unwrap();
        std::fs::write(dir.path().join(NEXT_DIR).join(KEY_FILE), "other-exe").unwrap();
        std::fs::write(dir.path().join(NEXT_DIR).join("README.txt"), "other").unwrap();
        assert_eq!(install_pending_docs(dir.path(), KEY, rename), Pending::None);
        assert_eq!(read(&dir.path().join(NEXT_DIR), "README.txt"), "other");
        assert!(!dir.path().join("README.txt").exists());
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(PENDING_DIR), b"not a folder").unwrap();
        assert_eq!(install_pending_docs(dir.path(), KEY, rename), Pending::None);
        assert_eq!(read(dir.path(), PENDING_DIR), "not a folder");
    }

    /// The new build's notices are held at its start, so its batch waits as
    /// the current one; an older companion exe started before the new build
    /// runs again never drops it; once the editor lets go, the new build's
    /// next start installs the notices.
    #[test]
    fn an_older_companion_keeps_the_held_notices_of_the_new_build_until_it_installs_them() {
        let dir = folder(Some(KEY));
        let (_stage, exe_new, key_new, readme_new) = new_exe(b"new build");
        let notices_new = readme_new.with_file_name("THIRD-PARTY-NOTICES.md");
        std::fs::write(&notices_new, "notices of the new build").unwrap();
        let license_new = readme_new.with_file_name("LICENSE.txt");
        std::fs::write(&license_new, "license of the new build").unwrap();
        stage_pending_docs(
            dir.path(),
            &exe_new,
            &[
                (&readme_new, "README.txt"),
                (&license_new, "LICENSE.txt"),
                (&notices_new, "THIRD-PARTY-NOTICES.md"),
            ],
            "n",
        )
        .unwrap();
        // The new build's start promotes its batch; an editor holds the notices.
        let editor = RefCell::new(vec!["THIRD-PARTY-NOTICES.md"]);
        let Pending::Docs(first) =
            install_pending_docs(dir.path(), &key_new, held(&editor, ERROR_SHARING_VIOLATION))
        else {
            panic!("the new build's batch")
        };
        assert!(first.contains(&("README.txt", Outcome::Installed)));
        assert!(first.contains(&("LICENSE.txt", Outcome::Installed)));
        assert!(first.contains(&("THIRD-PARTY-NOTICES.md", Outcome::InUse)));
        assert_eq!(read(dir.path(), "LICENSE.txt"), "license of the new build");
        let current = dir.path().join(PENDING_DIR);
        assert_eq!(read(&current, KEY_FILE), key_new);
        // The older companion starts: the batch is the new build's, kept whole.
        let never = |_: &Path, _: &Path| -> io::Result<()> {
            panic!("the older exe never installs the new build's documents")
        };
        assert_eq!(install_pending_docs(dir.path(), KEY, never), Pending::None);
        assert_eq!(
            read(&current, "THIRD-PARTY-NOTICES.md"),
            "notices of the new build"
        );
        assert_eq!(read(&current, KEY_FILE), key_new);
        // Released, the new build installs them at its next start.
        editor.borrow_mut().clear();
        assert_eq!(
            install_pending_docs(dir.path(), &key_new, held(&editor, ERROR_SHARING_VIOLATION)),
            Pending::Docs(vec![("THIRD-PARTY-NOTICES.md", Outcome::Installed)])
        );
        assert_eq!(
            read(dir.path(), "THIRD-PARTY-NOTICES.md"),
            "notices of the new build"
        );
        assert_eq!(read(dir.path(), "LICENSE.txt"), "license of the new build");
        assert_eq!(read(dir.path(), "README.txt"), "readme of new build");
        assert!(!current.exists());
    }

    /// A folder of the user's at the next batch's name, with a file named
    /// like a document: staging fails and leaves both files, a discard and an
    /// install pass leave them too, so the update stops before its swap.
    #[test]
    fn a_next_folder_that_is_no_batch_stops_the_staging_and_keeps_its_files() {
        let dir = folder(Some(KEY));
        let next = dir.path().join(NEXT_DIR);
        std::fs::create_dir(&next).unwrap();
        std::fs::write(next.join("notes.txt"), "my notes").unwrap();
        std::fs::write(next.join("README.txt"), "my readme").unwrap();
        let (_stage, exe_b, key_b, readme_b) = new_exe(b"B");
        let refused =
            stage_pending_docs(dir.path(), &exe_b, &[(&readme_b, "README.txt")], "u").unwrap_err();
        assert!(refused.contains("is not an update batch"), "{refused}");
        assert!(!dir.path().join(format!("{TMP_PREFIX}u")).exists());
        discard_pending_docs(dir.path(), &key_b);
        let Pending::Docs(_) = install_pending_docs(dir.path(), KEY, rename) else {
            panic!("the running exe's own batch still installs")
        };
        assert_eq!(read(&next, "notes.txt"), "my notes");
        assert_eq!(read(&next, "README.txt"), "my readme");
        assert_eq!(read(dir.path(), "README.txt"), "new README.txt");
    }

    #[cfg(unix)]
    #[test]
    fn a_retry_that_cannot_take_the_lock_keeps_the_held_result() {
        let dir = folder(Some(KEY));
        let busy = RefCell::new(vec!["README.txt"]);
        let lock = std::env::temp_dir().join(format!(
            "aeroftp-pending-docs-{}.lock",
            folder_id(dir.path())
        ));
        // Between the passes the lock cannot be taken (a folder at its path).
        let outcome = install_with_shared_budget(
            dir.path(),
            KEY,
            2,
            Duration::from_secs(1),
            held(&busy, ERROR_SHARING_VIOLATION),
            |_| {
                let _ = std::fs::remove_file(&lock);
                let _ = std::fs::create_dir(&lock);
            },
        );
        let _ = std::fs::remove_dir(&lock);
        let Pending::Docs(docs) = outcome else {
            panic!("the held result is kept: {outcome:?}");
        };
        assert!(docs.contains(&("README.txt", Outcome::InUse)), "{docs:?}");
        assert_eq!(
            read(&dir.path().join(PENDING_DIR), "README.txt"),
            "new README.txt"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_batch_whose_key_cannot_be_read_now_is_kept_and_installed_later() {
        use std::os::unix::fs::PermissionsExt;
        let dir = folder(Some(KEY));
        let key = dir.path().join(PENDING_DIR).join(KEY_FILE);
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&key).is_ok() {
            return; // as root every file is readable: nothing to hold
        }
        let replace = |_: &Path, _: &Path| -> io::Result<()> {
            panic!("nothing is installed while the key cannot be read")
        };
        assert!(matches!(
            install_pending_docs(dir.path(), KEY, replace),
            Pending::Unavailable(why) if why.contains("cannot be read now")
        ));
        assert_eq!(
            read(&dir.path().join(PENDING_DIR), "README.txt"),
            "new README.txt"
        );
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
        install_pending_docs(dir.path(), KEY, rename);
        assert_eq!(read(dir.path(), "README.txt"), "new README.txt");
    }

    #[test]
    fn cleanup_removes_only_what_a_staging_writes_and_keeps_an_unrelated_folder() {
        let dir = folder(Some("other-exe"));
        // A folder that only has a staging name, with content of its own.
        let unrelated = dir.path().join(format!("{TMP_PREFIX}user"));
        std::fs::create_dir_all(unrelated.join("sub")).unwrap();
        std::fs::write(unrelated.join("notes.txt"), "mine").unwrap();
        // Named like a document a staging writes, but the user's.
        std::fs::write(unrelated.join("README.txt"), "my readme").unwrap();
        std::fs::write(unrelated.join("sub").join("deep.txt"), "mine too").unwrap();
        // A folder with a staging name that holds a key file and a README
        // too, as a staging does: not created by a staging, so not one.
        let keyed = dir.path().join(format!("{TMP_PREFIX}keyed"));
        std::fs::create_dir(&keyed).unwrap();
        std::fs::write(keyed.join(KEY_FILE), "my key").unwrap();
        std::fs::write(keyed.join("README.txt"), "my readme").unwrap();
        // A staging an update stopped in the middle: the folder it created
        // and recorded, with documents and key.
        let stopped = dir.path().join(format!("{TMP_PREFIX}stopped"));
        std::fs::create_dir(&stopped).unwrap();
        begin_staging(dir.path(), &format!("{TMP_PREFIX}stopped")).unwrap();
        std::fs::write(stopped.join("README.txt"), "half staged").unwrap();
        std::fs::write(stopped.join(KEY_FILE), "some-exe").unwrap();
        // A current batch that lost its key, with a file nobody staged in it.
        std::fs::remove_file(dir.path().join(PENDING_DIR).join(KEY_FILE)).unwrap();
        std::fs::write(dir.path().join(PENDING_DIR).join("user.txt"), "kept").unwrap();
        let replace = |_: &Path, _: &Path| -> io::Result<()> {
            panic!("a stale document is never installed")
        };
        assert_eq!(
            install_pending_docs(dir.path(), KEY, replace),
            Pending::None
        );
        assert!(!stopped.exists(), "the stopped staging goes whole");
        assert!(
            recorded_stagings(dir.path()).is_empty(),
            "and leaves the record"
        );
        assert_eq!(read(&keyed, KEY_FILE), "my key");
        assert_eq!(read(&keyed, "README.txt"), "my readme");
        assert_eq!(read(&unrelated, "notes.txt"), "mine");
        assert_eq!(read(&unrelated, "README.txt"), "my readme");
        assert_eq!(read(&unrelated.join("sub"), "deep.txt"), "mine too");
        // Without its key it is no batch any more: left whole.
        let pending = dir.path().join(PENDING_DIR);
        assert_eq!(read(&pending, "user.txt"), "kept");
        for name in DOCS {
            assert_eq!(read(&pending, name), format!("new {name}"));
        }
        // Staging and a discard leave them as they are too.
        let (_stage, exe_b, _key_b, readme_b) = new_exe(b"B");
        let token_b = stage_pending_docs(dir.path(), &exe_b, &[(&readme_b, "README.txt")], "b")
            .unwrap()
            .unwrap();
        discard_pending_docs(dir.path(), &token_b);
        assert!(!dir.path().join(NEXT_DIR).exists());
        assert_eq!(read(&unrelated.join("sub"), "deep.txt"), "mine too");
        assert_eq!(read(&pending, "user.txt"), "kept");
        // A staging whose folder name is taken fails without touching it.
        let taken = dir.path().join(format!("{TMP_PREFIX}taken"));
        std::fs::create_dir(&taken).unwrap();
        std::fs::write(taken.join("README.txt"), "not ours").unwrap();
        std::fs::write(taken.join("own.txt"), "not ours").unwrap();
        let failed = stage_with(
            dir.path(),
            &exe_b,
            &[(&readme_b, "README.txt")],
            "taken",
            |_, _| Ok(()),
        );
        assert!(failed.is_err());
        assert_eq!(read(&taken, "own.txt"), "not ours");
        assert_eq!(read(&taken, "README.txt"), "not ours");
        // A later sweep leaves the folders that are not stagings whole.
        discard_pending_docs(dir.path(), &token_b);
        assert_eq!(read(&taken, "README.txt"), "not ours");
        assert_eq!(read(&unrelated, "README.txt"), "my readme");
        assert_eq!(read(&keyed, KEY_FILE), "my key");
        assert_eq!(read(&keyed, "README.txt"), "my readme");
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

    /// Each staging attempt gets a distinct per-attempt ownership token; a
    /// same-exe retry that reuses the staged bytes transfers ownership under
    /// the lock to a new token so cleanup from the earlier attempt cannot
    /// delete the batch, while cleanup with the current token still removes
    /// it. No documents -> no batch, no token.
    #[test]
    fn staging_returns_a_unique_attempt_token_and_transfers_ownership_on_reuse() {
        let dir = folder(None);
        let (_stage, exe_b, key_b, readme_b) = new_exe(b"B");
        let next = dir.path().join(NEXT_DIR);
        let first_token = stage_pending_docs(dir.path(), &exe_b, &[(&readme_b, "README.txt")], "k")
            .unwrap()
            .expect("first token");
        assert_eq!(read(&next, KEY_FILE), key_b);
        assert_eq!(read(&next, STAGING_ID_FILE), first_token);
        // Same-exe retry reuses the complete staged bytes and transfers
        // ownership to a distinct token.
        let retry_token = stage_pending_docs(dir.path(), &exe_b, &[(&readme_b, "README.txt")], "l")
            .unwrap()
            .expect("retry token");
        assert_ne!(
            retry_token, first_token,
            "retry gets a distinct ownership token"
        );
        assert_eq!(read(&next, STAGING_ID_FILE), retry_token);
        assert_eq!(read(&next, KEY_FILE), key_b);
        // Earlier attempt's cleanup sees the old token no longer owns NEXT_DIR.
        discard_pending_docs(dir.path(), &first_token);
        assert!(
            next.is_dir(),
            "reused batch survives earlier attempt's cleanup"
        );
        assert_eq!(read(&next, "README.txt"), "readme of B");
        // Control: cleanup with the current attempt's token (no later retry)
        // removes the batch cleanly.
        discard_pending_docs(dir.path(), &retry_token);
        assert!(!next.exists(), "current owner's discard removes the batch");
        assert_eq!(
            stage_pending_docs(dir.path(), &exe_b, &[], "m").unwrap(),
            None
        );
    }

    /// An older batch superseded while one of its documents cannot be
    /// removed (on Windows an editor holds it; here a folder at the document's
    /// name, which `remove_file` cannot remove) is retired, never installed,
    /// and a later pass removes it once the document can go.
    #[test]
    fn a_superseded_batch_that_cannot_be_removed_now_goes_on_a_later_pass() {
        let dir = folder(Some("older-exe-key"));
        let current = dir.path().join(PENDING_DIR);
        let held = current.join(DOCS[3]);
        std::fs::remove_file(&held).unwrap();
        std::fs::create_dir(&held).unwrap();
        let (_stage, exe_b, key_b, readme_b) = new_exe(b"B");
        stage_pending_docs(dir.path(), &exe_b, &[(&readme_b, "README.txt")], "r").unwrap();
        let first = install_pending_docs(dir.path(), &key_b, rename);
        assert!(matches!(first, Pending::Docs(_)), "{first:?}");
        assert_eq!(read(dir.path(), "README.txt"), "readme of B");
        assert!(held.is_dir(), "what cannot be removed now stays");
        assert_eq!(
            std::fs::read_to_string(current.join(KEY_FILE)).unwrap(),
            RETIRED_KEY
        );
        assert_eq!(
            install_pending_docs(dir.path(), &key_b, rename),
            Pending::None
        );
        assert!(current.exists(), "still held");
        std::fs::remove_dir(&held).unwrap();
        assert_eq!(
            install_pending_docs(dir.path(), &key_b, rename),
            Pending::Stale
        );
        assert!(!current.exists(), "the retired batch goes once released");
    }

    /// The staging record names the folder as written: on a case-sensitive
    /// volume `App` and `app` are two folders, and one never sweeps the
    /// other's stagings.
    #[test]
    fn the_staging_record_of_a_folder_is_not_the_record_of_its_other_case() {
        let upper = staging_record(Path::new("/portable/App"));
        let lower = staging_record(Path::new("/portable/app"));
        assert_ne!(upper, lower);
        assert_eq!(upper, staging_record(Path::new("/portable/App")));
    }

    /// A lock released only after the deadline is not taken: the last pause
    /// is cut to the time left and the deadline is checked before the next
    /// probe (a full 5 ms pause and a probe after it would take the lock 1 ms
    /// past the deadline). A lock released before the deadline is taken.
    #[test]
    fn a_lock_released_past_the_deadline_is_not_taken() {
        let start = std::time::Instant::now();
        let clock = Cell::new(start);
        let deadline = start + Duration::from_millis(98);
        let free_at = |at: u64| {
            let release = start + Duration::from_millis(at);
            let probes = Cell::new(0u32);
            let result = wait_for_lock(
                Some(deadline),
                || {
                    assert!(clock.get() <= deadline, "a probe past the deadline");
                    probes.set(probes.get() + 1);
                    Ok(clock.get() >= release)
                },
                || clock.get(),
                |d| clock.set(clock.get() + d),
            );
            clock.set(start);
            (result, probes.get())
        };
        let (late, _) = free_at(99);
        assert_eq!(late.unwrap_err().kind(), io::ErrorKind::TimedOut);
        let (early, probes) = free_at(40);
        assert!(early.is_ok(), "{early:?}");
        assert_eq!(probes, 9, "probes every 5 ms from the start");
    }

    /// Windows: a mutex another user's session created refuses this one with
    /// `ERROR_ACCESS_DENIED`. That is the lock held by that session: probed
    /// again until it opens, never taken as free, and past the deadline a
    /// `TimedOut` like any held lock, never another error. Any other error
    /// is returned as it is.
    #[test]
    fn a_lock_another_session_holds_is_waited_for_and_never_taken_as_free() {
        let start = std::time::Instant::now();
        let clock = Cell::new(start);
        let deadline = Some(start + Duration::from_millis(50));
        let denied = || io::Error::from_raw_os_error(ERROR_ACCESS_DENIED);
        // Held past the deadline.
        let opens = Cell::new(0u32);
        let held = open_named_lock::<()>(
            deadline,
            || {
                opens.set(opens.get() + 1);
                Err(denied())
            },
            || clock.get(),
            |d| clock.set(clock.get() + d),
        );
        assert_eq!(held.unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert!(opens.get() > 1, "probed again while held");
        // Released by the other session before the deadline: opened then.
        clock.set(start);
        let opens = Cell::new(0u32);
        let released = open_named_lock(
            deadline,
            || {
                opens.set(opens.get() + 1);
                if opens.get() < 4 {
                    Err(denied())
                } else {
                    Ok("handle")
                }
            },
            || clock.get(),
            |d| clock.set(clock.get() + d),
        );
        assert_eq!(released.unwrap(), "handle");
        // No deadline: waited for as long as it takes.
        clock.set(start);
        let opens = Cell::new(0u32);
        let waited = open_named_lock(
            None,
            || {
                opens.set(opens.get() + 1);
                if opens.get() < 1000 {
                    Err(denied())
                } else {
                    Ok(())
                }
            },
            || clock.get(),
            |d| clock.set(clock.get() + d),
        );
        assert!(waited.is_ok());
        // Another error is no held lock: returned at once.
        let opens = Cell::new(0u32);
        let other = open_named_lock::<()>(
            deadline,
            || {
                opens.set(opens.get() + 1);
                Err(io::Error::from_raw_os_error(ERROR_SHARING_VIOLATION))
            },
            || clock.get(),
            |d| clock.set(clock.get() + d),
        );
        assert_eq!(
            other.unwrap_err().raw_os_error(),
            Some(ERROR_SHARING_VIOLATION)
        );
        assert_eq!(opens.get(), 1);
    }

    /// Another process holds the lock (a stalled staging): the pass before
    /// the window gives up at its deadline and touches nothing, and once the
    /// lock is released a later pass installs the documents.
    #[test]
    fn a_start_pass_stops_waiting_at_its_deadline_and_a_later_pass_installs() {
        let dir = folder(Some(KEY));
        let path = dir.path().to_path_buf();
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let holder = std::thread::spawn(move || {
            let _lock = lock_batches(&path).unwrap();
            held_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });
        held_rx.recv().unwrap();
        // The pass runs on its own thread, so a pass that never gives up
        // fails the test at the timeout instead of hanging it: the holder is
        // released first, then both threads are joined.
        let pass_dir = dir.path().to_path_buf();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let pass = std::thread::spawn(move || {
            let outcome =
                install_pending_docs_within(&pass_dir, KEY, Duration::from_millis(100), rename);
            let _ = done_tx.send(outcome);
        });
        let first = match done_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(first) => first,
            Err(e) => {
                release_tx.send(()).unwrap();
                holder.join().unwrap();
                pass.join().unwrap();
                panic!("the pass did not give up at its deadline: {e}");
            }
        };
        pass.join().unwrap();
        assert!(matches!(first, Pending::Unavailable(_)), "{first:?}");
        for name in DOCS {
            assert_eq!(read(dir.path(), name), format!("old {name}"));
            assert_eq!(
                read(&dir.path().join(PENDING_DIR), name),
                format!("new {name}")
            );
        }
        release_tx.send(()).unwrap();
        holder.join().unwrap();
        assert_eq!(
            install_pending_docs(dir.path(), KEY, rename),
            Pending::Docs(DOCS.iter().map(|n| (*n, Outcome::Installed)).collect())
        );
    }

    /// A batch of this exe that lacks a document asked for (a discard that
    /// could not remove all of them) is staged again, not reused; one that
    /// cannot be staged again stops the update before its swap. A whole
    /// batch is reused (staging_returns_the_key_its_batch_carries).
    #[test]
    fn a_batch_of_this_exe_that_lacks_a_document_is_staged_again() {
        let dir = tempfile::tempdir().unwrap();
        let (stage, exe_b, key_b, readme_b) = new_exe(b"B");
        let license_b = stage.path().join("LICENSE.txt");
        std::fs::write(&license_b, "license of B").unwrap();
        let docs = [
            (readme_b.as_path(), "README.txt"),
            (license_b.as_path(), "LICENSE.txt"),
        ];
        stage_pending_docs(dir.path(), &exe_b, &docs, "a").unwrap();
        let next = dir.path().join(NEXT_DIR);
        std::fs::remove_file(next.join("README.txt")).unwrap();
        let restaged = stage_pending_docs(dir.path(), &exe_b, &docs, "b")
            .unwrap()
            .expect("restaged token");
        assert_eq!(read(&next, KEY_FILE), key_b);
        assert_eq!(read(&next, STAGING_ID_FILE), restaged);
        assert_eq!(read(&next, "README.txt"), "readme of B");
        assert_eq!(read(&next, "LICENSE.txt"), "license of B");
        // A document that cannot be removed (here a folder at its name): the
        // batch cannot be staged again, and the staging fails.
        std::fs::remove_file(next.join("README.txt")).unwrap();
        std::fs::remove_file(next.join("LICENSE.txt")).unwrap();
        std::fs::create_dir(next.join("LICENSE.txt")).unwrap();
        assert!(stage_pending_docs(dir.path(), &exe_b, &docs, "c").is_err());
        assert!(next.join("LICENSE.txt").is_dir());
    }

    /// The running exe B's notices wait in the next batch (an older exe's
    /// current batch could not be removed, an editor held B's notices). A
    /// later update C keeps that batch until C replaces B: a publication
    /// that fails puts it back at once, a discarded update (the helper could
    /// not start) puts it back, and in both B installs its notices once they
    /// are released. When C does replace B, C installs its own documents and
    /// B's batch goes.
    #[test]
    fn a_failed_update_keeps_the_held_batch_of_the_running_exe_until_its_exe_is_replaced() {
        const NOTICES: &str = "THIRD-PARTY-NOTICES.md";
        let notices_of = |stage: &tempfile::TempDir, who: &str| {
            let path = stage.path().join(NOTICES);
            std::fs::write(&path, format!("notices of {who}")).unwrap();
            path
        };
        // B's held batch in the next slot.
        let setup = || {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join(NOTICES), "installed notices").unwrap();
            let current = dir.path().join(PENDING_DIR);
            std::fs::create_dir(&current).unwrap();
            std::fs::write(current.join(KEY_FILE), "older-exe-key").unwrap();
            // A document of the older batch that cannot be removed.
            std::fs::create_dir(current.join(DOCS[1])).unwrap();
            let (stage_b, exe_b, key_b, _) = new_exe(b"B");
            let notices_b = notices_of(&stage_b, "B");
            stage_pending_docs(dir.path(), &exe_b, &[(&notices_b, NOTICES)], "b").unwrap();
            let busy = RefCell::new(vec![NOTICES]);
            let first =
                install_pending_docs(dir.path(), &key_b, held(&busy, ERROR_SHARING_VIOLATION));
            assert_eq!(first, Pending::Docs(vec![(NOTICES, Outcome::InUse)]));
            assert_eq!(read(&dir.path().join(NEXT_DIR), KEY_FILE), key_b);
            (dir, key_b)
        };
        let b_installs = |dir: &Path, key_b: &str| {
            assert_eq!(read(&dir.join(NEXT_DIR), KEY_FILE), key_b);
            assert_eq!(read(&dir.join(NEXT_DIR), NOTICES), "notices of B");
            assert_eq!(
                install_pending_docs(dir, key_b, rename),
                Pending::Docs(vec![(NOTICES, Outcome::Installed)])
            );
            assert_eq!(read(dir, NOTICES), "notices of B");
        };
        let (stage_c, exe_c, key_c, _) = new_exe(b"C");
        let notices_c = notices_of(&stage_c, "C");
        // C's publication fails after its documents were staged.
        let (dir, key_b) = setup();
        let failed = stage_with(dir.path(), &exe_c, &[(&notices_c, NOTICES)], "c", |_, _| {
            Err(io::Error::other("publication refused"))
        });
        assert!(failed.is_err());
        assert!(!dir.path().join(PREV_DIR).exists());
        b_installs(dir.path(), &key_b);
        // C staged, then the update failed before the swap.
        let (dir, key_b) = setup();
        let token_c = stage_pending_docs(dir.path(), &exe_c, &[(&notices_c, NOTICES)], "c")
            .unwrap()
            .unwrap();
        assert_eq!(read(&dir.path().join(PREV_DIR), KEY_FILE), key_b);
        discard_pending_docs(dir.path(), &token_c);
        assert!(!dir.path().join(PREV_DIR).exists());
        b_installs(dir.path(), &key_b);
        // C staged, and B still runs: B installs from the batch set aside.
        let (dir, key_b) = setup();
        stage_pending_docs(dir.path(), &exe_c, &[(&notices_c, NOTICES)], "c").unwrap();
        assert_eq!(
            install_pending_docs(dir.path(), &key_b, rename),
            Pending::Docs(vec![(NOTICES, Outcome::Installed)])
        );
        assert_eq!(read(dir.path(), NOTICES), "notices of B");
        assert_eq!(read(&dir.path().join(NEXT_DIR), KEY_FILE), key_c);
        // The control: C replaced B, installs its own, and B's batch goes.
        let (dir, _key_b) = setup();
        stage_pending_docs(dir.path(), &exe_c, &[(&notices_c, NOTICES)], "c").unwrap();
        assert_eq!(
            install_pending_docs(dir.path(), &key_c, rename),
            Pending::Docs(vec![(NOTICES, Outcome::Installed)])
        );
        assert_eq!(read(dir.path(), NOTICES), "notices of C");
        assert!(!dir.path().join(PREV_DIR).exists());
    }

    /// The next batch's key cannot be read at the first pass (here a folder
    /// at its name): the pass reports it unavailable, never "nothing to do",
    /// and the same start installs the batch once the key can be read. A
    /// readable batch of another exe is left untouched.
    #[test]
    fn a_next_batch_whose_key_cannot_be_read_yet_is_installed_by_the_same_start() {
        let dir = tempfile::tempdir().unwrap();
        for name in DOCS {
            std::fs::write(dir.path().join(name), format!("old {name}")).unwrap();
        }
        let next = dir.path().join(NEXT_DIR);
        std::fs::create_dir(&next).unwrap();
        std::fs::write(next.join("README.txt"), "new README.txt").unwrap();
        std::fs::create_dir(next.join(KEY_FILE)).unwrap();
        let first = install_pending_docs(dir.path(), KEY, rename);
        assert!(
            matches!(&first, Pending::Unavailable(why) if why.contains("cannot be read now")),
            "{first:?}"
        );
        assert_eq!(read(dir.path(), "README.txt"), "old README.txt");
        // The key becomes readable during the start's retry budget.
        let released = Cell::new(false);
        let outcome = retry_held(dir.path(), KEY, first, 3, Duration::ZERO, rename, |_| {
            if !released.replace(true) {
                std::fs::remove_dir(next.join(KEY_FILE)).unwrap();
                std::fs::write(next.join(KEY_FILE), KEY).unwrap();
            }
        });
        assert_eq!(
            outcome,
            Pending::Docs(vec![("README.txt", Outcome::Installed)])
        );
        assert_eq!(read(dir.path(), "README.txt"), "new README.txt");
        // Another exe's readable next batch: nothing to do, and kept.
        std::fs::create_dir(&next).unwrap();
        std::fs::write(next.join("README.txt"), "other README").unwrap();
        std::fs::write(next.join(KEY_FILE), "other-exe-key").unwrap();
        assert_eq!(install_pending_docs(dir.path(), KEY, rename), Pending::None);
        assert_eq!(read(&next, "README.txt"), "other README");
    }

    /// Another process holds the lock past the lookup's wait (a stalled
    /// staging): About gets no pending notices within that wait, so it opens
    /// the installed ones (thirdPartyLicenses.ts tries the installed file
    /// when no pending one is given), and once the lock is free it gets the
    /// pending ones again.
    #[test]
    fn the_pending_notices_lookup_stops_waiting_for_a_held_lock() {
        let dir = folder(Some(KEY));
        let path = dir.path().to_path_buf();
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let holder = std::thread::spawn(move || {
            let _lock = lock_batches(&path).unwrap();
            held_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });
        held_rx.recv().unwrap();
        let lookup_dir = dir.path().to_path_buf();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let lookup = std::thread::spawn(move || {
            let found = own_pending_notices(&lookup_dir, || Some(KEY.to_string()));
            let _ = done_tx.send(found);
        });
        // Check while the holder still owns the lock (before releasing it);
        // exact deadline behavior is pinned by the fake-clock test above,
        // without relying on Windows WaitForSingleObject timer granularity.
        let outcome = done_rx.recv_timeout(Duration::from_secs(10));
        let found = outcome.expect("the lookup did not give up at its wait");
        assert_eq!(found, None);
        release_tx.send(()).unwrap();
        holder.join().unwrap();
        lookup.join().unwrap();
        assert!(read(dir.path(), "THIRD-PARTY-NOTICES.md").starts_with("old"));
        assert!(own_pending_notices(dir.path(), || Some(KEY.to_string())).is_some());
    }

    /// If another exe's batch is pending in NEXT_DIR and PREV_DIR cannot take
    /// it (an unrelated folder occupies PREV_DIR), staging fails without
    /// modifying or deleting that batch, so its exe still installs once the
    /// held file is released. Control: with PREV_DIR free, staging succeeds.
    #[test]
    fn staging_fails_without_dropping_another_exes_batch_when_prev_is_occupied() {
        const NOTICES: &str = "THIRD-PARTY-NOTICES.md";
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(NOTICES), "installed notices").unwrap();
        let (stage_b, exe_b, key_b, _) = new_exe(b"B");
        let notices_b = stage_b.path().join(NOTICES);
        std::fs::write(&notices_b, "notices of B").unwrap();
        stage_pending_docs(dir.path(), &exe_b, &[(&notices_b, NOTICES)], "b").unwrap();
        // Occupy PREV_DIR with an unrelated user folder.
        let prev = dir.path().join(PREV_DIR);
        std::fs::create_dir(&prev).unwrap();
        std::fs::write(prev.join("user.txt"), "user file").unwrap();
        let (stage_c, exe_c, _key_c, _) = new_exe(b"C");
        let notices_c = stage_c.path().join(NOTICES);
        std::fs::write(&notices_c, "notices of C").unwrap();
        assert!(stage_pending_docs(dir.path(), &exe_c, &[(&notices_c, NOTICES)], "c").is_err());
        // Both PREV_DIR and B's batch in NEXT_DIR stay intact.
        assert_eq!(read(&prev, "user.txt"), "user file");
        assert_eq!(read(&dir.path().join(NEXT_DIR), KEY_FILE), key_b);
        assert_eq!(read(&dir.path().join(NEXT_DIR), NOTICES), "notices of B");
        // Control: once PREV_DIR is free, C stages and sets B's batch aside.
        std::fs::remove_file(prev.join("user.txt")).unwrap();
        std::fs::remove_dir(&prev).unwrap();
        stage_pending_docs(dir.path(), &exe_c, &[(&notices_c, NOTICES)], "c2").unwrap();
        assert_eq!(read(&prev, KEY_FILE), key_b);
        // B still installs its notices from the set-aside batch.
        assert_eq!(
            install_pending_docs(dir.path(), &key_b, rename),
            Pending::Docs(vec![(NOTICES, Outcome::Installed)])
        );
        assert_eq!(read(dir.path(), NOTICES), "notices of B");
    }

    /// Only a batch AeroFTP itself staged (recorded in `batch_record`) is
    /// retired on promotion or staging: an unowned folder with its own
    /// `exe.key` and `README.txt` at PREV_DIR (or PENDING_DIR) survives both
    /// promotion and staging unchanged, while a superseded AeroFTP batch goes.
    #[test]
    fn an_unowned_folder_with_a_key_at_prev_survives_promotion_and_staging_while_owned_batches_go()
    {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("README.txt"), "installed").unwrap();
        let prev = dir.path().join(PREV_DIR);
        std::fs::create_dir(&prev).unwrap();
        std::fs::write(prev.join(KEY_FILE), "unowned-key").unwrap();
        std::fs::write(prev.join("README.txt"), "user prev readme").unwrap();
        let current = dir.path().join(PENDING_DIR);
        std::fs::create_dir(&current).unwrap();
        std::fs::write(current.join(KEY_FILE), "unowned-current-key").unwrap();
        std::fs::write(current.join("README.txt"), "user current readme").unwrap();
        // Promotion of a real staged batch leaves both unowned folders intact.
        let (_stage_a, exe_a, key_a, readme_a) = new_exe(b"A");
        stage_pending_docs(dir.path(), &exe_a, &[(&readme_a, "README.txt")], "a").unwrap();
        assert_eq!(
            install_pending_docs(dir.path(), &key_a, rename),
            Pending::Docs(vec![("README.txt", Outcome::Installed)])
        );
        assert_eq!(read(dir.path(), "README.txt"), "readme of A");
        assert_eq!(read(&prev, KEY_FILE), "unowned-key");
        assert_eq!(read(&prev, "README.txt"), "user prev readme");
        assert_eq!(read(&current, KEY_FILE), "unowned-current-key");
        assert_eq!(read(&current, "README.txt"), "user current readme");
        // Staging another exe while B's batch waits in NEXT_DIR cannot retire
        // the unowned PREV_DIR, so both PREV_DIR and B's batch stay intact.
        let (_stage_b, exe_b, key_b, readme_b) = new_exe(b"B");
        stage_pending_docs(dir.path(), &exe_b, &[(&readme_b, "README.txt")], "b").unwrap();
        let (_stage_c, exe_c, _key_c, readme_c) = new_exe(b"C");
        assert!(stage_pending_docs(dir.path(), &exe_c, &[(&readme_c, "README.txt")], "c").is_err());
        assert_eq!(read(&prev, KEY_FILE), "unowned-key");
        assert_eq!(read(&prev, "README.txt"), "user prev readme");
        assert_eq!(read(&dir.path().join(NEXT_DIR), KEY_FILE), key_b);
        // Control: remove the unowned folders and verify a real superseded
        // AeroFTP batch at PREV_DIR and PENDING_DIR is removed on promotion.
        std::fs::remove_dir_all(&prev).unwrap();
        std::fs::remove_dir_all(&current).unwrap();
        // C stages -> B's owned batch moves to PREV_DIR.
        stage_pending_docs(dir.path(), &exe_c, &[(&readme_c, "README.txt")], "c2").unwrap();
        assert_eq!(read(&prev, KEY_FILE), key_b);
        // Also place an owned superseded batch in PENDING_DIR.
        let owned_old_id = format!("{:032x}", rand::random::<u128>());
        std::fs::create_dir(&current).unwrap();
        std::fs::write(current.join(KEY_FILE), "old-owned-key").unwrap();
        std::fs::write(current.join(STAGING_ID_FILE), &owned_old_id).unwrap();
        std::fs::write(current.join("README.txt"), "old owned readme").unwrap();
        record_batch(dir.path(), &owned_old_id).unwrap();
        let key_c = exe_key(&exe_c).unwrap();
        assert_eq!(
            install_pending_docs(dir.path(), &key_c, rename),
            Pending::Docs(vec![("README.txt", Outcome::Installed)])
        );
        assert_eq!(read(dir.path(), "README.txt"), "readme of C");
        assert!(!prev.exists(), "superseded owned prev batch is removed");
        assert!(
            !current.exists(),
            "superseded owned current batch is removed"
        );
    }

    /// A transient unreadable key in NEXT_DIR during `retry_held` does not
    /// cut off the remaining retry budget: once both the key and the held
    /// document become available, the same start installs the document.
    #[test]
    fn a_transient_unreadable_next_key_during_retry_still_installs_held_documents() {
        let dir = folder(Some(KEY));
        let busy = RefCell::new(vec!["THIRD-PARTY-NOTICES.md"]);
        let first = install_pending_docs(dir.path(), KEY, held(&busy, ERROR_SHARING_VIOLATION));
        assert!(
            matches!(&first, Pending::Docs(docs) if docs.contains(&("THIRD-PARTY-NOTICES.md", Outcome::InUse)))
        );
        let next = dir.path().join(NEXT_DIR);
        std::fs::create_dir(&next).unwrap();
        std::fs::create_dir(next.join(KEY_FILE)).unwrap();
        let pass = Cell::new(0u32);
        let outcome = retry_held(
            dir.path(),
            KEY,
            first,
            4,
            Duration::ZERO,
            held(&busy, ERROR_SHARING_VIOLATION),
            |_| {
                let n = pass.get() + 1;
                pass.set(n);
                // Pass 1 sees the unreadable next key (Pending::Unavailable);
                // before pass 2 both the next key and the held document clear.
                if n == 2 {
                    std::fs::remove_dir(next.join(KEY_FILE)).unwrap();
                    std::fs::write(next.join(KEY_FILE), "other-exe-key").unwrap();
                    busy.borrow_mut().clear();
                }
            },
        );
        assert!(
            matches!(&outcome, Pending::Docs(docs) if docs.contains(&("THIRD-PARTY-NOTICES.md", Outcome::Installed))),
            "{outcome:?}"
        );
        assert_eq!(
            read(dir.path(), "THIRD-PARTY-NOTICES.md"),
            "new THIRD-PARTY-NOTICES.md"
        );
    }

    /// `discard_pending_docs` acquires the same batch lock before checking
    /// `exe.key`: if another update replaces `NEXT_DIR` while holding the
    /// lock, the waiting discard sees the new key once the lock is released
    /// and leaves the replacement batch whole.
    #[test]
    fn discard_waits_for_the_lock_and_keeps_a_replacement_batch_published_before_it_runs() {
        let dir = tempfile::tempdir().unwrap();
        let (_stage_a, exe_a, _key_a, readme_a) = new_exe(b"A");
        let token_a = stage_pending_docs(dir.path(), &exe_a, &[(&readme_a, "README.txt")], "a")
            .unwrap()
            .unwrap();
        let next = dir.path().join(NEXT_DIR);
        let lock = lock_batches(dir.path()).unwrap();
        let discard_dir = dir.path().to_path_buf();
        let token_a_clone = token_a.clone();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let discarder = std::thread::spawn(move || {
            discard_pending_docs(&discard_dir, &token_a_clone);
            let _ = done_tx.send(());
        });
        // While the lock is held, discard_pending_docs cannot proceed.
        assert!(done_rx.recv_timeout(Duration::from_millis(80)).is_err());
        // Another update publishes a replacement batch while holding the lock.
        let replacement_key = "ab".repeat(32);
        let replacement_id = "ef".repeat(16);
        std::fs::write(next.join(KEY_FILE), &replacement_key).unwrap();
        std::fs::write(next.join(STAGING_ID_FILE), &replacement_id).unwrap();
        std::fs::write(next.join("README.txt"), "replacement readme").unwrap();
        drop(lock);
        done_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("discard finishes once lock is released");
        discarder.join().unwrap();
        assert_eq!(read(&next, KEY_FILE), replacement_key);
        assert_eq!(read(&next, "README.txt"), "replacement readme");
    }

    /// `discard_pending_docs` requires `is_owned_batch` before deleting: an
    /// unowned folder at `NEXT_DIR` with a matching `exe.key` (and even a
    /// matching unrecorded `staging.id`) stays untouched, while a real
    /// registered AeroFTP batch is removed.
    #[test]
    fn discard_refuses_an_unowned_folder_even_with_matching_key_and_removes_owned_batch() {
        let dir = tempfile::tempdir().unwrap();
        let (_stage, exe_b, key_b, readme_b) = new_exe(b"B");
        let next = dir.path().join(NEXT_DIR);
        let unrecorded_id = "11".repeat(16);
        std::fs::create_dir(&next).unwrap();
        std::fs::write(next.join(KEY_FILE), &key_b).unwrap();
        std::fs::write(next.join(STAGING_ID_FILE), &unrecorded_id).unwrap();
        std::fs::write(next.join("README.txt"), "user readme").unwrap();
        discard_pending_docs(dir.path(), &unrecorded_id);
        discard_pending_docs(dir.path(), &key_b);
        assert_eq!(read(&next, "README.txt"), "user readme");
        assert_eq!(read(&next, KEY_FILE), key_b);
        // Control: a real registered AeroFTP batch is removed by its token.
        std::fs::remove_dir_all(&next).unwrap();
        let token = stage_pending_docs(dir.path(), &exe_b, &[(&readme_b, "README.txt")], "b")
            .unwrap()
            .unwrap();
        discard_pending_docs(dir.path(), &token);
        assert!(!next.exists());
    }

    /// Ownership evidence lives durably beside the batches inside `exe_dir`:
    /// deleting the temp-dir mirror or moving the entire portable folder to a
    /// new directory never makes an existing AeroFTP batch lose ownership or
    /// block future updates.
    #[test]
    fn batch_ownership_survives_temp_cleanup_and_moving_the_portable_folder() {
        const NOTICES: &str = "THIRD-PARTY-NOTICES.md";
        let root = tempfile::tempdir().unwrap();
        let orig = root.path().join("portable-a");
        std::fs::create_dir(&orig).unwrap();
        std::fs::write(orig.join(NOTICES), "installed").unwrap();
        let (stage_b, exe_b, key_b, _) = new_exe(b"B");
        let notices_b = stage_b.path().join(NOTICES);
        std::fs::write(&notices_b, "notices of B").unwrap();
        stage_pending_docs(&orig, &exe_b, &[(&notices_b, NOTICES)], "b").unwrap();
        let (stage_c, exe_c, key_c, _) = new_exe(b"C");
        let notices_c = stage_c.path().join(NOTICES);
        std::fs::write(&notices_c, "notices of C").unwrap();
        stage_pending_docs(&orig, &exe_c, &[(&notices_c, NOTICES)], "c").unwrap();
        assert_eq!(read(&orig.join(PREV_DIR), KEY_FILE), key_b);
        // Wipe the temp-dir mirror AND move the entire portable folder.
        let _ = std::fs::remove_file(temp_batch_record(&orig));
        let moved = root.path().join("portable-moved");
        std::fs::rename(&orig, &moved).unwrap();
        assert!(is_owned_batch(&moved, &moved.join(PREV_DIR)));
        assert!(is_owned_batch(&moved, &moved.join(NEXT_DIR)));
        // C promotes in the moved folder and retires B's set-aside batch.
        assert_eq!(
            install_pending_docs(&moved, &key_c, rename),
            Pending::Docs(vec![(NOTICES, Outcome::Installed)])
        );
        assert_eq!(read(&moved, NOTICES), "notices of C");
        assert!(!moved.join(PREV_DIR).exists());
        assert!(!owned_batch_record(&moved).exists());
        // A later staging in the moved folder succeeds cleanly.
        let (stage_d, exe_d, key_d, _) = new_exe(b"D");
        let notices_d = stage_d.path().join(NOTICES);
        std::fs::write(&notices_d, "notices of D").unwrap();
        stage_pending_docs(&moved, &exe_d, &[(&notices_d, NOTICES)], "d").unwrap();
        assert_eq!(read(&moved.join(NEXT_DIR), KEY_FILE), key_d);
    }

    /// Rewriting the ownership record is atomic via a sibling temp file: a
    /// write failure keeps the previously recorded IDs intact so live batches
    /// stay owned, and a retry completes cleanly.
    #[test]
    fn a_failed_record_rewrite_keeps_previous_ids_owned_and_allows_retry() {
        let dir = tempfile::tempdir().unwrap();
        let (_stage, exe_b, _key_b, readme_b) = new_exe(b"B");
        let token_b = stage_pending_docs(dir.path(), &exe_b, &[(&readme_b, "README.txt")], "b")
            .unwrap()
            .unwrap();
        let record = owned_batch_record(dir.path());
        assert!(is_owned_batch(dir.path(), &dir.path().join(NEXT_DIR)));
        // Inject a write failure when updating the record.
        let err = write_batches_with(&record, &[token_b.clone(), "ff".repeat(16)], |tmp, _| {
            std::fs::write(tmp, "partial")?;
            Err(io::Error::other("disk full"))
        });
        assert!(err.is_err());
        // Previous ID remains in the live record and the batch stays owned.
        assert_eq!(read_ids_from(&record), vec![token_b.clone()]);
        assert!(is_owned_batch(dir.path(), &dir.path().join(NEXT_DIR)));
        // Retry succeeds cleanly.
        let next_id = "ff".repeat(16);
        write_batches_to(&record, &[token_b.clone(), next_id.clone()]).unwrap();
        assert_eq!(read_ids_from(&record), vec![token_b, next_id]);
    }

    /// Scenario: B's held batch moves to `PREV_DIR` when C stages, C stops
    /// before its swap (so B still runs), and D stages. `make_room` must not
    /// retire or re-key B's live `PREV_DIR` batch; B keeps its notices and
    /// installs them once released. Control: when C succeeds and replaces B,
    /// C's promotion retires B's `PREV_DIR` batch.
    #[test]
    fn staging_d_after_aborted_c_keeps_running_exes_set_aside_batch_intact() {
        const NOTICES: &str = "THIRD-PARTY-NOTICES.md";
        let notices_of = |stage: &tempfile::TempDir, who: &str| {
            let path = stage.path().join(NOTICES);
            std::fs::write(&path, format!("notices of {who}")).unwrap();
            path
        };
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(NOTICES), "installed").unwrap();
        let (stage_b, exe_b, key_b, _) = new_exe(b"B");
        let notices_b = notices_of(&stage_b, "B");
        stage_pending_docs(dir.path(), &exe_b, &[(&notices_b, NOTICES)], "b").unwrap();
        // C stages -> moves B's batch to PREV_DIR, then C stops before swap.
        let (stage_c, exe_c, key_c, _) = new_exe(b"C");
        let notices_c = notices_of(&stage_c, "C");
        stage_pending_docs(dir.path(), &exe_c, &[(&notices_c, NOTICES)], "c").unwrap();
        let prev = dir.path().join(PREV_DIR);
        assert_eq!(read(&prev, KEY_FILE), key_b);
        // D tries to stage while B is still running with its batch in PREV_DIR.
        let (stage_d, exe_d, _key_d, _) = new_exe(b"D");
        let notices_d = notices_of(&stage_d, "D");
        assert!(stage_pending_docs(dir.path(), &exe_d, &[(&notices_d, NOTICES)], "d").is_err());
        // B's batch in PREV_DIR is completely intact (neither removed nor re-keyed).
        assert_eq!(read(&prev, KEY_FILE), key_b);
        assert_eq!(read(&prev, NOTICES), "notices of B");
        // Once the hold clears, B installs its notices from PREV_DIR.
        assert_eq!(
            install_pending_docs(dir.path(), &key_b, rename),
            Pending::Docs(vec![(NOTICES, Outcome::Installed)])
        );
        assert_eq!(read(dir.path(), NOTICES), "notices of B");
        assert!(!prev.exists());
        // Control: when C does replace B, C's promotion retires B's PREV_DIR batch.
        let dir2 = tempfile::tempdir().unwrap();
        std::fs::write(dir2.path().join(NOTICES), "installed").unwrap();
        stage_pending_docs(dir2.path(), &exe_b, &[(&notices_b, NOTICES)], "b2").unwrap();
        stage_pending_docs(dir2.path(), &exe_c, &[(&notices_c, NOTICES)], "c2").unwrap();
        assert_eq!(read(&dir2.path().join(PREV_DIR), KEY_FILE), key_b);
        assert_eq!(
            install_pending_docs(dir2.path(), &key_c, rename),
            Pending::Docs(vec![(NOTICES, Outcome::Installed)])
        );
        assert_eq!(read(dir2.path(), NOTICES), "notices of C");
        assert!(!dir2.path().join(PREV_DIR).exists());
    }

    /// The next batch of the exe B that runs (its key is there) stays when a
    /// later update C fails: C's discard removes only a batch C staged.
    #[test]
    fn a_failed_later_update_keeps_the_next_batch_of_the_exe_that_runs() {
        let dir = folder(Some(KEY));
        let (_stage, exe_b, key_b, readme_b) = new_exe(b"B");
        stage_pending_docs(dir.path(), &exe_b, &[(&readme_b, "README.txt")], "b").unwrap();
        let (_stage_c, _exe_c, key_c, _readme_c) = new_exe(b"C");
        discard_pending_docs(dir.path(), &key_c);
        assert_eq!(read(&dir.path().join(NEXT_DIR), KEY_FILE), key_b);
        let Pending::Docs(outcomes) = install_pending_docs(dir.path(), &key_b, rename) else {
            panic!("B still installs its batch")
        };
        assert!(outcomes.contains(&("README.txt", Outcome::Installed)));
        assert_eq!(read(dir.path(), "README.txt"), "readme of B");
    }

    #[test]
    fn a_discarded_update_drops_its_next_batch_and_keeps_the_current_one() {
        let dir = folder(Some(KEY));
        let (_stage, exe_b, _key_b, readme_b) = new_exe(b"B");
        let token_b = stage_pending_docs(dir.path(), &exe_b, &[(&readme_b, "README.txt")], "d")
            .unwrap()
            .unwrap();
        discard_pending_docs(dir.path(), &token_b);
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
    fn an_old_exe_started_in_another_process_keeps_the_update_s_batch_and_installs_its_own() {
        // A stages B's batch for its update; a second A process starts before
        // the swap. It runs in a process of its own, so nothing of the
        // staging process can tell it whose batch that is.
        let dir = folder(Some(KEY));
        let (_stage, exe_b, key_b, readme_b) = new_exe(b"B");
        let token_b = stage_pending_docs(dir.path(), &exe_b, &[(&readme_b, "README.txt")], "f")
            .unwrap()
            .unwrap();
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "portable_docs::tests::child_install_pass",
                "--ignored",
                "--nocapture",
            ])
            .env(CHILD_DIR, dir.path())
            .env(CHILD_KEY, KEY)
            .output()
            .unwrap();
        let out = String::from_utf8_lossy(&child.stdout);
        assert!(child.status.success(), "{out}");
        assert!(out.contains("child pass: Docs("), "{out}");
        // A's documents are installed, B's batch waits for its swap, whole.
        assert_eq!(read(dir.path(), "README.txt"), "new README.txt");
        assert_eq!(read(&dir.path().join(NEXT_DIR), KEY_FILE), key_b);
        assert_eq!(
            read(&dir.path().join(NEXT_DIR), "README.txt"),
            "readme of B"
        );
        // The swap then fails: the helper's discard drops B's batch, and A
        // starts again with nothing left of it.
        discard_pending_docs(dir.path(), &token_b);
        assert!(!dir.path().join(NEXT_DIR).exists());
        assert_eq!(install_pending_docs(dir.path(), KEY, rename), Pending::None);
        // A batch of another exe a crash left is replaced by the next staging.
        stage_pending_docs(dir.path(), &exe_b, &[(&readme_b, "README.txt")], "g").unwrap();
        let (_stage_c, exe_c, key_c, readme_c) = new_exe(b"C");
        stage_pending_docs(dir.path(), &exe_c, &[(&readme_c, "README.txt")], "h").unwrap();
        assert_eq!(read(&dir.path().join(NEXT_DIR), KEY_FILE), key_c);
        assert_eq!(
            read(&dir.path().join(NEXT_DIR), "README.txt"),
            "readme of C"
        );
    }

    const CHILD_DIR: &str = "AEROFTP_TEST_PORTABLE_DOCS_DIR";
    const CHILD_KEY: &str = "AEROFTP_TEST_PORTABLE_DOCS_KEY";

    /// The second process of the test above: one install pass, nothing else.
    #[test]
    #[ignore = "run by an_old_exe_started_in_another_process_keeps_the_update_s_batch_and_installs_its_own"]
    fn child_install_pass() {
        let (Some(dir), Some(key)) = (std::env::var_os(CHILD_DIR), std::env::var(CHILD_KEY).ok())
        else {
            return;
        };
        let found = install_pending_docs(Path::new(&dir), &key, rename);
        println!("child pass: {found:?}");
    }

    #[test]
    fn the_about_dialog_gets_only_the_pending_notices_of_the_running_exe() {
        let notices = |dir: &Path, name: &str, key: &str, text: &str| {
            let batch = dir.join(name);
            std::fs::create_dir_all(&batch).unwrap();
            std::fs::write(batch.join("THIRD-PARTY-NOTICES.md"), text).unwrap();
            std::fs::write(batch.join(KEY_FILE), key).unwrap();
            let id = format!("{:032x}", rand::random::<u128>());
            std::fs::write(batch.join(STAGING_ID_FILE), &id).unwrap();
            record_batch(dir, &id).unwrap();
        };
        let own = || Some(KEY.to_string());
        // No batch: the key is not even read.
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            own_pending_notices(dir.path(), || panic!("no batch, no hash")),
            None
        );
        // The current batch is the running exe's.
        notices(dir.path(), PENDING_DIR, KEY, "own");
        let offered = own_pending_notices(dir.path(), own).expect("own batch");
        assert_eq!(std::fs::read_to_string(&offered).unwrap(), "own");
        // A staging of another build replaces the batch after the lookup:
        // the path About got still holds the notices it checked.
        notices(dir.path(), PENDING_DIR, "build-c", "build C");
        assert_eq!(std::fs::read_to_string(&offered).unwrap(), "own");
        // The same from the next batch: B's notices are looked up there,
        // then a staging of build C replaces the next batch; the path About
        // got still holds B's.
        let dir = tempfile::tempdir().unwrap();
        notices(dir.path(), PENDING_DIR, "older-exe", "older");
        notices(dir.path(), NEXT_DIR, KEY, "notices of B");
        let offered_b = own_pending_notices(dir.path(), own).expect("B's next batch");
        let (_stage, exe_c, key_c, readme_c) = new_exe(b"C");
        let notices_c = readme_c.with_file_name("THIRD-PARTY-NOTICES.md");
        std::fs::write(&notices_c, "notices of C").unwrap();
        stage_pending_docs(
            dir.path(),
            &exe_c,
            &[(&notices_c, "THIRD-PARTY-NOTICES.md")],
            "c",
        )
        .unwrap();
        assert_eq!(read(&dir.path().join(NEXT_DIR), KEY_FILE), key_c);
        assert_eq!(
            read(&dir.path().join(NEXT_DIR), "THIRD-PARTY-NOTICES.md"),
            "notices of C"
        );
        assert_eq!(std::fs::read_to_string(&offered_b).unwrap(), "notices of B");
        // An editor held an older exe's current batch, so the promotion
        // failed and the pass installs from the next batch: the older exe's
        // notices are never offered, the next batch's are.
        let dir = tempfile::tempdir().unwrap();
        notices(dir.path(), PENDING_DIR, "older-exe", "older");
        notices(dir.path(), NEXT_DIR, KEY, "own");
        let offered = own_pending_notices(dir.path(), own).expect("next batch");
        assert_eq!(std::fs::read_to_string(offered).unwrap(), "own");
        // Once its notices are installed, nothing pending is offered, though
        // the older batch is still there.
        std::fs::remove_dir_all(dir.path().join(NEXT_DIR)).unwrap();
        assert_eq!(own_pending_notices(dir.path(), own), None);
        // A next batch staged for an update, not yet swapped, is the new
        // exe's: the running one never offers it.
        let dir = tempfile::tempdir().unwrap();
        notices(dir.path(), NEXT_DIR, "new-exe", "new");
        assert_eq!(own_pending_notices(dir.path(), own), None);
        // A batch without a key is nobody's.
        let dir = tempfile::tempdir().unwrap();
        notices(dir.path(), PENDING_DIR, KEY, "own");
        std::fs::remove_file(dir.path().join(PENDING_DIR).join(KEY_FILE)).unwrap();
        assert_eq!(own_pending_notices(dir.path(), own), None);
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

    /// The record outlives its folder (a staging stopped after its publishing
    /// rename, before it dropped the entry), and a folder is created later at
    /// that path, with a key file and a README as a staging writes: it is not
    /// that staging's folder (it lacks the identity the record keeps), so it
    /// stays whole and the entry goes. A folder the staging left empty (it
    /// stopped before writing its identity) is removed.
    #[test]
    fn a_folder_created_later_at_a_recorded_staging_path_stays_whole() {
        let dir = folder(Some(KEY));
        let name = format!("{TMP_PREFIX}gone");
        let path = dir.path().join(&name);
        std::fs::create_dir(&path).unwrap();
        begin_staging(dir.path(), &name).unwrap();
        // Published: the folder is renamed away, the entry stays.
        std::fs::rename(&path, dir.path().join("published")).unwrap();
        assert_eq!(recorded_stagings(dir.path()).len(), 1);
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join(KEY_FILE), "my key").unwrap();
        std::fs::write(path.join("README.txt"), "my readme").unwrap();
        // Another recorded staging that stopped before its identity.
        let empty_name = format!("{TMP_PREFIX}empty");
        let empty = dir.path().join(&empty_name);
        std::fs::create_dir(&empty).unwrap();
        begin_staging(dir.path(), &empty_name).unwrap();
        std::fs::remove_file(empty.join(STAGING_ID_FILE)).unwrap();
        let Pending::Docs(_) = install_pending_docs(dir.path(), KEY, rename) else {
            panic!("docs")
        };
        assert_eq!(read(&path, KEY_FILE), "my key");
        assert_eq!(read(&path, "README.txt"), "my readme");
        assert!(!empty.exists(), "an empty recorded folder goes");
        assert!(recorded_stagings(dir.path()).is_empty(), "and the entries");
    }

    #[test]
    fn a_staging_folder_a_stopped_update_left_is_removed_and_never_read() {
        let dir = folder(Some(KEY));
        // What a staging stopped after its copy left: the folder it created
        // and recorded, with a partial document and its key.
        let tmp = dir.path().join(format!("{TMP_PREFIX}c"));
        std::fs::create_dir(&tmp).unwrap();
        begin_staging(dir.path(), &format!("{TMP_PREFIX}c")).unwrap();
        std::fs::write(tmp.join("README.txt"), b"partial").unwrap();
        std::fs::write(tmp.join(KEY_FILE), KEY).unwrap();
        let Pending::Docs(_) = install_pending_docs(dir.path(), KEY, rename) else {
            panic!("docs")
        };
        assert_eq!(read(dir.path(), "README.txt"), "new README.txt");
        assert!(!tmp.exists());
        assert!(recorded_stagings(dir.path()).is_empty());
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
        // The running exe's key is the same hash, read once.
        let running = exe_key(&std::env::current_exe().unwrap()).unwrap();
        assert_eq!(running_exe_key().as_deref(), Some(running.as_str()));
        assert_eq!(running_exe_key().as_deref(), Some(running.as_str()));
    }
}
