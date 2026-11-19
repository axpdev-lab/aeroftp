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
//! place, so it is whole or absent, and never touches the current batch. Each
//! staging returns a fresh per-attempt token (`staging.id`), not `exe.key`,
//! so a failed swap or discarded update removes only the batch of that exact
//! attempt. At its start an app promotes an owned next batch with its own key
//! to the current batch (retiring any older current or set-aside batch), then
//! renames each document over the installed one, an operation on that exact
//! path that Windows refuses while another process holds the file and that
//! never writes into a directory with the document's name. A document still
//! held stays in the current batch: the held ones share one retry budget, and
//! what is left is installed at a later start. A start with a different
//! executable key preserves a live current batch unless a matching next batch
//! supersedes it. A next batch of another exe is never removed at start, by
//! any process: an update another process hands over may be waiting for its
//! swap. A failed swap drops it by attempt token (the helper's discard), a
//! discarded update drops it by attempt token, and the next staging sets it
//! aside. Staging, discarding and each install pass hold one lock per exe
//! folder, so none of them sees another half done. Every outcome is logged as
//! what it is.

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
    /// A directory has the document's name: nothing was written into it.
    /// The pending copy is dropped, except for the mode-defining marker.
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

/// Strips the Windows extended-length prefix (`\\?\` or `\\?\UNC\`) returned by
/// `std::fs::canonicalize` so a canonical real folder path produces the exact
/// same hash and mutex name as a legacy non-canonical path (and as a junction
/// targeting that folder).
#[cfg(any(windows, test))]
fn plain_windows_path(path: &str) -> String {
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else {
        path.strip_prefix(r"\\?\").unwrap_or(path).to_string()
    }
}

/// Canonical per-folder identity shared by the batch lock, recovery mirrors,
/// staging records, and About notice snapshots.
/// Canonicalizes `exe_dir` first (stripping `\\?\` on Windows so legacy and
/// canonical paths hash identically) so a directory junction or symlink shares
/// one identity with the real folder, while case-folding only on Windows so
/// case-sensitive Unix volumes keep distinct folders separate.
pub(crate) fn folder_id(exe_dir: &Path) -> String {
    use sha2::{Digest, Sha256};
    let resolved = std::fs::canonicalize(exe_dir).unwrap_or_else(|_| exe_dir.to_path_buf());
    #[cfg(windows)]
    let key = plain_windows_path(&resolved.to_string_lossy()).to_lowercase();
    #[cfg(not(windows))]
    let key = resolved.to_string_lossy();
    hex::encode(Sha256::digest(key.as_bytes()))
}

/// Renames `from` to `to` atomically without replacing an existing entry at `to`
/// (even an empty directory). Relies directly on the OS no-replace primitive
/// (`MoveFileExW(..., 0)`, `renameat2(..., RENAME_NOREPLACE)`, `renamex_np(..., RENAME_EXCL)`)
/// and never falls back to overwrite-capable `std::fs::rename`: if atomic
/// no-replace rename is unavailable, returns an error so `install_pass_with`
/// installs directly from `NEXT_DIR`.
fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        use windows::core::HSTRING;
        use windows::Win32::Storage::FileSystem::{MoveFileExW, MOVE_FILE_FLAGS};
        let from_w = HSTRING::from(from.as_os_str());
        let to_w = HSTRING::from(to.as_os_str());
        // SAFETY: both HSTRINGs are valid null-terminated UTF-16 paths; flags=0
        // refuses to overwrite an existing file or directory at `to`.
        unsafe { MoveFileExW(&from_w, &to_w, MOVE_FILE_FLAGS(0)) }.map_err(io::Error::other)
    }
    #[cfg(target_os = "linux")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let c_from = CString::new(from.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
        let c_to = CString::new(to.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
        // SAFETY: both pointers are valid NUL-terminated C strings.
        let rc = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                c_from.as_ptr(),
                libc::AT_FDCWD,
                c_to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if rc == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    #[cfg(target_os = "macos")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let c_from = CString::new(from.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
        let c_to = CString::new(to.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
        // SAFETY: both pointers are valid NUL-terminated C strings.
        let rc = unsafe { libc::renamex_np(c_from.as_ptr(), c_to.as_ptr(), libc::RENAME_EXCL) };
        if rc == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    #[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
    {
        let _ = (from, to);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "atomic no-replace rename is unavailable on this platform",
        ))
    }
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
#[cfg(test)]
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
    /// A folder whose ownership identity (`staging.id` or ownership ledger)
    /// exists as a regular file but cannot be read now (transient I/O error).
    OwnershipUnreadable,
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
    std::env::temp_dir().join(format!(
        "aeroftp-pending-docs-{}.staging",
        folder_id(exe_dir)
    ))
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
    std::env::temp_dir().join(format!(
        "aeroftp-pending-docs-{}.batches",
        folder_id(exe_dir)
    ))
}

const UPDATE_HANDOFF_PREFIX: &str = ".aeroftp-update-handoff-";

#[cfg(windows)]
#[derive(Debug)]
pub(crate) struct UpdateHandoffReservation {
    path: PathBuf,
    exe_dir: PathBuf,
    persist: bool,
}

#[cfg(windows)]
impl UpdateHandoffReservation {
    pub(crate) fn transfer_to(&self, pid: u32) -> io::Result<()> {
        self.transfer_to_within(pid, Some(Duration::from_secs(10)))
    }

    fn transfer_to_within(&self, pid: u32, wait: Option<Duration>) -> io::Result<()> {
        // Staging reads the reservation while holding this same mutex. The
        // transfer therefore cannot interleave with its liveness decision.
        let _lock = lock_batches_within(&self.exe_dir, wait)?;
        let tmp = self.path.with_file_name(format!(
            ".aeroftp-handoff-owner.tmp-{:016x}",
            rand::random::<u64>()
        ));
        let result = std::fs::write(&tmp, pid.to_string())
            .and_then(|()| replace_record_file(&tmp, &self.path));
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        result
    }

    pub(crate) fn persist(&mut self) {
        self.persist = true;
    }
}

#[cfg(windows)]
impl Drop for UpdateHandoffReservation {
    fn drop(&mut self) {
        if !self.persist {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(windows)]
pub(crate) fn reserve_update_handoff(
    exe_dir: &Path,
    attempt_id: &str,
) -> io::Result<UpdateHandoffReservation> {
    if attempt_id.is_empty() || !attempt_id.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid portable update attempt id",
        ));
    }
    let path = exe_dir.join(format!("{UPDATE_HANDOFF_PREFIX}{attempt_id}"));
    std::fs::write(&path, std::process::id().to_string())?;
    Ok(UpdateHandoffReservation {
        path,
        exe_dir: exe_dir.to_path_buf(),
        persist: false,
    })
}

#[cfg(windows)]
fn process_is_alive(pid: u32) -> bool {
    use windows::Win32::Foundation::{CloseHandle, BOOL, WAIT_TIMEOUT};
    use windows::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    };
    // SAFETY: the handle, when returned, is owned here and closed before return.
    let handle = match unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            BOOL::from(false),
            pid,
        )
    } {
        Ok(handle) => handle,
        Err(e) => {
            let code = e.code().0 as u32;
            let raw = if code & 0xFFFF_0000 == 0x8007_0000 {
                code & 0xFFFF
            } else {
                code
            };
            if raw == 87 {
                return false;
            }
            return true;
        }
    };
    // SAFETY: a zero wait only queries whether the process handle is signaled.
    let alive = unsafe { WaitForSingleObject(handle, 0) } == WAIT_TIMEOUT;
    // SAFETY: this function owns the handle.
    let _ = unsafe { CloseHandle(handle) };
    alive
}

#[cfg(not(windows))]
fn process_is_alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    // SAFETY: signal zero only checks process existence and permissions.
    unsafe {
        libc::kill(pid, 0) == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

const CANCEL_MARKER_PREFIX: &str = ".aeroftp-pending-docs-cancel-";

fn cancel_marker_path(exe_dir: &Path, token: &str) -> PathBuf {
    exe_dir.join(format!("{CANCEL_MARKER_PREFIX}{token}"))
}

/// Persists an installation-scoped, token-keyed cancellation marker inside
/// `exe_dir` (independent of `TMP`/`%TEMP%`) before
/// `discard_pending_docs_within` waits for the batch lock.
/// Survives process exit and different temp directories so the next locked pass
/// discards the failed attempt's batch and restores any set-aside batch.
fn record_cancelled_batch(exe_dir: &Path, token: &str) -> io::Result<()> {
    if is_valid_batch_id(token) {
        std::fs::write(cancel_marker_path(exe_dir, token), b"")?;
    }
    Ok(())
}

#[derive(Default)]
struct CancellationState {
    unfinished: bool,
    identity_unresolved: bool,
}

/// Applies persisted token-scoped cancellation markers in `exe_dir` while the
/// caller holds the batch lock. An unfinished confirmed cancellation masks only
/// `NEXT_DIR`; an unreadable identity also keeps portable detection `Pending`
/// unless another valid marker proves portable mode.
fn apply_cancelled_batches_with(
    exe_dir: &Path,
    read_id: &impl Fn(&Path) -> io::Result<String>,
) -> CancellationState {
    let Ok(entries) = std::fs::read_dir(exe_dir) else {
        return CancellationState::default();
    };
    let next = exe_dir.join(NEXT_DIR);
    let prev = exe_dir.join(PREV_DIR);
    let mut state = CancellationState::default();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        let Some(token) = name_str.strip_prefix(CANCEL_MARKER_PREFIX) else {
            continue;
        };
        if !is_valid_batch_id(token) {
            continue;
        }
        let marker_path = entry.path();
        if std::fs::symlink_metadata(&next).is_err() {
            let _ = std::fs::remove_file(&marker_path);
            continue;
        }
        match batch_ownership_with(exe_dir, &next, read_id) {
            BatchOwnership::Unreadable => {
                // The token match is unresolved: preserve cancellation and do
                // not let detection conclude installed mode from this slot.
                state.unfinished = true;
                state.identity_unresolved = true;
                continue;
            }
            BatchOwnership::Unowned => {
                let _ = std::fs::remove_file(&marker_path);
                continue;
            }
            BatchOwnership::Owned => {}
        }
        let id_path = next.join(STAGING_ID_FILE);
        let current_token = match read_id(&id_path) {
            Ok(raw) => raw.trim().to_string(),
            Err(_) => {
                state.unfinished = true;
                state.identity_unresolved = true;
                continue;
            }
        };
        if current_token != token {
            let _ = std::fs::remove_file(&marker_path);
            continue;
        }
        remove_batch(&next);
        if std::fs::symlink_metadata(&next).is_err() {
            let _ = forget_batch(exe_dir, token);
            if is_owned_batch(exe_dir, &prev)
                && matches!(slot(&prev), Slot::Key(k) if k != RETIRED_KEY)
            {
                let _ = std::fs::rename(&prev, &next);
            }
            let _ = std::fs::remove_file(&marker_path);
        } else {
            // Partial cleanup (for example a document is held): keep the marker
            // active so no pass installs or exposes the cancelled batch.
            state.unfinished = true;
        }
    }
    state
}

fn apply_cancelled_batches(exe_dir: &Path) -> CancellationState {
    apply_cancelled_batches_with(exe_dir, &|p| std::fs::read_to_string(p))
}

fn is_valid_batch_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Strictly parses an ownership ledger file: returns `Ok(None)` when absent,
/// `Ok(Some(ids))` when it is a regular UTF-8 file containing only valid
/// 32-char hex batch IDs (one per non-empty line), and `Err(InvalidData)` if
/// the path is a symlink/directory, non-UTF-8 binary, or holds any
/// unrecognized content so staging never overwrites a user file.
fn read_ledger_strict(path: &Path) -> io::Result<Option<Vec<String>>> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    if !meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} is not a regular ownership ledger file", path.display()),
        ));
    }
    let bytes = std::fs::read(path)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} is not a valid ownership ledger", path.display()),
        )
    })?;
    let mut ids = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !is_valid_batch_id(trimmed) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} contains an unrecognized entry", path.display()),
            ));
        }
        if !ids.iter().any(|existing| existing == trimmed) {
            ids.push(trimmed.to_string());
        }
    }
    Ok(Some(ids))
}

fn read_ids_from(path: &Path) -> Vec<String> {
    read_ledger_strict(path).ok().flatten().unwrap_or_default()
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
    let primary_path = owned_batch_record(exe_dir);
    let mut ids = read_ledger_strict(&primary_path)?.unwrap_or_default();
    for temp_id in read_ids_from(&temp_batch_record(exe_dir)) {
        if !ids.iter().any(|existing| existing == &temp_id) {
            ids.push(temp_id);
        }
    }
    if !ids.iter().any(|existing| existing == id) {
        ids.push(id.to_string());
    }
    write_batches_to(&primary_path, &ids)?;
    let _ = write_batches_to(&temp_batch_record(exe_dir), &ids);
    Ok(())
}

fn forget_batch(exe_dir: &Path, id: &str) -> io::Result<()> {
    let primary_path = owned_batch_record(exe_dir);
    let primary = match read_ledger_strict(&primary_path)? {
        Some(existing) => {
            let ids: Vec<String> = existing.into_iter().filter(|e| e != id).collect();
            write_batches_to(&primary_path, &ids)
        }
        None => Ok(()),
    };
    let temp_path = temp_batch_record(exe_dir);
    if let Ok(Some(existing)) = read_ledger_strict(&temp_path) {
        let ids: Vec<String> = existing.into_iter().filter(|e| e != id).collect();
        let _ = write_batches_to(&temp_path, &ids);
    }
    primary
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BatchOwnership {
    Owned,
    Unowned,
    Unreadable,
}

fn batch_id(path: &Path) -> Option<String> {
    std::fs::symlink_metadata(path.join(STAGING_ID_FILE))
        .ok()
        .filter(|m| m.is_file())?;
    let id = std::fs::read_to_string(path.join(STAGING_ID_FILE)).ok()?;
    let id = id.trim();
    (!id.is_empty()).then(|| id.to_string())
}

/// Checks whether `path` is a batch folder staged by AeroFTP for `exe_dir`,
/// rejecting any non-regular `staging.id` entry (symlink, directory, FIFO) before
/// opening it, while distinguishing a transient read error on a regular
/// `staging.id` or the ownership ledger (`Unreadable`) from a missing or
/// unregistered batch (`Unowned`).
fn batch_ownership(exe_dir: &Path, path: &Path) -> BatchOwnership {
    batch_ownership_with(exe_dir, path, |p| std::fs::read_to_string(p))
}

fn batch_ownership_with(
    exe_dir: &Path,
    path: &Path,
    read_id: impl Fn(&Path) -> io::Result<String>,
) -> BatchOwnership {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return BatchOwnership::Unowned,
        Err(_) => return BatchOwnership::Unreadable,
        Ok(m) if !m.is_dir() => return BatchOwnership::Unowned,
        Ok(_) => {}
    }
    match std::fs::symlink_metadata(path.join(KEY_FILE)) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return BatchOwnership::Unowned,
        Err(_) => return BatchOwnership::Unreadable,
        Ok(m) if m.file_type().is_symlink() => return BatchOwnership::Unowned,
        Ok(_) => {}
    }
    let id_path = path.join(STAGING_ID_FILE);
    match std::fs::symlink_metadata(&id_path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return BatchOwnership::Unowned,
        Err(_) => return BatchOwnership::Unreadable,
        Ok(m) if !m.is_file() => return BatchOwnership::Unowned,
        Ok(_) => {}
    }
    let id = match read_id(&id_path) {
        Ok(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return BatchOwnership::Unowned;
            }
            trimmed.to_string()
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => return BatchOwnership::Unowned,
        Err(_) => return BatchOwnership::Unreadable,
    };
    let mut ledger_unreadable = false;
    for record in [owned_batch_record(exe_dir), temp_batch_record(exe_dir)] {
        match read_ledger_strict(&record) {
            Ok(Some(ids)) if ids.iter().any(|r| r == &id) => return BatchOwnership::Owned,
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::InvalidData => {}
            Err(_) => ledger_unreadable = true,
        }
    }
    if ledger_unreadable {
        BatchOwnership::Unreadable
    } else {
        BatchOwnership::Unowned
    }
}

/// Whether `path` is a batch folder that AeroFTP itself staged for `exe_dir`:
/// a real folder carrying `KEY_FILE` and a `STAGING_ID_FILE` recorded in the
/// durable ownership record (`owned_batch_record` or `temp_batch_record`).
fn is_owned_batch(exe_dir: &Path, path: &Path) -> bool {
    matches!(batch_ownership(exe_dir, path), BatchOwnership::Owned)
}

/// Outcome of checking whether `exe_dir` is a portable install, synchronized
/// with `install_pending_docs` and `stage_pending_docs` via the batch mutex.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PortableMarkerState {
    /// `portable.marker` is present beside the exe, or staged in an owned
    /// pending batch belonging to this exe: resolve portable mode immediately.
    Present,
    /// Neither `portable.marker` nor any owned staged marker for this exe
    /// exists under the batch lock: definitively not portable.
    Absent,
    /// Could not acquire the batch lock within `wait` or transiently could not
    /// read a marker/batch/exe key: leave portable detection uncached.
    Pending,
}

/// Atomically checks `exe_dir/portable.marker` and any owned pending batch for
/// `own_key` under the batch mutex, keeping a fast path when `portable.marker`
/// is already present beside the exe, and confirming `Absent` only while
/// holding the batch mutex.
pub(crate) fn probe_portable_marker_within(
    exe_dir: &Path,
    own_key: impl FnOnce() -> Option<String>,
    wait: Option<Duration>,
) -> PortableMarkerState {
    probe_portable_marker_full(
        exe_dir,
        own_key,
        wait,
        |p| std::fs::metadata(p),
        |p| std::fs::read_to_string(p),
    )
}

#[cfg(test)]
pub(crate) fn probe_portable_marker_with_read_id(
    exe_dir: &Path,
    own_key: impl FnOnce() -> Option<String>,
    wait: Option<Duration>,
    read_id: impl Fn(&Path) -> io::Result<String>,
) -> PortableMarkerState {
    probe_portable_marker_full(
        exe_dir,
        own_key,
        wait,
        |p| std::fs::symlink_metadata(p),
        read_id,
    )
}

#[cfg(test)]
fn probe_portable_marker_with(
    exe_dir: &Path,
    own_key: impl FnOnce() -> Option<String>,
    wait: Option<Duration>,
    metadata_fn: impl Fn(&Path) -> io::Result<std::fs::Metadata>,
) -> PortableMarkerState {
    probe_portable_marker_full(exe_dir, own_key, wait, metadata_fn, |p| {
        std::fs::read_to_string(p)
    })
}

fn probe_portable_marker_full(
    exe_dir: &Path,
    own_key: impl FnOnce() -> Option<String>,
    wait: Option<Duration>,
    metadata_fn: impl Fn(&Path) -> io::Result<std::fs::Metadata>,
    read_id: impl Fn(&Path) -> io::Result<String>,
) -> PortableMarkerState {
    // Fast path when `portable.marker` is already installed beside the exe.
    if metadata_fn(&exe_dir.join("portable.marker")).is_ok_and(|m| m.is_file()) {
        return PortableMarkerState::Present;
    }
    // Always confirm absence under the batch lock so an install that moves
    // `portable.marker` and removes the empty batch cannot race between checks.
    let Ok(_lock) = lock_batches_within(exe_dir, wait) else {
        return PortableMarkerState::Pending;
    };
    let cancellations = apply_cancelled_batches_with(exe_dir, &read_id);
    let cancelled_next = cancellations.unfinished;
    let mut any_unreadable = cancellations.identity_unresolved;
    match metadata_fn(&exe_dir.join("portable.marker")) {
        Ok(m) if m.is_file() => return PortableMarkerState::Present,
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(_) => any_unreadable = true,
    }
    let mut staged_candidates: Vec<String> = Vec::new();
    for name in [NEXT_DIR, PENDING_DIR, PREV_DIR] {
        if cancelled_next && name == NEXT_DIR {
            continue;
        }
        let dir = exe_dir.join(name);
        match metadata_fn(&dir) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(_) => {
                any_unreadable = true;
                continue;
            }
            Ok(m) if !m.is_dir() => continue,
            Ok(_) => {}
        }
        match metadata_fn(&dir.join("portable.marker")) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(_) => {
                any_unreadable = true;
                continue;
            }
            Ok(m) if !m.is_file() => continue,
            Ok(_) => {}
        }
        match batch_ownership_with(exe_dir, &dir, &read_id) {
            BatchOwnership::Owned => match slot(&dir) {
                Slot::Key(k) => staged_candidates.push(k),
                Slot::Unreadable | Slot::OwnershipUnreadable => any_unreadable = true,
                _ => {}
            },
            BatchOwnership::Unreadable => any_unreadable = true,
            BatchOwnership::Unowned => {}
        }
    }
    if !staged_candidates.is_empty() {
        let Some(own) = own_key() else {
            return PortableMarkerState::Pending;
        };
        if staged_candidates.iter().any(|k| k == &own) {
            return PortableMarkerState::Present;
        }
    }
    if any_unreadable {
        PortableMarkerState::Pending
    } else {
        PortableMarkerState::Absent
    }
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
        // Remove `KEY_FILE` before `STAGING_ID_FILE`: if another process holds
        // `exe.key` open without delete sharing on Windows, keeping
        // `staging.id` leaves the batch AeroFTP-owned and retryable once the
        // handle closes.
        let _ = std::fs::remove_file(path.join(KEY_FILE));
        if std::fs::symlink_metadata(path.join(KEY_FILE)).is_err() {
            let _ = std::fs::remove_file(path.join(STAGING_ID_FILE));
            let _ = std::fs::remove_dir(path);
        }
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

/// How long update preparation waits for the batch lock before failing so a
/// stalled holder cannot hang the updater indefinitely.
const STAGE_LOCK_WAIT: Duration = Duration::from_secs(10);

/// Stages `docs` (source, document name) as the next batch of `exe_dir`, for
/// the exe at `new_exe`. The current batch is never touched; a next batch of
/// another exe is set aside to `PREV_DIR` (or reclaimed when provably
/// abandoned). Anything else at the next batch's name (an unowned folder, a
/// file) is not ours: staging fails and leaves it as it is, so the update
/// stops before its swap. Returns a fresh per-attempt token (`staging.id`,
/// never `exe.key`, or `None` with no documents) that a failed swap or
/// discarded update passes to `discard_pending_docs`.
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

#[cfg(all(windows, test))]
pub(crate) fn stage_pending_docs_within(
    exe_dir: &Path,
    new_exe: &Path,
    docs: &[(&Path, &'static str)],
    suffix: &str,
    wait: Option<Duration>,
) -> Result<Option<String>, String> {
    stage_with_wait(exe_dir, new_exe, docs, suffix, wait, |from, to| {
        std::fs::rename(from, to)
    })
}

/// Keys of all installed app executables in `exe_dir` (`AeroFTP.exe`,
/// `aeroftp-app.exe`, and `std::env::current_exe()` when it resides in
/// `exe_dir`) and sidecars with a live handoff reservation: a batch matching
/// any of these belongs to a running executable or a helper waiting to swap.
/// Sidecars left by a dead attempt do not block later updates.
///
/// Returns `None` when an executable that exists in `exe_dir` fails to hash
/// (a transient read or sharing error): an incomplete inventory must never
/// reclaim a batch by key mismatch. Missing executables (`NotFound`) are
/// simply absent and keep the inventory complete.
fn installed_app_keys(exe_dir: &Path) -> Option<Vec<String>> {
    installed_app_keys_with_liveness(exe_dir, exe_key, process_is_alive)
}

fn installed_app_keys_with(
    exe_dir: &Path,
    hash_exe: impl Fn(&Path) -> io::Result<String>,
) -> Option<Vec<String>> {
    installed_app_keys_with_liveness(exe_dir, hash_exe, process_is_alive)
}

fn add_active_handoff_keys(
    exe_dir: &Path,
    hash_exe: &impl Fn(&Path) -> io::Result<String>,
    process_alive: &impl Fn(u32) -> bool,
    keys: &mut Vec<String>,
) -> io::Result<()> {
    for entry in std::fs::read_dir(exe_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(attempt) = name.strip_prefix(UPDATE_HANDOFF_PREFIX) else {
            continue;
        };
        if attempt.is_empty() || !attempt.bytes().all(|b| b.is_ascii_alphanumeric()) {
            continue;
        }
        let pid = match std::fs::read_to_string(entry.path()) {
            Ok(raw) => match raw.trim().parse::<u32>() {
                Ok(pid) => pid,
                Err(_) => continue,
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        if !process_alive(pid) {
            let _ = std::fs::remove_file(entry.path());
            continue;
        }
        for app in ["AeroFTP.exe", "aeroftp-app.exe"] {
            let sidecar = exe_dir.join(format!("{app}.{attempt}.new"));
            match hash_exe(&sidecar) {
                Ok(k) => {
                    if !keys.contains(&k) {
                        keys.push(k);
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            }
        }
    }
    Ok(())
}

fn installed_app_keys_with_liveness(
    exe_dir: &Path,
    hash_exe: impl Fn(&Path) -> io::Result<String>,
    process_alive: impl Fn(u32) -> bool,
) -> Option<Vec<String>> {
    let mut keys = Vec::new();
    for name in ["AeroFTP.exe", "aeroftp-app.exe"] {
        let path = exe_dir.join(name);
        match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(_) => return None,
            Ok(_) => {}
        }
        match hash_exe(&path) {
            Ok(k) => {
                if !keys.contains(&k) {
                    keys.push(k);
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(_) => return None,
        }
    }
    if add_active_handoff_keys(exe_dir, &hash_exe, &process_alive, &mut keys).is_err() {
        return None;
    }
    if let Ok(cur) = std::env::current_exe() {
        let same_dir = cur.parent().is_some_and(|p| {
            p == exe_dir
                || std::fs::canonicalize(p)
                    .ok()
                    .zip(std::fs::canonicalize(exe_dir).ok())
                    .is_some_and(|(a, b)| a == b)
        });
        if same_dir {
            match hash_exe(&cur) {
                Ok(k) => {
                    if !keys.contains(&k) {
                        keys.push(k);
                    }
                }
                Err(_) => return None,
            }
        }
    }
    Some(keys)
}

fn stage_with(
    exe_dir: &Path,
    new_exe: &Path,
    docs: &[(&Path, &'static str)],
    suffix: &str,
    publish: impl Fn(&Path, &Path) -> io::Result<()>,
) -> Result<Option<String>, String> {
    stage_with_wait(
        exe_dir,
        new_exe,
        docs,
        suffix,
        Some(STAGE_LOCK_WAIT),
        publish,
    )
}

fn stage_with_wait(
    exe_dir: &Path,
    new_exe: &Path,
    docs: &[(&Path, &'static str)],
    suffix: &str,
    wait: Option<Duration>,
    publish: impl Fn(&Path, &Path) -> io::Result<()>,
) -> Result<Option<String>, String> {
    stage_with_commit(exe_dir, new_exe, docs, suffix, wait, record_batch, publish)
}

#[allow(clippy::too_many_arguments)]
fn stage_with_commit(
    exe_dir: &Path,
    new_exe: &Path,
    docs: &[(&Path, &'static str)],
    suffix: &str,
    wait: Option<Duration>,
    record: impl Fn(&Path, &str) -> io::Result<()>,
    publish: impl Fn(&Path, &Path) -> io::Result<()>,
) -> Result<Option<String>, String> {
    if docs.is_empty() {
        return Ok(None);
    }
    let fail = |e: io::Error| format!("Failed to stage the portable documents: {e}");
    let key = exe_key(new_exe).map_err(fail)?;
    let _lock = lock_batches_within(exe_dir, wait).map_err(fail)?;
    if apply_cancelled_batches(exe_dir).unfinished {
        return Err(
            "Failed to stage the portable documents: a cancelled previous batch could not finish cleanup yet"
                .to_string(),
        );
    }
    remove_staging_leftovers(exe_dir);
    let next = exe_dir.join(NEXT_DIR);
    let prev = exe_dir.join(PREV_DIR);
    if is_owned_batch(exe_dir, &next)
        && matches!(slot(&next), Slot::Key(k) if k == key)
        && DOCS.iter().all(|name| {
            let staged_doc = next.join(name);
            match docs.iter().find(|(_, doc_name)| doc_name == name) {
                Some((source, _)) => {
                    staged_doc.is_file()
                        && std::fs::read(source)
                            .ok()
                            .zip(std::fs::read(&staged_doc).ok())
                            .is_some_and(|(src, dst)| src == dst)
                }
                None => std::fs::symlink_metadata(&staged_doc).is_err(),
            }
        })
    {
        // Already staged by AeroFTP for this exe, with every requested
        // document matching byte for byte. Transfer ownership under the lock
        // to a fresh per-attempt token so cleanup from an earlier failed
        // attempt of the same exe cannot delete the batch this retry relies on.
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
    // Validate an existing ownership ledger before creating any staging files
    // so an unrecognized user file at `.aeroftp-pending-docs.owned` stops
    // staging untouched.
    read_ledger_strict(&owned_batch_record(exe_dir)).map_err(fail)?;
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
    let installed_keys = installed_app_keys(exe_dir);
    let staged = (|| -> io::Result<()> {
        std::fs::write(tmp.join(KEY_FILE), &key)?;
        for (source, name) in docs {
            std::fs::copy(source, tmp.join(name))?;
        }
        let room = make_room(exe_dir, &next, &prev, &key, installed_keys.as_deref())?;
        let mut recorded = false;
        let committed = (|| -> io::Result<()> {
            record(exe_dir, &staging_id)?;
            recorded = true;
            publish(&tmp, &next)
        })();
        if committed.is_err() {
            if recorded {
                let _ = forget_batch(exe_dir, &staging_id);
            }
            if matches!(
                room,
                RoomOutcome::SetAsideOther | RoomOutcome::SetAsideSameKey
            ) && std::fs::symlink_metadata(&next).is_err()
            {
                // Nothing replaced the batch set aside: restore it to NEXT_DIR.
                let _ = std::fs::rename(&prev, &next);
            }
        }
        committed
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RoomOutcome {
    Cleared,
    SetAsideOther,
    SetAsideSameKey,
}

/// Makes room at `next` for the batch of the exe `key`. When `next`'s key is
/// temporarily unreadable (`Slot::Unreadable`), returns an error immediately
/// without modifying `next` or `prev`. If `installed_keys` is `Some` and
/// non-empty (a complete inventory of installed `AeroFTP.exe` /
/// `aeroftp-app.exe` / running exe in `exe_dir`) and `next` does not match any
/// of them, `next` belongs to an abandoned update that never swapped and is
/// removed directly without disturbing `prev`. Otherwise sets `next` aside at
/// `prev` (including when `next` has the same key as `key`, so a running build's
/// live batch stays recoverable if replacement publication fails), reclaiming
/// `prev` only when it is already `RETIRED_KEY` or provably an abandoned update
/// (complete inventory with key mismatch). When `prev` already protects the
/// original same-exe batch, a repeated restage replaces only the intermediate
/// `next`. Other occupied `prev` states or a failed rename return an error.
fn make_room(
    exe_dir: &Path,
    next: &Path,
    prev: &Path,
    key: &str,
    installed_keys: Option<&[String]>,
) -> io::Result<RoomOutcome> {
    let is_abandoned = |k: &str| {
        installed_keys.is_some_and(|keys| !keys.is_empty() && !keys.iter().any(|cur| cur == k))
    };
    match slot(next) {
        Slot::Unreadable => {
            return Err(io::Error::other(format!(
                "cannot read key of {}, leaving it intact",
                next.display()
            )));
        }
        Slot::Key(next_key) if next_key != RETIRED_KEY => {
            if is_abandoned(&next_key) {
                retire_batch(exe_dir, next);
                if std::fs::symlink_metadata(next).is_err() {
                    return Ok(RoomOutcome::Cleared);
                }
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("cannot remove abandoned batch {}", next.display()),
                ));
            }
            if next_key == key
                && is_owned_batch(exe_dir, prev)
                && matches!(slot(prev), Slot::Key(k) if k == key)
            {
                // The original same-exe batch is already recoverable in PREV_DIR.
                // Replace only the intermediate NEXT_DIR on another restage.
                retire_batch(exe_dir, next);
                if std::fs::symlink_metadata(next).is_err() {
                    return Ok(RoomOutcome::Cleared);
                }
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("cannot replace intermediate batch {}", next.display()),
                ));
            }
            let prev_reclaimable = match slot(prev) {
                Slot::Key(k) if k == RETIRED_KEY => true,
                Slot::Key(k) => is_abandoned(&k),
                _ => false,
            };
            if prev_reclaimable {
                retire_batch(exe_dir, prev);
            }
            if std::fs::symlink_metadata(prev).is_err() {
                std::fs::rename(next, prev)?;
                return Ok(if next_key == key {
                    RoomOutcome::SetAsideSameKey
                } else {
                    RoomOutcome::SetAsideOther
                });
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
        _ => {}
    }
    let id = batch_id(next);
    remove_batch(next);
    if std::fs::symlink_metadata(next).is_err() {
        if let Some(id) = id {
            forget_batch(exe_dir, &id)?;
        }
    }
    Ok(RoomOutcome::Cleared)
}

/// Removes the next batch of `exe_dir` only when it is an AeroFTP-owned
/// batch whose per-attempt token (`STAGING_ID_FILE`) matches `token`, plus
/// any staging leftover: the update attempt did not go through. An unowned
/// folder (even with a matching key), a batch whose ownership was
/// transferred to a same-exe retry, or another exe's batch stays whole.
pub(crate) fn discard_pending_docs(exe_dir: &Path, token: &str) {
    let _ = discard_pending_docs_within(exe_dir, token, None);
}

#[cfg(test)]
const DISCARD_LOCK_TIMEOUT_MARKER_ENV: &str = "AEROFTP_TEST_DISCARD_LOCK_TIMEOUT";

fn lock_for_discard(exe_dir: &Path, wait: Option<Duration>) -> io::Result<BatchLock> {
    #[cfg(test)]
    if wait.is_none() && std::env::var_os(DISCARD_LOCK_TIMEOUT_MARKER_ENV).is_some() {
        loop {
            match lock_batches_within(exe_dir, Some(Duration::from_millis(200))) {
                Ok(lock) => return Ok(lock),
                Err(e) if e.kind() == io::ErrorKind::TimedOut => {
                    if let Some(path) = std::env::var_os(DISCARD_LOCK_TIMEOUT_MARKER_ENV) {
                        std::fs::write(path, b"timeout")?;
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }
    lock_batches_within(exe_dir, wait)
}

/// Same as `discard_pending_docs`, waiting at most `wait` for the batch lock
/// (`None` waits unboundedly). Cancellation is persisted before any lock wait,
/// closing the timeout/install race. Returns `false` when that marker cannot be
/// written, the lock fails, or a bounded wait times out.
pub(crate) fn discard_pending_docs_within(
    exe_dir: &Path,
    token: &str,
    wait: Option<Duration>,
) -> bool {
    if let Err(e) = record_cancelled_batch(exe_dir, token) {
        log::warn!("Could not persist portable batch cancellation for {token}: {e}");
        return false;
    }
    let _lock = match lock_for_discard(exe_dir, wait) {
        Ok(lock) => lock,
        Err(e) => {
            if e.kind() != io::ErrorKind::TimedOut {
                log::warn!("Could not lock portable document batches for discard: {e}");
            }
            return false;
        }
    };
    let _ = apply_cancelled_batches(exe_dir);
    remove_staging_leftovers(exe_dir);
    let next = exe_dir.join(NEXT_DIR);
    let mut cleaned_target = true;
    match batch_ownership(exe_dir, &next) {
        BatchOwnership::Owned => match batch_id(&next) {
            Some(id) if id == token => {
                remove_batch(&next);
                if std::fs::symlink_metadata(&next).is_err() {
                    let _ = forget_batch(exe_dir, token);
                } else {
                    // The pre-recorded marker stays until a later pass finishes.
                    cleaned_target = false;
                }
            }
            Some(_) => {}
            None => cleaned_target = false,
        },
        BatchOwnership::Unreadable => cleaned_target = false,
        BatchOwnership::Unowned => {}
    }
    if cleaned_target {
        let _ = std::fs::remove_file(cancel_marker_path(exe_dir, token));
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
    true
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
fn install_pass(
    exe_dir: &Path,
    own_key: &str,
    wait: Option<Duration>,
    replace: impl Fn(&Path, &Path) -> io::Result<()>,
) -> Pending {
    install_pass_with(exe_dir, own_key, wait, replace, |p| {
        std::fs::read_to_string(p)
    })
}

fn install_pass_with(
    exe_dir: &Path,
    own_key: &str,
    wait: Option<Duration>,
    replace: impl Fn(&Path, &Path) -> io::Result<()>,
    read_id: impl Fn(&Path) -> io::Result<String>,
) -> Pending {
    install_pass_with_promote(exe_dir, own_key, wait, replace, read_id, rename_no_replace)
}

fn install_pass_with_promote(
    exe_dir: &Path,
    own_key: &str,
    wait: Option<Duration>,
    replace: impl Fn(&Path, &Path) -> io::Result<()>,
    read_id: impl Fn(&Path) -> io::Result<String>,
    promote: impl Fn(&Path, &Path) -> io::Result<()>,
) -> Pending {
    let _lock = match lock_batches_within(exe_dir, wait) {
        Ok(lock) => lock,
        Err(e) => return Pending::Unavailable(e.to_string()),
    };
    let cancelled_next = apply_cancelled_batches_with(exe_dir, &read_id).unfinished;
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
    // Only AeroFTP-owned batches (registered in the ownership ledger) may be
    // promoted or installed; an unowned folder with a matching exe.key is
    // treated as Invalid and left untouched, while a transient read error on
    // a regular `staging.id` or the ledger yields `Slot::OwnershipUnreadable`.
    let owned_slot = |p: &Path| -> Slot {
        match std::fs::symlink_metadata(p) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Slot::Missing,
            Err(_) => Slot::OwnershipUnreadable,
            Ok(_) => match batch_ownership_with(exe_dir, p, &read_id) {
                BatchOwnership::Owned => slot(p),
                BatchOwnership::Unreadable => Slot::OwnershipUnreadable,
                BatchOwnership::Unowned => Slot::Invalid,
            },
        }
    };
    let mut source = current.clone();
    let next_slot = if cancelled_next {
        Slot::Invalid
    } else {
        owned_slot(&next)
    };
    match next_slot {
        Slot::Missing => {}
        Slot::Key(k) if k == own_key => {
            // This exe's documents supersede an older exe's, and the batch
            // its update set aside.
            stale |= retire_batch(exe_dir, &current);
            stale |= retire_batch(exe_dir, &prev);
            // Never replace an existing (including empty unowned) `PENDING_DIR`:
            // install directly from `NEXT_DIR` when `PENDING_DIR` is occupied.
            if std::fs::symlink_metadata(&current).is_ok() || promote(&next, &current).is_err() {
                source = next.clone();
            }
        }
        // A key or ownership identity that cannot be read now may be this exe's: a later pass.
        Slot::Unreadable => {
            let why = format!("the key of {} cannot be read now", next.display());
            return Pending::Unavailable(why);
        }
        Slot::OwnershipUnreadable => {
            let why = format!(
                "the ownership identity of {} cannot be read now",
                next.display()
            );
            return Pending::Unavailable(why);
        }
        // Another exe's: an update, of this process or another, may be
        // waiting for its swap. It is never read here; a failed swap, a
        // discard or the next staging removes it. No batch: not ours.
        Slot::Key(_) | Slot::Invalid => {}
    }
    // This exe's batch, set aside by the staging of an update that has not
    // replaced this exe: its documents install from there.
    if !matches!(owned_slot(&source), Slot::Key(k) if k == own_key) {
        match owned_slot(&prev) {
            Slot::Key(k) if k == own_key => source = prev.clone(),
            Slot::Unreadable => {
                let why = format!("the key of {} cannot be read now", prev.display());
                return Pending::Unavailable(why);
            }
            Slot::OwnershipUnreadable => {
                let why = format!(
                    "the ownership identity of {} cannot be read now",
                    prev.display()
                );
                return Pending::Unavailable(why);
            }
            _ => {}
        }
    }
    match owned_slot(&source) {
        Slot::Missing => {
            let found = if stale { Pending::Stale } else { Pending::None };
            return found;
        }
        Slot::Key(k) if k == own_key => {}
        // Retired and still held: removed by a later pass.
        Slot::Key(k) if k == RETIRED_KEY => {
            let found = if stale { Pending::Stale } else { Pending::None };
            return found;
        }
        // Another exe's batch: this exe may be an older companion the
        // update could not replace, and the batch the new exe's, waiting for
        // documents to be released. Only the exe whose batch supersedes it
        // removes it (above). A key that cannot be read now: a later pass.
        Slot::Key(_) => return Pending::None,
        Slot::Unreadable => {
            let why = format!("the key of {} cannot be read now", source.display());
            return Pending::Unavailable(why);
        }
        Slot::OwnershipUnreadable => {
            let why = format!(
                "the ownership identity of {} cannot be read now",
                source.display()
            );
            return Pending::Unavailable(why);
        }
        // No batch (no key, a file, a link): not ours, left as it is.
        Slot::Invalid => {
            let found = if stale { Pending::Stale } else { Pending::None };
            return found;
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
        if matches!(outcome, Outcome::Failed(_))
            || (matches!(outcome, Outcome::Directory) && name != "portable.marker")
        {
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
    Pending::Docs(outcomes)
}

/// One install pass over the batches of `exe_dir` for the exe whose key is
/// `own_key`. `replace` moves a pending document over the installed one
/// (`std::fs::rename` in production).
pub(crate) fn install_pending_docs(
    exe_dir: &Path,
    own_key: &str,
    replace: impl Fn(&Path, &Path) -> io::Result<()>,
) -> Pending {
    install_pass(exe_dir, own_key, None, replace)
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
    install_pass(exe_dir, own_key, Some(wait), replace)
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
    retry_held_with_hook(
        exe_dir,
        own_key,
        first,
        (rounds, pause),
        replace,
        sleep,
        || {},
    )
}

fn retry_held_with_hook(
    exe_dir: &Path,
    own_key: &str,
    first: Pending,
    budget: (u32, Duration),
    replace: impl Fn(&Path, &Path) -> io::Result<()>,
    sleep: impl Fn(Duration),
    mut after_pass: impl FnMut(),
) -> Pending {
    let (mut left, pause) = budget;
    let mut first = first;
    // A first pass that could not look at the batches (a key that cannot be
    // read now, the lock) is tried again within the same budget.
    while left > 0 && matches!(first, Pending::Unavailable(_)) {
        left -= 1;
        sleep(pause);
        first = install_pass(exe_dir, own_key, None, &replace);
    }
    let Pending::Docs(mut last) = first else {
        return first;
    };
    while left > 0 && last.iter().any(|(_, outcome)| outcome.waits()) {
        left -= 1;
        sleep(pause);
        let retried = install_pass(exe_dir, own_key, None, &replace);
        after_pass();
        // A pass that could not look at the batch (the lock, a key that
        // cannot be read now) says nothing about the held documents: keep
        // the held result and use the remaining retry budget.
        if matches!(retried, Pending::Unavailable(_)) {
            continue;
        }
        if let Pending::Docs(retried_docs) = &retried {
            for (name, outcome) in retried_docs {
                if let Some(entry) = last.iter_mut().find(|(n, _)| n == name) {
                    entry.1 = outcome.clone();
                } else {
                    last.push((*name, outcome.clone()));
                }
            }
            // Decide which waiting documents remain from the outcomes observed
            // under the lock, never by re-checking a mutable slot path after
            // releasing the lock (a concurrent staging may move NEXT_DIR to
            // PREV_DIR immediately after `install_pass` drops the mutex).
            last.retain(|(name, outcome)| {
                !outcome.waits() || retried_docs.iter().any(|(n, _)| n == name)
            });
        } else {
            last.retain(|(_, outcome)| !outcome.waits());
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
    let cancelled_next = apply_cancelled_batches(exe_dir).unfinished;
    let held: Vec<(PathBuf, String)> = [NEXT_DIR, PENDING_DIR, PREV_DIR]
        .iter()
        .filter(|name| !(cancelled_next && **name == NEXT_DIR))
        .filter_map(|name| {
            let dir = exe_dir.join(name);
            if !is_owned_batch(exe_dir, &dir) {
                return None;
            }
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
pub(crate) use tests::FolderArtifactsGuard;

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

    /// Clean up per-folder temp-dir artifacts (Unix lock file, mirror batch
    /// ledger `.batches`, staging record `.staging`, and About notice snapshots)
    /// for `folder_id`.
    pub(crate) fn cleanup_folder_artifacts_by_id(id: &str) {
        let _ = std::fs::remove_file(
            std::env::temp_dir().join(format!("aeroftp-pending-docs-{id}.batches")),
        );
        let _ = std::fs::remove_file(
            std::env::temp_dir().join(format!("aeroftp-pending-docs-{id}.staging")),
        );
        #[cfg(not(windows))]
        let _ = std::fs::remove_file(
            std::env::temp_dir().join(format!("aeroftp-pending-docs-{id}.lock")),
        );
        let snapshot_prefix = format!("aeroftp-pending-notices-{id}-");
        if let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                if name.to_string_lossy().starts_with(&snapshot_prefix) {
                    let p = entry.path();
                    if let Ok(meta) = std::fs::metadata(&p) {
                        let mut perms = meta.permissions();
                        if perms.readonly() {
                            #[cfg(unix)]
                            {
                                use std::os::unix::fs::PermissionsExt;
                                perms.set_mode(0o600);
                            }
                            #[cfg(not(unix))]
                            #[allow(clippy::permissions_set_readonly_false)]
                            {
                                perms.set_readonly(false);
                            }
                            let _ = std::fs::set_permissions(&p, perms);
                        }
                    }
                    let _ = std::fs::remove_file(&p);
                }
            }
        }
    }

    /// Scoped cleanup guard for an explicit installation directory path (for
    /// tests that rename/move the directory or create subdirectories under a
    /// shared root `TempDir`).
    pub(crate) struct FolderArtifactsGuard {
        id: String,
    }

    impl FolderArtifactsGuard {
        pub(crate) fn new(exe_dir: &Path) -> Self {
            Self {
                id: folder_id(exe_dir),
            }
        }
    }

    impl Drop for FolderArtifactsGuard {
        fn drop(&mut self) {
            cleanup_folder_artifacts_by_id(&self.id);
        }
    }

    /// Scoped cleanup guard for per-folder temp-dir artifacts (Unix lock file,
    /// mirror batch ledger, staging record, and About notice snapshots) so
    /// every test leaves no files behind in `std::env::temp_dir()`, even on
    /// assertion failure.
    struct FixtureDir {
        dir: tempfile::TempDir,
        id: String,
    }

    impl FixtureDir {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("temp dir");
            let id = folder_id(dir.path());
            Self { dir, id }
        }

        fn path(&self) -> &Path {
            self.dir.path()
        }
    }

    impl Drop for FixtureDir {
        fn drop(&mut self) {
            cleanup_folder_artifacts_by_id(&self.id);
            let current_id = folder_id(self.dir.path());
            if current_id != self.id {
                cleanup_folder_artifacts_by_id(&current_id);
            }
        }
    }

    /// An app folder with installed documents and a pending folder for the
    /// exe whose key is `key` (none: no key file).
    fn folder(key: Option<&str>) -> FixtureDir {
        let dir = FixtureDir::new();
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
        let dir = FixtureDir::new();
        std::fs::create_dir(dir.path().join(NEXT_DIR)).unwrap();
        std::fs::write(dir.path().join(NEXT_DIR).join("README.txt"), "no key").unwrap();
        assert_eq!(install_pending_docs(dir.path(), KEY, rename), Pending::None);
        assert_eq!(read(&dir.path().join(NEXT_DIR), "README.txt"), "no key");
        assert!(!dir.path().join("README.txt").exists());
        let dir = FixtureDir::new();
        std::fs::write(dir.path().join(NEXT_DIR), b"not a folder").unwrap();
        assert_eq!(install_pending_docs(dir.path(), KEY, rename), Pending::None);
        assert_eq!(read(dir.path(), NEXT_DIR), "not a folder");
        // A next batch of another exe is kept for its update, and never read.
        let dir = FixtureDir::new();
        std::fs::create_dir(dir.path().join(NEXT_DIR)).unwrap();
        std::fs::write(dir.path().join(NEXT_DIR).join(KEY_FILE), "other-exe").unwrap();
        std::fs::write(dir.path().join(NEXT_DIR).join("README.txt"), "other").unwrap();
        assert_eq!(install_pending_docs(dir.path(), KEY, rename), Pending::None);
        assert_eq!(read(&dir.path().join(NEXT_DIR), "README.txt"), "other");
        assert!(!dir.path().join("README.txt").exists());
        let dir = FixtureDir::new();
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

    /// The staging record preserves case on case-sensitive Unix volumes so
    /// `App` and `app` never sweep each other's stagings; on Windows,
    /// `folder_id` normalizes canonical paths case-insensitively and alias
    /// equivalence is pinned by `folder_id_and_batch_lock_share_namespace_across_symlink_or_junction`.
    #[cfg(not(windows))]
    #[test]
    fn the_staging_record_of_a_folder_is_not_the_record_of_its_other_case() {
        let upper = staging_record(Path::new("/portable/App"));
        let lower = staging_record(Path::new("/portable/app"));
        assert_ne!(upper, lower);
        assert_eq!(upper, staging_record(Path::new("/portable/App")));
    }

    #[test]
    fn plain_windows_path_strips_verbatim_prefixes_for_legacy_mutex_compatibility() {
        assert_eq!(plain_windows_path(r"\\?\C:\AeroFTP"), r"C:\AeroFTP");
        assert_eq!(
            plain_windows_path(r"\\?\UNC\server\share\AeroFTP"),
            r"\\server\share\AeroFTP"
        );
        assert_eq!(plain_windows_path(r"C:\AeroFTP"), r"C:\AeroFTP");
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
        let dir = FixtureDir::new();
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
        // A document that cannot be removed (here a folder at its name): with
        // a fresh fixture PREV_DIR is free, so same-key restaging sets the old
        // batch aside and publishes the installable replacement at NEXT_DIR.
        let dir = FixtureDir::new();
        stage_pending_docs(dir.path(), &exe_b, &docs, "held-a").unwrap();
        let next = dir.path().join(NEXT_DIR);
        std::fs::remove_file(next.join("README.txt")).unwrap();
        std::fs::remove_file(next.join("LICENSE.txt")).unwrap();
        std::fs::create_dir(next.join("LICENSE.txt")).unwrap();
        let restaged2 = stage_pending_docs(dir.path(), &exe_b, &docs, "c")
            .unwrap()
            .expect("restaged with free PREV_DIR");
        let prev = dir.path().join(PREV_DIR);
        assert!(
            prev.join("LICENSE.txt").is_dir(),
            "old batch's unremovable folder is preserved at PREV_DIR"
        );
        assert_eq!(read(&next, STAGING_ID_FILE), restaged2);
        assert_eq!(read(&next, "README.txt"), "readme of B");
        assert_eq!(read(&next, "LICENSE.txt"), "license of B");
        // When PREV_DIR is occupied (here still holding that unremovable folder),
        // a further incomplete-batch restage cannot set NEXT_DIR aside and fails,
        // leaving NEXT_DIR and PREV_DIR untouched.
        std::fs::remove_file(next.join("README.txt")).unwrap();
        std::fs::remove_file(next.join("LICENSE.txt")).unwrap();
        std::fs::create_dir(next.join("LICENSE.txt")).unwrap();
        assert!(stage_pending_docs(dir.path(), &exe_b, &docs, "d").is_err());
        assert!(next.join("LICENSE.txt").is_dir());
        assert!(prev.join("LICENSE.txt").is_dir());
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
            let dir = FixtureDir::new();
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
        let dir = FixtureDir::new();
        for name in DOCS {
            std::fs::write(dir.path().join(name), format!("old {name}")).unwrap();
        }
        let next = dir.path().join(NEXT_DIR);
        std::fs::create_dir(&next).unwrap();
        let next_id = format!("{:032x}", rand::random::<u128>());
        std::fs::write(next.join(STAGING_ID_FILE), &next_id).unwrap();
        record_batch(dir.path(), &next_id).unwrap();
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
        let dir = FixtureDir::new();
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
        let dir = FixtureDir::new();
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
        let next_id = format!("{:032x}", rand::random::<u128>());
        std::fs::write(next.join(STAGING_ID_FILE), &next_id).unwrap();
        record_batch(dir.path(), &next_id).unwrap();
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
        let dir = FixtureDir::new();
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
        let dir = FixtureDir::new();
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
        let _orig_guard = FolderArtifactsGuard::new(&orig);
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
        let _moved_guard = FolderArtifactsGuard::new(&moved);
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
        let dir = FixtureDir::new();
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
        let dir = FixtureDir::new();
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
        let dir2 = FixtureDir::new();
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

    /// An unrecognized file at `.aeroftp-pending-docs.owned` (binary bytes or
    /// non-ledger text) stops staging without being modified, while a valid
    /// ledger accepts a new record and keeps its previous IDs.
    #[test]
    fn an_unrecognized_ledger_file_stops_staging_unchanged_and_a_valid_ledger_accepts_records() {
        let dir = FixtureDir::new();
        let (_stage, exe_b, _key_b, readme_b) = new_exe(b"B");
        let ledger = owned_batch_record(dir.path());
        let binary = [0xffu8, 0x00, 0xfe, 0x01, 0x80];
        std::fs::write(&ledger, binary).unwrap();
        assert!(
            stage_pending_docs(dir.path(), &exe_b, &[(&readme_b, "README.txt")], "b1").is_err()
        );
        assert_eq!(std::fs::read(&ledger).unwrap(), binary);
        std::fs::write(&ledger, "not a ledger line\n").unwrap();
        assert!(
            stage_pending_docs(dir.path(), &exe_b, &[(&readme_b, "README.txt")], "b2").is_err()
        );
        assert_eq!(
            std::fs::read_to_string(&ledger).unwrap(),
            "not a ledger line\n"
        );
        // Control: a valid ledger with an existing 32-char hex ID accepts a new record.
        let prior = "aa".repeat(16);
        std::fs::write(&ledger, format!("{prior}\n")).unwrap();
        let token = stage_pending_docs(dir.path(), &exe_b, &[(&readme_b, "README.txt")], "b3")
            .unwrap()
            .unwrap();
        let ids = read_ids_from(&ledger);
        assert!(ids.contains(&prior), "{ids:?}");
        assert!(ids.contains(&token), "{ids:?}");
    }

    /// A retry with the same executable bytes and a corrected document
    /// restages the corrected document instead of reusing the stale copy;
    /// identical document contents still reuse and transfer ownership.
    #[test]
    fn a_retry_with_corrected_document_contents_for_the_same_exe_is_restaged() {
        const NOTICES: &str = "THIRD-PARTY-NOTICES.md";
        let dir = FixtureDir::new();
        let (stage, exe_b, _key_b, _) = new_exe(b"B");
        let notices = stage.path().join(NOTICES);
        std::fs::write(&notices, "notices v1").unwrap();
        let t1 = stage_pending_docs(dir.path(), &exe_b, &[(&notices, NOTICES)], "v1")
            .unwrap()
            .unwrap();
        let next = dir.path().join(NEXT_DIR);
        assert_eq!(read(&next, NOTICES), "notices v1");
        // Same exe, corrected notices -> must restage, not reuse v1.
        std::fs::write(&notices, "notices v2").unwrap();
        let t2 = stage_pending_docs(dir.path(), &exe_b, &[(&notices, NOTICES)], "v2")
            .unwrap()
            .unwrap();
        assert_ne!(t2, t1);
        assert_eq!(read(&next, NOTICES), "notices v2");
        // Control: identical content reuses and transfers ownership to a fresh token.
        let t3 = stage_pending_docs(dir.path(), &exe_b, &[(&notices, NOTICES)], "v3")
            .unwrap()
            .unwrap();
        assert_ne!(t3, t2);
        assert_eq!(read(&next, NOTICES), "notices v2");
    }

    /// With version A still installed (`exe_dir/AeroFTP.exe`), interrupting
    /// update B before swap and interrupting update C before swap never blocks
    /// update D: abandoned non-running batches are reclaimed safely while A's
    /// own held batch is preserved.
    #[test]
    fn abandoned_interrupted_updates_are_reclaimed_while_installed_a_keeps_updating() {
        const NOTICES: &str = "THIRD-PARTY-NOTICES.md";
        let notices_of = |stage: &tempfile::TempDir, who: &str| {
            let path = stage.path().join(NOTICES);
            std::fs::write(&path, format!("notices of {who}")).unwrap();
            path
        };
        let dir = FixtureDir::new();
        std::fs::write(dir.path().join("AeroFTP.exe"), b"A").unwrap();
        std::fs::write(dir.path().join(NOTICES), "installed A").unwrap();
        let (stage_b, exe_b, _key_b, _) = new_exe(b"B");
        let (stage_c, exe_c, _key_c, _) = new_exe(b"C");
        let (stage_d, exe_d, key_d, _) = new_exe(b"D");
        let notices_b = notices_of(&stage_b, "B");
        let notices_c = notices_of(&stage_c, "C");
        let notices_d = notices_of(&stage_d, "D");
        // B stages and stops before swap; C stages and stops before swap; D stages cleanly.
        stage_pending_docs(dir.path(), &exe_b, &[(&notices_b, NOTICES)], "b").unwrap();
        stage_pending_docs(dir.path(), &exe_c, &[(&notices_c, NOTICES)], "c").unwrap();
        stage_pending_docs(dir.path(), &exe_d, &[(&notices_d, NOTICES)], "d").unwrap();
        assert_eq!(read(&dir.path().join(NEXT_DIR), KEY_FILE), key_d);
        assert_eq!(read(&dir.path().join(NEXT_DIR), NOTICES), "notices of D");
        // Also if an abandoned B batch already sits in PREV_DIR while A's own
        // held batch sits in NEXT_DIR, staging C reclaims abandoned B and keeps A.
        let dir2 = FixtureDir::new();
        std::fs::write(dir2.path().join("AeroFTP.exe"), b"A").unwrap();
        let key_a = exe_key(&dir2.path().join("AeroFTP.exe")).unwrap();
        let (stage_a, exe_a, _, _) = new_exe(b"A");
        let notices_a = notices_of(&stage_a, "A");
        stage_pending_docs(dir2.path(), &exe_b, &[(&notices_b, NOTICES)], "b2").unwrap();
        std::fs::rename(dir2.path().join(NEXT_DIR), dir2.path().join(PREV_DIR)).unwrap();
        stage_pending_docs(dir2.path(), &exe_a, &[(&notices_a, NOTICES)], "a2").unwrap();
        stage_pending_docs(dir2.path(), &exe_c, &[(&notices_c, NOTICES)], "c2").unwrap();
        assert_eq!(read(&dir2.path().join(PREV_DIR), KEY_FILE), key_a);
        assert_eq!(read(&dir2.path().join(PREV_DIR), NOTICES), "notices of A");
    }

    /// Failed-swap cleanup removes `exe.key` before `staging.id`: if `exe.key`
    /// cannot be removed (held open without delete sharing on Windows),
    /// `staging.id` stays so the batch remains AeroFTP-owned and recoverable
    /// once the handle closes.
    #[cfg(windows)]
    #[test]
    fn a_batch_whose_exe_key_is_held_during_discard_stays_recoverable() {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 1;
        let dir = FixtureDir::new();
        let (_stage_b, exe_b, _key_b, readme_b) = new_exe(b"B");
        let token_b = stage_pending_docs(dir.path(), &exe_b, &[(&readme_b, "README.txt")], "b")
            .unwrap()
            .unwrap();
        let next = dir.path().join(NEXT_DIR);
        let held_key = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(next.join(KEY_FILE))
            .expect("hold exe.key without delete sharing");
        discard_pending_docs(dir.path(), &token_b);
        assert!(next.join(KEY_FILE).is_file());
        assert!(next.join(STAGING_ID_FILE).is_file());
        assert!(is_owned_batch(dir.path(), &next));
        drop(held_key);
        let (_stage_c, exe_c, key_c, readme_c) = new_exe(b"C");
        stage_pending_docs(dir.path(), &exe_c, &[(&readme_c, "README.txt")], "c").unwrap();
        assert_eq!(read(&next, KEY_FILE), key_c);
        assert_eq!(read(&next, "README.txt"), "readme of C");
    }

    /// On Unix, verify `remove_batch` keeps `STAGING_ID_FILE` whenever
    /// `KEY_FILE` could not be removed (here `KEY_FILE` is kept by read-only
    /// parent permissions), so the batch remains AeroFTP-owned and recoverable
    /// once writable again.
    #[cfg(unix)]
    #[test]
    fn a_batch_whose_exe_key_cannot_be_removed_during_discard_stays_recoverable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = FixtureDir::new();
        let (_stage_b, exe_b, _key_b, readme_b) = new_exe(b"B");
        let token_b = stage_pending_docs(dir.path(), &exe_b, &[(&readme_b, "README.txt")], "b")
            .unwrap()
            .unwrap();
        let next = dir.path().join(NEXT_DIR);
        std::fs::remove_file(next.join("README.txt")).unwrap();
        // Make NEXT_DIR read-only so removing exe.key fails; staging.id must stay too.
        // A runner with CAP_DAC_OVERRIDE can still delete files: use a real probe
        // and exercise only the successful-cleanup control in that environment.
        let probe = next.join("permission-probe");
        std::fs::write(&probe, b"probe").unwrap();
        std::fs::set_permissions(&next, std::fs::Permissions::from_mode(0o555)).unwrap();
        if std::fs::remove_file(&probe).is_ok() {
            std::fs::set_permissions(&next, std::fs::Permissions::from_mode(0o755)).unwrap();
            discard_pending_docs(dir.path(), &token_b);
            assert!(!next.exists());
            return;
        }
        discard_pending_docs(dir.path(), &token_b);
        std::fs::set_permissions(&next, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::remove_file(probe).unwrap();
        assert!(next.join(KEY_FILE).is_file());
        assert!(next.join(STAGING_ID_FILE).is_file());
        assert!(is_owned_batch(dir.path(), &next));
        let (_stage_c, exe_c, key_c, readme_c) = new_exe(b"C");
        stage_pending_docs(dir.path(), &exe_c, &[(&readme_c, "README.txt")], "c").unwrap();
        assert_eq!(read(&next, KEY_FILE), key_c);
        assert_eq!(read(&next, "README.txt"), "readme of C");
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
        let dir = FixtureDir::new();
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
        let dir = FixtureDir::new();
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
        let dir = FixtureDir::new();
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
        let dir = FixtureDir::new();
        notices(dir.path(), NEXT_DIR, "new-exe", "new");
        assert_eq!(own_pending_notices(dir.path(), own), None);
        // A batch without a key is nobody's.
        let dir = FixtureDir::new();
        notices(dir.path(), PENDING_DIR, KEY, "own");
        std::fs::remove_file(dir.path().join(PENDING_DIR).join(KEY_FILE)).unwrap();
        assert_eq!(own_pending_notices(dir.path(), own), None);
    }

    #[test]
    fn a_pass_never_sees_a_staging_half_done_and_never_sweeps_a_live_one() {
        // A staging holds the lock while it writes its temporary folder and
        // publishes it; a pass started meanwhile waits, then finds the batch
        // whole, and the temporary folder was never removed under it.
        let dir = FixtureDir::new();
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
                let id = format!("{:032x}", rand::random::<u128>());
                std::fs::write(tmp.join(STAGING_ID_FILE), &id).unwrap();
                record_batch(&exe_dir, &id).unwrap();
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
    fn a_running_companion_at_aeroftp_app_exe_keeps_its_batch_when_aeroftp_exe_is_older() {
        let dir = FixtureDir::new();
        let src = tempfile::tempdir().unwrap();
        let exe_a = dir.path().join("AeroFTP.exe");
        let exe_b = dir.path().join("aeroftp-app.exe");
        let exe_c = src.path().join("c.exe");
        let exe_x = src.path().join("x.exe");
        std::fs::write(&exe_a, b"exe A (older primary)").unwrap();
        std::fs::write(&exe_b, b"exe B (running companion)").unwrap();
        std::fs::write(&exe_c, b"exe C (new update)").unwrap();
        std::fs::write(&exe_x, b"exe X (abandoned update)").unwrap();
        let key_b = exe_key(&exe_b).unwrap();
        let key_c = exe_key(&exe_c).unwrap();

        let doc_b = src.path().join("b-readme.txt");
        let doc_c = src.path().join("c-readme.txt");
        let doc_x = src.path().join("x-readme.txt");
        std::fs::write(&doc_b, "docs of B").unwrap();
        std::fs::write(&doc_c, "docs of C").unwrap();
        std::fs::write(&doc_x, "docs of X").unwrap();

        // Companion B has a staged batch in NEXT_DIR while AeroFTP.exe still
        // holds older A. Staging C must treat B as live (setting B aside to
        // PREV_DIR), not delete it as abandoned.
        stage_pending_docs(dir.path(), &exe_b, &[(&doc_b, "README.txt")], "b").unwrap();
        let token_c = stage_pending_docs(dir.path(), &exe_c, &[(&doc_c, "README.txt")], "c")
            .unwrap()
            .expect("token C");
        assert_eq!(read(&dir.path().join(NEXT_DIR), KEY_FILE), key_c);
        assert_eq!(read(&dir.path().join(PREV_DIR), KEY_FILE), key_b);
        assert_eq!(read(&dir.path().join(PREV_DIR), "README.txt"), "docs of B");

        // If C's swap fails and discards its batch, B's batch returns to
        // NEXT_DIR and B can still install its documents.
        discard_pending_docs(dir.path(), &token_c);
        assert_eq!(read(&dir.path().join(NEXT_DIR), KEY_FILE), key_b);
        assert!(!dir.path().join(PREV_DIR).exists());
        let Pending::Docs(outcomes) = install_pending_docs(dir.path(), &key_b, rename) else {
            panic!("B installs its docs");
        };
        assert_eq!(outcomes, vec![("README.txt", Outcome::Installed)]);
        assert_eq!(read(dir.path(), "README.txt"), "docs of B");

        // By contrast, an abandoned batch X matching neither AeroFTP.exe nor
        // aeroftp-app.exe is still reclaimed directly without occupying PREV_DIR.
        stage_pending_docs(dir.path(), &exe_x, &[(&doc_x, "README.txt")], "x").unwrap();
        stage_pending_docs(dir.path(), &exe_c, &[(&doc_c, "README.txt")], "c2").unwrap();
        assert_eq!(read(&dir.path().join(NEXT_DIR), KEY_FILE), key_c);
        assert!(!dir.path().join(PREV_DIR).exists());
    }

    #[test]
    fn discard_pending_docs_within_times_out_while_lock_is_held_and_runs_after_release() {
        let dir = FixtureDir::new();
        let src = tempfile::tempdir().unwrap();
        let exe = src.path().join("new.exe");
        let doc = src.path().join("README.txt");
        std::fs::write(&exe, b"new exe").unwrap();
        std::fs::write(&doc, "new readme").unwrap();
        let token = stage_pending_docs(dir.path(), &exe, &[(&doc, "README.txt")], "t")
            .unwrap()
            .expect("token");

        let exe_dir = dir.path().to_path_buf();
        let (locked_tx, locked_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let holder = std::thread::spawn(move || {
            let _lock = lock_batches(&exe_dir).unwrap();
            locked_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });
        locked_rx.recv().unwrap();
        assert!(
            !discard_pending_docs_within(dir.path(), &token, Some(Duration::from_millis(50))),
            "discard must return false when another thread holds the batch lock"
        );
        assert!(
            dir.path().join(NEXT_DIR).join("README.txt").is_file(),
            "batch must stay intact on lock timeout"
        );
        release_tx.send(()).unwrap();
        holder.join().unwrap();

        assert!(
            discard_pending_docs_within(dir.path(), &token, Some(Duration::from_millis(200))),
            "discard must succeed once the batch lock is released"
        );
        assert!(!dir.path().join(NEXT_DIR).exists());
    }

    #[test]
    fn incomplete_executable_inventory_preserves_companion_batch_while_complete_inventory_reclaims_abandoned(
    ) {
        let dir = FixtureDir::new();
        let src = tempfile::tempdir().unwrap();
        let exe_a = dir.path().join("AeroFTP.exe");
        let exe_b = src.path().join("b.exe");
        let exe_c = src.path().join("c.exe");
        let exe_x = src.path().join("x.exe");
        std::fs::write(&exe_a, b"exe A (primary)").unwrap();
        std::fs::write(&exe_b, b"exe B (companion)").unwrap();
        std::fs::write(&exe_c, b"exe C (new update)").unwrap();
        std::fs::write(&exe_x, b"exe X (abandoned)").unwrap();
        let key_b = exe_key(&exe_b).unwrap();
        let key_c = exe_key(&exe_c).unwrap();

        let doc_b = src.path().join("b.txt");
        let doc_c = src.path().join("c.txt");
        let doc_x = src.path().join("x.txt");
        std::fs::write(&doc_b, "docs of B").unwrap();
        std::fs::write(&doc_c, "docs of C").unwrap();
        std::fs::write(&doc_x, "docs of X").unwrap();

        // Companion exists at `aeroftp-app.exe` but fails `exe_key` hashing:
        // `installed_app_keys_with` must treat the inventory as incomplete
        // (`None`) instead of returning only `AeroFTP.exe`.
        let companion = dir.path().join("aeroftp-app.exe");
        std::fs::write(&companion, b"exe B (companion)").unwrap();
        assert_eq!(
            installed_app_keys_with(dir.path(), |p| {
                if p.ends_with("aeroftp-app.exe") {
                    Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "transient lock",
                    ))
                } else {
                    exe_key(p)
                }
            }),
            None
        );

        // End-to-end: make `aeroftp-app.exe` exist yet fail hashing portably
        // (a directory at that path causes `exe_key` to fail with a non-NotFound
        // error on every OS). Staging C must preserve B's batch in PREV_DIR.
        std::fs::remove_file(&companion).unwrap();
        std::fs::create_dir(&companion).unwrap();
        stage_pending_docs(dir.path(), &exe_b, &[(&doc_b, "README.txt")], "b").unwrap();
        stage_pending_docs(dir.path(), &exe_c, &[(&doc_c, "README.txt")], "c").unwrap();
        assert_eq!(read(&dir.path().join(NEXT_DIR), KEY_FILE), key_c);
        assert_eq!(read(&dir.path().join(PREV_DIR), KEY_FILE), key_b);
        assert_eq!(read(&dir.path().join(PREV_DIR), "README.txt"), "docs of B");

        // Control: when both AeroFTP.exe and aeroftp-app.exe hash cleanly, an
        // abandoned batch X matching neither is reclaimed directly.
        std::fs::remove_dir(&companion).unwrap();
        std::fs::write(&companion, b"exe B (companion)").unwrap();
        let _ = std::fs::remove_dir_all(dir.path().join(NEXT_DIR));
        let _ = std::fs::remove_dir_all(dir.path().join(PREV_DIR));
        stage_pending_docs(dir.path(), &exe_x, &[(&doc_x, "README.txt")], "x").unwrap();
        stage_pending_docs(dir.path(), &exe_c, &[(&doc_c, "README.txt")], "c2").unwrap();
        assert_eq!(read(&dir.path().join(NEXT_DIR), KEY_FILE), key_c);
        assert!(!dir.path().join(PREV_DIR).exists());
    }

    #[test]
    fn a_retry_that_omits_a_previously_staged_document_restages_instead_of_reusing() {
        let dir = FixtureDir::new();
        let src = tempfile::tempdir().unwrap();
        let exe = src.path().join("new.exe");
        let marker = src.path().join("portable.marker");
        let readme = src.path().join("README.txt");
        std::fs::write(&exe, b"exe bytes").unwrap();
        std::fs::write(&marker, b"portable").unwrap();
        std::fs::write(&readme, b"earlier readme").unwrap();
        let key = exe_key(&exe).unwrap();

        // First attempt stages both portable.marker and README.txt.
        let tok1 = stage_pending_docs(
            dir.path(),
            &exe,
            &[(&marker, "portable.marker"), (&readme, "README.txt")],
            "first",
        )
        .unwrap()
        .expect("token 1");

        // Retry for the same exe with only portable.marker must NOT reuse the
        // batch that still holds README.txt.
        let tok2 = stage_pending_docs(dir.path(), &exe, &[(&marker, "portable.marker")], "second")
            .unwrap()
            .expect("token 2");
        assert_ne!(tok2, tok1);
        assert!(dir.path().join(NEXT_DIR).join("portable.marker").is_file());
        assert!(
            !dir.path().join(NEXT_DIR).join("README.txt").exists(),
            "omitted README.txt must not remain in NEXT_DIR"
        );

        // Control: a retry with the exact same document set and bytes reuses
        // the batch in place while transferring ownership to a fresh token.
        let tok3 = stage_pending_docs(dir.path(), &exe, &[(&marker, "portable.marker")], "third")
            .unwrap()
            .expect("token 3");
        assert_ne!(tok3, tok2);
        assert!(dir.path().join(NEXT_DIR).join("portable.marker").is_file());
        assert!(!dir.path().join(NEXT_DIR).join("README.txt").exists());

        // Startup installs only portable.marker, never the omitted README.txt.
        let Pending::Docs(outcomes) = install_pending_docs(dir.path(), &key, rename) else {
            panic!("expected installed docs");
        };
        assert_eq!(outcomes, vec![("portable.marker", Outcome::Installed)]);
        assert!(dir.path().join("portable.marker").is_file());
        assert!(!dir.path().join("README.txt").exists());
    }

    #[test]
    fn staging_leaves_next_batch_intact_when_its_key_is_unreadable() {
        let dir = FixtureDir::new();
        let src = tempfile::tempdir().unwrap();
        let exe_b = src.path().join("b.exe");
        let exe_c = src.path().join("c.exe");
        let notice_b = src.path().join("notices-b.md");
        let notice_c = src.path().join("notices-c.md");
        std::fs::write(&exe_b, b"exe B").unwrap();
        std::fs::write(&exe_c, b"exe C").unwrap();
        std::fs::write(&notice_b, "notices of B").unwrap();
        std::fs::write(&notice_c, "notices of C").unwrap();
        let key_b = exe_key(&exe_b).unwrap();
        let key_c = exe_key(&exe_c).unwrap();

        stage_pending_docs(
            dir.path(),
            &exe_b,
            &[(&notice_b, "THIRD-PARTY-NOTICES.md")],
            "b",
        )
        .unwrap();

        // Simulate a temporary unreadable `exe.key` portably across OSes.
        let key_path = dir.path().join(NEXT_DIR).join(KEY_FILE);
        std::fs::remove_file(&key_path).unwrap();
        std::fs::create_dir(&key_path).unwrap();

        // Staging C must fail without touching B's staged notice.
        let err = stage_pending_docs(
            dir.path(),
            &exe_c,
            &[(&notice_c, "THIRD-PARTY-NOTICES.md")],
            "c",
        )
        .expect_err("unreadable next key must stop staging");
        assert!(err.contains("cannot read key"), "{err}");
        assert_eq!(
            read(&dir.path().join(NEXT_DIR), "THIRD-PARTY-NOTICES.md"),
            "notices of B"
        );

        // Once the read hold clears, B can still install its staged notice.
        std::fs::remove_dir(&key_path).unwrap();
        std::fs::write(&key_path, &key_b).unwrap();
        let Pending::Docs(outcomes) = install_pending_docs(dir.path(), &key_b, rename) else {
            panic!("B installs after hold clears");
        };
        assert_eq!(
            outcomes,
            vec![("THIRD-PARTY-NOTICES.md", Outcome::Installed)]
        );
        assert_eq!(read(dir.path(), "THIRD-PARTY-NOTICES.md"), "notices of B");

        // Control: with a readable key, staging C sets B aside to PREV_DIR.
        stage_pending_docs(
            dir.path(),
            &exe_b,
            &[(&notice_b, "THIRD-PARTY-NOTICES.md")],
            "b2",
        )
        .unwrap();
        stage_pending_docs(
            dir.path(),
            &exe_c,
            &[(&notice_c, "THIRD-PARTY-NOTICES.md")],
            "c2",
        )
        .unwrap();
        assert_eq!(read(&dir.path().join(NEXT_DIR), KEY_FILE), key_c);
        assert_eq!(read(&dir.path().join(PREV_DIR), KEY_FILE), key_b);
        assert_eq!(
            read(&dir.path().join(PREV_DIR), "THIRD-PARTY-NOTICES.md"),
            "notices of B"
        );
    }

    #[test]
    fn install_pass_ignores_unowned_folders_with_matching_key_in_every_slot() {
        for slot_name in [PENDING_DIR, NEXT_DIR, PREV_DIR] {
            let dir = FixtureDir::new();
            std::fs::write(dir.path().join("README.txt"), "installed readme").unwrap();
            let slot_dir = dir.path().join(slot_name);
            std::fs::create_dir(&slot_dir).unwrap();
            std::fs::write(slot_dir.join(KEY_FILE), KEY).unwrap();
            std::fs::write(slot_dir.join("README.txt"), "unowned readme").unwrap();

            assert_eq!(
                install_pending_docs(dir.path(), KEY, rename),
                Pending::None,
                "unowned {slot_name} with matching key must not install"
            );
            assert_eq!(read(dir.path(), "README.txt"), "installed readme");
            assert_eq!(read(&slot_dir, "README.txt"), "unowned readme");
            assert_eq!(read(&slot_dir, KEY_FILE), KEY);
        }

        // An unowned NEXT_DIR with a matching key must neither retire an owned
        // PENDING_DIR nor prevent the owned batch from installing.
        let dir = folder(Some(KEY));
        let unowned_next = dir.path().join(NEXT_DIR);
        std::fs::create_dir(&unowned_next).unwrap();
        std::fs::write(unowned_next.join(KEY_FILE), KEY).unwrap();
        std::fs::write(unowned_next.join("README.txt"), "unowned next").unwrap();
        let Pending::Docs(outcomes) = install_pending_docs(dir.path(), KEY, rename) else {
            panic!("owned pending batch must still install");
        };
        assert!(outcomes.contains(&("README.txt", Outcome::Installed)));
        assert_eq!(read(dir.path(), "README.txt"), "new README.txt");
        assert_eq!(read(&unowned_next, "README.txt"), "unowned next");
    }

    #[test]
    fn own_pending_notices_ignores_unowned_matching_key_slots() {
        let dir = folder(Some(KEY));
        // Unowned NEXT_DIR with matching key ahead of registered PENDING_DIR:
        // lookup must skip unowned NEXT_DIR and offer PENDING_DIR's notices.
        let unowned_next = dir.path().join(NEXT_DIR);
        std::fs::create_dir(&unowned_next).unwrap();
        std::fs::write(unowned_next.join(KEY_FILE), KEY).unwrap();
        std::fs::write(
            unowned_next.join("THIRD-PARTY-NOTICES.md"),
            "unowned notices",
        )
        .unwrap();
        let own = || Some(KEY.to_string());
        let offered = own_pending_notices(dir.path(), own).expect("owned pending notices");
        assert_eq!(
            std::fs::read_to_string(&offered).unwrap(),
            "new THIRD-PARTY-NOTICES.md"
        );
        let _ = std::fs::remove_file(&offered);

        // When no slot is owned (even with matching keys in all three slots),
        // lookup returns None.
        let dir2 = FixtureDir::new();
        for slot_name in [NEXT_DIR, PENDING_DIR, PREV_DIR] {
            let slot_dir = dir2.path().join(slot_name);
            std::fs::create_dir(&slot_dir).unwrap();
            std::fs::write(slot_dir.join(KEY_FILE), KEY).unwrap();
            std::fs::write(slot_dir.join("THIRD-PARTY-NOTICES.md"), "unowned").unwrap();
        }
        assert_eq!(own_pending_notices(dir2.path(), own), None);
    }

    #[test]
    fn staging_times_out_when_batch_lock_is_held_without_touching_existing_batch() {
        let dir = FixtureDir::new();
        let src = tempfile::tempdir().unwrap();
        let exe_a = src.path().join("a.exe");
        let exe_b = src.path().join("b.exe");
        let doc_a = src.path().join("a.txt");
        let doc_b = src.path().join("b.txt");
        std::fs::write(&exe_a, b"exe A").unwrap();
        std::fs::write(&exe_b, b"exe B").unwrap();
        std::fs::write(&doc_a, "docs of A").unwrap();
        std::fs::write(&doc_b, "docs of B").unwrap();
        let key_a = exe_key(&exe_a).unwrap();
        let key_b = exe_key(&exe_b).unwrap();

        stage_pending_docs(dir.path(), &exe_a, &[(&doc_a, "README.txt")], "a").unwrap();

        // Hold the lock on a separate thread (Windows mutexes are re-entrant on
        // the owning thread).
        let exe_dir = dir.path().to_path_buf();
        let (locked_tx, locked_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let holder = std::thread::spawn(move || {
            let _lock = lock_batches(&exe_dir).unwrap();
            locked_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });
        locked_rx.recv().unwrap();

        let err = stage_with_wait(
            dir.path(),
            &exe_b,
            &[(&doc_b, "README.txt")],
            "b",
            Some(Duration::from_millis(50)),
            rename,
        )
        .expect_err("staging must time out while another process holds the batch lock");
        assert!(
            err.contains("Failed to stage the portable documents"),
            "{err}"
        );
        assert_eq!(read(&dir.path().join(NEXT_DIR), KEY_FILE), key_a);
        assert_eq!(read(&dir.path().join(NEXT_DIR), "README.txt"), "docs of A");

        release_tx.send(()).unwrap();
        holder.join().unwrap();

        // Control: once the lock is free, staging succeeds normally.
        stage_with_wait(
            dir.path(),
            &exe_b,
            &[(&doc_b, "README.txt")],
            "b2",
            Some(Duration::from_millis(200)),
            rename,
        )
        .unwrap();
        assert_eq!(read(&dir.path().join(NEXT_DIR), KEY_FILE), key_b);
        assert_eq!(read(&dir.path().join(NEXT_DIR), "README.txt"), "docs of B");
    }

    #[test]
    fn a_batch_whose_staging_id_cannot_be_read_yet_recovers_during_startup_retries() {
        let dir = FixtureDir::new();
        let src = tempfile::tempdir().unwrap();
        let exe = src.path().join("app.exe");
        let doc = src.path().join("notices.md");
        std::fs::write(&exe, b"app bytes").unwrap();
        std::fs::write(&doc, "new notices").unwrap();
        std::fs::write(dir.path().join("THIRD-PARTY-NOTICES.md"), "old notices").unwrap();
        let key = exe_key(&exe).unwrap();
        let _token = stage_pending_docs(dir.path(), &exe, &[(&doc, "THIRD-PARTY-NOTICES.md")], "t")
            .unwrap()
            .expect("token");

        // Inject a transient I/O error reading the regular `staging.id` file:
        // must report an ownership identity read failure (distinct from key
        // read failure or folder lock failure) and leave installed notices alone.
        let fail_read = |_: &Path| -> io::Result<String> {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "transient sharing violation",
            ))
        };
        let first = install_pass_with(dir.path(), &key, None, rename, fail_read);
        assert!(
            matches!(&first, Pending::Unavailable(why) if why.contains("ownership identity") && !why.contains("lock")),
            "{first:?}"
        );
        assert_eq!(read(dir.path(), "THIRD-PARTY-NOTICES.md"), "old notices");

        // Hold clears during the startup retry window: notices install in the
        // same startup.
        let outcome = retry_held(dir.path(), &key, first, 3, Duration::ZERO, rename, |_| {});
        assert_eq!(
            outcome,
            Pending::Docs(vec![("THIRD-PARTY-NOTICES.md", Outcome::Installed)])
        );
        assert_eq!(read(dir.path(), "THIRD-PARTY-NOTICES.md"), "new notices");

        // Persistent unreadable `staging.id` keeps returning the ownership
        // identity diagnostic (never misattributed to the folder lock).
        let _token2 =
            stage_pending_docs(dir.path(), &exe, &[(&doc, "THIRD-PARTY-NOTICES.md")], "t2")
                .unwrap()
                .expect("token 2");
        let persistent = install_pass_with(dir.path(), &key, None, rename, fail_read);
        assert!(
            matches!(&persistent, Pending::Unavailable(why) if why.contains("ownership identity") && !why.contains("lock")),
            "{persistent:?}"
        );

        // Non-regular `staging.id` entry (e.g., a directory) is rejected as
        // Unowned without attempting to read it.
        let id_path = dir.path().join(NEXT_DIR).join(STAGING_ID_FILE);
        std::fs::remove_file(&id_path).unwrap();
        std::fs::create_dir(&id_path).unwrap();
        let non_regular = install_pass_with(dir.path(), &key, None, rename, |_| {
            panic!("non-regular staging.id must be rejected before reading")
        });
        assert_eq!(non_regular, Pending::None);
        std::fs::remove_dir_all(dir.path().join(NEXT_DIR)).unwrap();

        // Control: an unregistered folder with readable staging.id is Invalid -> Pending::None.
        let unowned = dir.path().join(NEXT_DIR);
        std::fs::create_dir(&unowned).unwrap();
        std::fs::write(unowned.join(KEY_FILE), &key).unwrap();
        std::fs::write(unowned.join(STAGING_ID_FILE), "00".repeat(16)).unwrap();
        std::fs::write(unowned.join("THIRD-PARTY-NOTICES.md"), "rogue").unwrap();
        assert_eq!(
            install_pending_docs(dir.path(), &key, rename),
            Pending::None
        );
        assert_eq!(read(dir.path(), "THIRD-PARTY-NOTICES.md"), "new notices");
    }

    #[test]
    fn probe_portable_marker_distinguishes_metadata_errors_and_unreadable_slots() {
        let dir = FixtureDir::new();
        let src = tempfile::tempdir().unwrap();
        let exe_a = src.path().join("a.exe");
        let exe_b = src.path().join("b.exe");
        let marker = src.path().join("portable.marker");
        std::fs::write(&exe_a, b"exe A").unwrap();
        std::fs::write(&exe_b, b"exe B").unwrap();
        std::fs::write(&marker, b"portable").unwrap();
        let key_a = exe_key(&exe_a).unwrap();
        let key_b = exe_key(&exe_b).unwrap();

        // 1. Metadata failure on the only staged marker yields Pending (not
        // Absent), then recovers to Present when metadata succeeds.
        stage_pending_docs(dir.path(), &exe_a, &[(&marker, "portable.marker")], "a").unwrap();
        let injected = probe_portable_marker_with(
            dir.path(),
            || Some(key_a.clone()),
            Some(Duration::from_secs(1)),
            |p| {
                if p.ends_with("portable.marker")
                    && p.parent().is_some_and(|d| d.ends_with(NEXT_DIR))
                {
                    Err(io::Error::new(io::ErrorKind::PermissionDenied, "injected"))
                } else {
                    std::fs::symlink_metadata(p)
                }
            },
        );
        assert_eq!(injected, PortableMarkerState::Pending);
        assert_eq!(
            probe_portable_marker_within(
                dir.path(),
                || Some(key_a.clone()),
                Some(Duration::from_secs(1))
            ),
            PortableMarkerState::Present
        );

        // 2. Two slots populated: A set aside in PREV_DIR with an unreadable
        // key, B readable in NEXT_DIR. B matches -> Present; A does not match
        // B and has an unreadable slot -> Pending (never Absent).
        stage_pending_docs(dir.path(), &exe_b, &[(&marker, "portable.marker")], "b").unwrap();
        let prev_key = dir.path().join(PREV_DIR).join(KEY_FILE);
        std::fs::remove_file(&prev_key).unwrap();
        std::fs::create_dir(&prev_key).unwrap();
        assert_eq!(
            probe_portable_marker_within(
                dir.path(),
                || Some(key_b.clone()),
                Some(Duration::from_secs(1))
            ),
            PortableMarkerState::Present
        );
        assert_eq!(
            probe_portable_marker_within(
                dir.path(),
                || Some(key_a.clone()),
                Some(Duration::from_secs(1))
            ),
            PortableMarkerState::Pending
        );

        // 3. Control: a directory with no marker and no batches returns Absent.
        let empty = FixtureDir::new();
        assert_eq!(
            probe_portable_marker_within(
                empty.path(),
                || Some(key_a.clone()),
                Some(Duration::from_secs(1))
            ),
            PortableMarkerState::Absent
        );
    }

    #[cfg(windows)]
    #[test]
    fn handoff_pid_transfer_is_atomic_for_concurrent_inventory_reads() {
        let dir = FixtureDir::new();
        let sidecar = dir.path().join("AeroFTP.exe.atomic01.new");
        std::fs::write(&sidecar, b"exe B").unwrap();
        let key = exe_key(&sidecar).unwrap();
        let reservation = reserve_update_handoff(dir.path(), "atomic01").unwrap();
        std::thread::scope(|scope| {
            let writer = &reservation;
            scope.spawn(move || {
                for pid in 1..=500u32 {
                    writer.transfer_to(pid).unwrap();
                }
            });
            scope.spawn(|| {
                for _ in 0..500 {
                    let keys = installed_app_keys_with_liveness(dir.path(), exe_key, |_| true)
                        .expect("complete inventory");
                    assert!(keys.contains(&key), "reader observed a partial PID write");
                }
            });
        });
    }

    #[cfg(windows)]
    #[test]
    fn handoff_transfer_waits_for_the_inventory_lock_before_replacing_owner() {
        let dir = FixtureDir::new();
        let sidecar = dir.path().join("AeroFTP.exe.locked01.new");
        std::fs::write(&sidecar, b"exe B").unwrap();
        let key = exe_key(&sidecar).unwrap();
        let reservation = reserve_update_handoff(dir.path(), "locked01").unwrap();
        let inventory_lock = lock_batches(dir.path()).unwrap();
        let error = reservation
            .transfer_to_within(4242, Some(Duration::from_millis(50)))
            .expect_err("transfer must wait for C's inventory lock");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        let keys = installed_app_keys_with_liveness(dir.path(), exe_key, |_| true)
            .expect("complete inventory");
        assert!(keys.contains(&key), "B remains live during C's inventory");
        drop(inventory_lock);
        reservation.transfer_to(4242).unwrap();
        let keys = installed_app_keys_with_liveness(dir.path(), exe_key, |_| true)
            .expect("complete inventory after transfer");
        assert!(keys.contains(&key));
    }

    #[test]
    fn marker_directory_conflict_keeps_owned_staged_marker_and_recovers_after_clearing() {
        let dir = FixtureDir::new();
        let src = tempfile::tempdir().unwrap();
        let exe = src.path().join("AeroFTP.exe");
        let marker = src.path().join("portable.marker");
        std::fs::write(&exe, b"portable exe").unwrap();
        std::fs::write(&marker, b"portable").unwrap();
        let key = exe_key(&exe).unwrap();
        stage_pending_docs(dir.path(), &exe, &[(&marker, "portable.marker")], "start").unwrap();
        let target = dir.path().join("portable.marker");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("sentinel"), b"untouched").unwrap();
        assert_eq!(
            install_pending_docs(dir.path(), &key, rename),
            Pending::Docs(vec![("portable.marker", Outcome::Directory)])
        );
        assert!(dir
            .path()
            .join(PENDING_DIR)
            .join("portable.marker")
            .is_file());
        assert_eq!(read(&target, "sentinel"), "untouched");
        assert_eq!(
            probe_portable_marker_within(dir.path(), || Some(key.clone()), None),
            PortableMarkerState::Present
        );

        std::fs::remove_dir_all(&target).unwrap();
        assert_eq!(
            install_pending_docs(dir.path(), &key, rename),
            Pending::Docs(vec![("portable.marker", Outcome::Installed)])
        );
        assert!(target.is_file());
        assert_eq!(
            probe_portable_marker_within(dir.path(), || Some(key), None),
            PortableMarkerState::Present
        );
    }

    #[cfg(unix)]
    #[test]
    fn probe_portable_marker_follows_symlink_to_regular_file_without_pending_batch() {
        let dir = FixtureDir::new();
        let target = dir.path().join("actual-marker");
        std::fs::write(&target, b"portable").unwrap();
        std::os::unix::fs::symlink(&target, dir.path().join("portable.marker")).unwrap();
        assert_eq!(
            probe_portable_marker_within(dir.path(), || None, Some(Duration::from_secs(1))),
            PortableMarkerState::Present
        );
    }

    #[test]
    fn a_held_document_keeps_retrying_when_another_staging_moves_its_batch_after_the_pass() {
        let dir = FixtureDir::new();
        let src = tempfile::tempdir().unwrap();
        let exe_b = src.path().join("b.exe");
        let exe_c = src.path().join("c.exe");
        let notice_b = src.path().join("notices-b.md");
        let marker_c = src.path().join("marker-c");
        std::fs::write(&exe_b, b"exe B").unwrap();
        std::fs::write(&exe_c, b"exe C").unwrap();
        std::fs::write(&notice_b, "notices of B").unwrap();
        std::fs::write(&marker_c, "portable").unwrap();
        std::fs::write(dir.path().join("THIRD-PARTY-NOTICES.md"), "old notices").unwrap();
        let key_b = exe_key(&exe_b).unwrap();

        // Keep an older exe's owned PENDING_DIR holding a file so B installs
        // directly from NEXT_DIR (cannot promote over occupied PENDING_DIR).
        let old_pending = dir.path().join(PENDING_DIR);
        std::fs::create_dir(&old_pending).unwrap();
        let old_id = format!("{:032x}", rand::random::<u128>());
        std::fs::write(old_pending.join(KEY_FILE), "older-exe").unwrap();
        std::fs::write(old_pending.join(STAGING_ID_FILE), &old_id).unwrap();
        record_batch(dir.path(), &old_id).unwrap();
        std::fs::create_dir(old_pending.join("sub-keep")).unwrap();

        stage_pending_docs(
            dir.path(),
            &exe_b,
            &[(&notice_b, "THIRD-PARTY-NOTICES.md")],
            "b",
        )
        .unwrap();

        let busy = RefCell::new(vec!["THIRD-PARTY-NOTICES.md"]);
        let first = install_pending_docs(dir.path(), &key_b, held(&busy, ERROR_SHARING_VIOLATION));
        assert_eq!(
            first,
            Pending::Docs(vec![("THIRD-PARTY-NOTICES.md", Outcome::InUse)])
        );

        // Immediately after retry pass 1 releases the batch mutex and BEFORE
        // `retry_held` runs `last.retain` bookkeeping, a concurrent staging C
        // moves B's batch from NEXT_DIR to PREV_DIR and publishes C in NEXT_DIR
        // with only `portable.marker`. Because bookkeeping uses the outcomes
        // observed under the lock instead of re-probing NEXT_DIR, B's held
        // result stays retryable and installs once the editor hold clears.
        let moved = Cell::new(false);
        let sleep_step = Cell::new(0u32);
        let outcome = retry_held_with_hook(
            dir.path(),
            &key_b,
            first,
            (4, Duration::ZERO),
            held(&busy, ERROR_SHARING_VIOLATION),
            |_| {
                let s = sleep_step.get() + 1;
                sleep_step.set(s);
                if s == 2 {
                    busy.borrow_mut().clear();
                }
            },
            || {
                if !moved.replace(true) {
                    stage_pending_docs(dir.path(), &exe_c, &[(&marker_c, "portable.marker")], "c")
                        .unwrap();
                    assert!(dir
                        .path()
                        .join(PREV_DIR)
                        .join("THIRD-PARTY-NOTICES.md")
                        .is_file());
                    assert!(!dir
                        .path()
                        .join(NEXT_DIR)
                        .join("THIRD-PARTY-NOTICES.md")
                        .exists());
                }
            },
        );
        assert!(moved.get());
        assert_eq!(
            outcome,
            Pending::Docs(vec![("THIRD-PARTY-NOTICES.md", Outcome::Installed)])
        );
        assert_eq!(read(dir.path(), "THIRD-PARTY-NOTICES.md"), "notices of B");
    }

    #[test]
    fn folder_id_and_batch_lock_share_namespace_across_symlink_or_junction() {
        let root = tempfile::tempdir().unwrap();
        let real = root.path().join("real-app");
        let alias = root.path().join("alias-app");
        std::fs::create_dir(&real).unwrap();
        let _guard = FolderArtifactsGuard::new(&real);

        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        #[cfg(windows)]
        {
            let status = std::process::Command::new("cmd.exe")
                .args(["/D", "/C", "mklink", "/J"])
                .arg(&alias)
                .arg(&real)
                .status()
                .expect("mklink /J");
            assert!(status.success(), "mklink /J must succeed");
        }

        assert_eq!(
            folder_id(&alias),
            folder_id(&real),
            "junction/symlink and real folder must derive the same folder_id"
        );

        let (locked_tx, locked_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let alias_for_holder = alias.clone();
        let holder = std::thread::spawn(move || {
            let _lock = lock_batches(&alias_for_holder).expect("lock through alias");
            locked_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });
        locked_rx.recv().unwrap();

        let err = lock_batches_within(&real, Some(Duration::from_millis(60)))
            .err()
            .expect("real path must see alias lock as held");
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);

        let acquired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let acquired_clone = std::sync::Arc::clone(&acquired);
        let real_for_waiter = real.clone();
        let waiter = std::thread::spawn(move || {
            let _lock = lock_batches(&real_for_waiter).expect("lock through real path");
            acquired_clone.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        std::thread::sleep(Duration::from_millis(50));
        assert!(
            !acquired.load(std::sync::atomic::Ordering::SeqCst),
            "second lock acquisition must wait while alias holds the lock"
        );
        release_tx.send(()).unwrap();
        holder.join().unwrap();
        waiter.join().unwrap();
        assert!(acquired.load(std::sync::atomic::Ordering::SeqCst));

        // Mirror-only recovery across alias and real path: stage through alias,
        // delete the durable ledger in `exe_dir`, and verify the real path still
        // recognizes and installs the batch via the shared canonical `.batches` mirror.
        let src = tempfile::tempdir().unwrap();
        let exe = src.path().join("AeroFTP.exe");
        let notices = src.path().join("THIRD-PARTY-NOTICES.md");
        std::fs::write(&exe, b"alias-recovery-exe").unwrap();
        std::fs::write(&notices, "notices via alias").unwrap();
        let key = exe_key(&exe).unwrap();
        stage_pending_docs(
            &alias,
            &exe,
            &[(&notices, "THIRD-PARTY-NOTICES.md")],
            "alias-mirror",
        )
        .unwrap();
        assert_eq!(temp_batch_record(&alias), temp_batch_record(&real));
        assert_eq!(staging_record(&alias), staging_record(&real));
        std::fs::remove_file(owned_batch_record(&real)).unwrap();
        assert!(is_owned_batch(&real, &real.join(NEXT_DIR)));
        assert_eq!(
            install_pending_docs(&real, &key, rename),
            Pending::Docs(vec![("THIRD-PARTY-NOTICES.md", Outcome::Installed)])
        );
        assert_eq!(read(&real, "THIRD-PARTY-NOTICES.md"), "notices via alias");

        // Remove the alias before `root` drops so Windows cleans up the junction cleanly.
        #[cfg(windows)]
        let _ = std::fs::remove_dir(&alias);
        #[cfg(unix)]
        let _ = std::fs::remove_file(&alias);
    }

    #[test]
    fn timed_out_discard_persists_token_cancellation_resolved_on_next_pass() {
        let dir = FixtureDir::new();
        let src = tempfile::tempdir().unwrap();
        let exe_a = src.path().join("exe-a.exe");
        let exe_b = src.path().join("exe-b.exe");
        let notice_a = src.path().join("a.md");
        let notice_b = src.path().join("b.md");
        std::fs::write(&exe_a, b"build-A").unwrap();
        std::fs::write(&exe_b, b"build-B").unwrap();
        std::fs::write(&notice_a, "notices A").unwrap();
        std::fs::write(&notice_b, "notices B").unwrap();
        let key_a = exe_key(&exe_a).unwrap();
        let key_b = exe_key(&exe_b).unwrap();

        // Stage A's live batch, then stage B (which sets A aside in PREV_DIR).
        stage_pending_docs(
            dir.path(),
            &exe_a,
            &[(&notice_a, "THIRD-PARTY-NOTICES.md")],
            "a",
        )
        .unwrap();
        let token_b = stage_pending_docs(
            dir.path(),
            &exe_b,
            &[(&notice_b, "THIRD-PARTY-NOTICES.md")],
            "b",
        )
        .unwrap()
        .unwrap();
        assert!(dir.path().join(PREV_DIR).is_dir());

        // Hold batch lock on another thread while `discard_pending_docs_within`
        // times out: it must persist the token-scoped cancellation marker.
        let exe_dir = dir.path().to_path_buf();
        let (locked_tx, locked_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let holder = std::thread::spawn(move || {
            let _lock = lock_batches(&exe_dir).unwrap();
            locked_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });
        locked_rx.recv().unwrap();

        assert!(!discard_pending_docs_within(
            dir.path(),
            &token_b,
            Some(Duration::from_millis(50))
        ));
        assert!(
            cancel_marker_path(dir.path(), &token_b).is_file(),
            "timed-out discard must persist a token-scoped cancellation marker"
        );
        release_tx.send(()).unwrap();
        holder.join().unwrap();

        // Even if the next launch runs with exe B's key, `install_pending_docs`
        // applies the persisted cancellation BEFORE installing: B's failed batch
        // is discarded (never installed!), A's set-aside batch is restored to
        // NEXT_DIR, and the cancel marker is removed.
        assert_eq!(
            install_pending_docs(dir.path(), &key_b, rename),
            Pending::None
        );
        assert!(!dir.path().join("THIRD-PARTY-NOTICES.md").exists());
        assert!(!cancel_marker_path(dir.path(), &token_b).exists());
        assert_eq!(read(&dir.path().join(NEXT_DIR), KEY_FILE), key_a);
        // A can now install its restored batch cleanly.
        assert_eq!(
            install_pending_docs(dir.path(), &key_a, rename),
            Pending::Docs(vec![("THIRD-PARTY-NOTICES.md", Outcome::Installed)])
        );
        assert_eq!(read(dir.path(), "THIRD-PARTY-NOTICES.md"), "notices A");

        // Incomplete cancellation 1: temporarily unreadable `staging.id` keeps
        // the cancellation marker active and blocks install/probe until readable.
        let dir2 = FixtureDir::new();
        let token_b2 = stage_pending_docs(
            dir2.path(),
            &exe_b,
            &[(&notice_b, "THIRD-PARTY-NOTICES.md")],
            "b2",
        )
        .unwrap()
        .unwrap();
        record_cancelled_batch(dir2.path(), &token_b2).unwrap();
        let blocked = install_pass_with(dir2.path(), &key_b, None, rename, |_| {
            Err(io::Error::new(io::ErrorKind::PermissionDenied, "locked id"))
        });
        assert_eq!(blocked, Pending::None);
        assert!(
            cancel_marker_path(dir2.path(), &token_b2).is_file(),
            "unreadable staging.id must keep cancel marker active"
        );
        assert!(!dir2.path().join("THIRD-PARTY-NOTICES.md").exists());
        assert_eq!(
            probe_portable_marker_with_read_id(
                dir2.path(),
                || Some(key_b.clone()),
                None,
                |_| Err(io::Error::new(io::ErrorKind::PermissionDenied, "locked id")),
            ),
            PortableMarkerState::Pending
        );
        // Once readable again, the cancellation finishes and B is not installed.
        assert_eq!(
            install_pending_docs(dir2.path(), &key_b, rename),
            Pending::None
        );
        assert!(!cancel_marker_path(dir2.path(), &token_b2).exists());
        assert!(!dir2.path().join(NEXT_DIR).exists());

        // Incomplete cancellation 2: a pending document slot that cannot be
        // removed yet (simulated by a directory at the named document path)
        // keeps `STAGING_ID_FILE` and the cancel marker active until cleared,
        // preventing install and notices lookup on the cancelled batch.
        let dir3 = FixtureDir::new();
        let token_b3 = stage_pending_docs(
            dir3.path(),
            &exe_b,
            &[(&notice_b, "THIRD-PARTY-NOTICES.md")],
            "b3",
        )
        .unwrap()
        .unwrap();
        let next3 = dir3.path().join(NEXT_DIR);
        let blocker = next3.join("THIRD-PARTY-NOTICES.md");
        std::fs::remove_file(&blocker).unwrap();
        std::fs::create_dir(&blocker).unwrap();
        record_cancelled_batch(dir3.path(), &token_b3).unwrap();
        assert_eq!(
            install_pending_docs(dir3.path(), &key_b, rename),
            Pending::None
        );
        assert!(
            cancel_marker_path(dir3.path(), &token_b3).is_file(),
            "partial cleanup must keep cancel marker active"
        );
        assert_eq!(
            own_pending_notices(dir3.path(), || Some(key_b.clone())),
            None
        );
        // Removing the obstruction allows the next pass to finish cancellation.
        std::fs::remove_dir(&blocker).unwrap();
        assert_eq!(
            install_pending_docs(dir3.path(), &key_b, rename),
            Pending::None
        );
        assert!(!cancel_marker_path(dir3.path(), &token_b3).exists());
        assert!(!next3.exists());
    }

    #[test]
    fn cancellation_with_unreadable_identity_keeps_probe_pending_until_token_is_known() {
        let dir = FixtureDir::new();
        let src = tempfile::tempdir().unwrap();
        let exe = src.path().join("AeroFTP.exe");
        let marker = src.path().join("portable.marker");
        std::fs::write(&exe, b"exe B").unwrap();
        std::fs::write(&marker, b"portable").unwrap();
        let key_b = exe_key(&exe).unwrap();
        let token_b = stage_pending_docs(dir.path(), &exe, &[(&marker, "portable.marker")], "b")
            .unwrap()
            .unwrap();
        let token_a = "00".repeat(16);
        assert_ne!(token_a, token_b);
        record_cancelled_batch(dir.path(), &token_a).unwrap();
        assert_eq!(
            probe_portable_marker_with_read_id(
                dir.path(),
                || Some(key_b.clone()),
                None,
                |_| Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "unreadable id"
                )),
            ),
            PortableMarkerState::Pending
        );
        assert!(cancel_marker_path(dir.path(), &token_a).is_file());
        assert_eq!(
            probe_portable_marker_within(dir.path(), || Some(key_b.clone()), None),
            PortableMarkerState::Present,
            "once readable, token A does not cancel B's owned marker"
        );

        record_cancelled_batch(dir.path(), &token_b).unwrap();
        assert_eq!(
            probe_portable_marker_within(dir.path(), || Some(key_b), None),
            PortableMarkerState::Absent,
            "confirmed cancellation of B removes the staged marker"
        );
    }

    #[test]
    fn discard_preserves_batch_when_cancellation_marker_cannot_be_written() {
        let dir = FixtureDir::new();
        let (_stage, exe, _key, readme) = new_exe(b"B");
        let token = stage_pending_docs(dir.path(), &exe, &[(&readme, "README.txt")], "b")
            .unwrap()
            .unwrap();
        let marker = cancel_marker_path(dir.path(), &token);
        std::fs::create_dir(&marker).unwrap();
        assert!(!discard_pending_docs_within(
            dir.path(),
            &token,
            Some(Duration::from_secs(1))
        ));
        assert!(dir.path().join(NEXT_DIR).join("README.txt").is_file());

        std::fs::remove_dir(&marker).unwrap();
        assert!(discard_pending_docs_within(
            dir.path(),
            &token,
            Some(Duration::from_secs(1))
        ));
        assert!(!dir.path().join(NEXT_DIR).exists());
    }

    #[test]
    fn unfinished_cancellation_defers_only_next_and_lets_set_aside_owner_recover() {
        let dir = FixtureDir::new();
        let (_stage_a, exe_a, key_a, notice_a) = new_exe(b"A");
        let (_stage_b, exe_b, key_b, notice_b) = new_exe(b"B");
        stage_pending_docs(
            dir.path(),
            &exe_a,
            &[(&notice_a, "THIRD-PARTY-NOTICES.md")],
            "a",
        )
        .unwrap();
        let token_b = stage_pending_docs(
            dir.path(),
            &exe_b,
            &[(&notice_b, "THIRD-PARTY-NOTICES.md")],
            "b",
        )
        .unwrap()
        .unwrap();
        let next = dir.path().join(NEXT_DIR);
        let blocker = next.join("THIRD-PARTY-NOTICES.md");
        std::fs::remove_file(&blocker).unwrap();
        std::fs::create_dir(&blocker).unwrap();
        record_cancelled_batch(dir.path(), &token_b).unwrap();

        let offered = own_pending_notices(dir.path(), || Some(key_a.clone()))
            .expect("A's unaffected set-aside notices remain readable");
        assert_eq!(std::fs::read_to_string(offered).unwrap(), "readme of A");
        assert_eq!(
            install_pending_docs(dir.path(), &key_a, rename),
            Pending::Docs(vec![("THIRD-PARTY-NOTICES.md", Outcome::Installed)])
        );
        assert_eq!(read(dir.path(), "THIRD-PARTY-NOTICES.md"), "readme of A");
        assert!(cancel_marker_path(dir.path(), &token_b).is_file());
        assert!(next.is_dir());

        std::fs::remove_dir(&blocker).unwrap();
        assert_eq!(
            install_pending_docs(dir.path(), &key_b, rename),
            Pending::None
        );
        assert!(!next.exists());
        assert!(!cancel_marker_path(dir.path(), &token_b).exists());
    }

    #[test]
    fn ledger_registration_failure_restores_set_aside_batch_to_next_dir() {
        let dir = FixtureDir::new();
        let src = tempfile::tempdir().unwrap();
        let exe_b = src.path().join("exe-b.exe");
        let notice_orig = src.path().join("orig.md");
        let notice_new = src.path().join("new.md");
        std::fs::write(&exe_b, b"build-B-ledger-rb").unwrap();
        std::fs::copy(&exe_b, dir.path().join("AeroFTP.exe")).unwrap();
        std::fs::write(&notice_orig, "original notices").unwrap();
        std::fs::write(&notice_new, "replacement notices").unwrap();

        stage_pending_docs(
            dir.path(),
            &exe_b,
            &[(&notice_orig, "THIRD-PARTY-NOTICES.md")],
            "orig",
        )
        .unwrap();

        // Inject a failure in `record` AFTER `make_room` has moved B's live
        // batch from NEXT_DIR to PREV_DIR: rollback must restore B's batch to
        // NEXT_DIR without calling `publish`.
        let published_called = Cell::new(false);
        let err = stage_with_commit(
            dir.path(),
            &exe_b,
            &[(&notice_new, "THIRD-PARTY-NOTICES.md")],
            "fail-ledger",
            None,
            |_, _| Err(io::Error::other("injected ledger write failure")),
            |_, _| {
                published_called.set(true);
                Ok(())
            },
        )
        .expect_err("failed ledger registration must return error");
        assert!(err.contains("injected ledger write failure"), "{err}");
        assert!(
            !published_called.get(),
            "publish must not run when ledger registration fails"
        );
        assert!(
            !dir.path().join(PREV_DIR).exists(),
            "rollback must restore set-aside batch from PREV_DIR to NEXT_DIR"
        );
        assert_eq!(
            read(&dir.path().join(NEXT_DIR), "THIRD-PARTY-NOTICES.md"),
            "original notices"
        );
    }

    #[test]
    fn fixture_dir_drop_removes_temp_artifacts() {
        let src = tempfile::tempdir().unwrap();
        let exe = src.path().join("AeroFTP.exe");
        let notices = src.path().join("THIRD-PARTY-NOTICES.md");
        std::fs::write(&exe, b"cleanup-exe").unwrap();
        std::fs::write(&notices, "cleanup notices").unwrap();
        let key = exe_key(&exe).unwrap();

        let (batches_path, snapshot_path, id);
        {
            let dir = FixtureDir::new();
            id = folder_id(dir.path());
            batches_path = temp_batch_record(dir.path());
            stage_pending_docs(
                dir.path(),
                &exe,
                &[(&notices, "THIRD-PARTY-NOTICES.md")],
                "cleanup",
            )
            .unwrap();
            assert!(batches_path.is_file());
            snapshot_path =
                own_pending_notices(dir.path(), || Some(key.clone())).expect("snapshot");
            assert!(snapshot_path.is_file());
        }
        assert!(
            !batches_path.exists(),
            "FixtureDir::drop must remove .batches mirror"
        );
        assert!(
            !snapshot_path.exists(),
            "FixtureDir::drop must remove About notice snapshot"
        );
        #[cfg(not(windows))]
        assert!(
            !std::env::temp_dir()
                .join(format!("aeroftp-pending-docs-{id}.lock"))
                .exists(),
            "FixtureDir::drop must remove Unix lock file"
        );
        let _ = id;
    }

    #[test]
    fn same_exe_restage_keeps_live_batch_recoverable_on_publish_failure_and_replaces_on_retry() {
        let dir = FixtureDir::new();
        let src = tempfile::tempdir().unwrap();
        let exe_b = src.path().join("AeroFTP.exe");
        std::fs::write(&exe_b, b"build-B-exe").unwrap();
        // Install build B in `exe_dir` so it is a live installed executable.
        std::fs::copy(&exe_b, dir.path().join("AeroFTP.exe")).unwrap();
        let key_b = exe_key(&exe_b).unwrap();
        let orig_notices = src.path().join("orig.md");
        let fixed_notices = src.path().join("fixed.md");
        std::fs::write(&orig_notices, "original notices of B").unwrap();
        std::fs::write(&fixed_notices, "corrected notices of B").unwrap();

        stage_pending_docs(
            dir.path(),
            &exe_b,
            &[(&orig_notices, "THIRD-PARTY-NOTICES.md")],
            "b-orig",
        )
        .unwrap();

        // Same-exe restage with corrected notices rejects byte-for-byte reuse,
        // and publication fails: B's live original batch must be restored.
        let err = stage_with(
            dir.path(),
            &exe_b,
            &[(&fixed_notices, "THIRD-PARTY-NOTICES.md")],
            "b-fail",
            |_, _| {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "injected publish error",
                ))
            },
        )
        .expect_err("failed publish must return error");
        assert!(err.contains("injected publish error"), "{err}");
        assert!(
            !dir.path().join(PREV_DIR).exists(),
            "failed same-key restage must restore NEXT_DIR without leaving PREV_DIR behind"
        );
        assert_eq!(
            read(&dir.path().join(NEXT_DIR), "THIRD-PARTY-NOTICES.md"),
            "original notices of B"
        );
        // Verify B can still install its original pending notices when editor closes.
        assert_eq!(
            install_pending_docs(dir.path(), &key_b, rename),
            Pending::Docs(vec![("THIRD-PARTY-NOTICES.md", Outcome::Installed)])
        );
        assert_eq!(
            read(dir.path(), "THIRD-PARTY-NOTICES.md"),
            "original notices of B"
        );

        // A helper-handoff failure after publication discards the replacement
        // and restores the original batch, which was not retired prematurely.
        stage_pending_docs(
            dir.path(),
            &exe_b,
            &[(&orig_notices, "THIRD-PARTY-NOTICES.md")],
            "b-orig-2",
        )
        .unwrap();
        let fixed_token = stage_pending_docs(
            dir.path(),
            &exe_b,
            &[(&fixed_notices, "THIRD-PARTY-NOTICES.md")],
            "b-fixed-fails-handoff",
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            read(&dir.path().join(PREV_DIR), "THIRD-PARTY-NOTICES.md"),
            "original notices of B"
        );
        discard_pending_docs(dir.path(), &fixed_token);
        assert_eq!(
            read(&dir.path().join(NEXT_DIR), "THIRD-PARTY-NOTICES.md"),
            "original notices of B"
        );
        assert_eq!(
            install_pending_docs(dir.path(), &key_b, rename),
            Pending::Docs(vec![("THIRD-PARTY-NOTICES.md", Outcome::Installed)])
        );

        // Control: a successful same-key update installs the corrected notices
        // and only then retires the superseded PREV_DIR batch.
        stage_pending_docs(
            dir.path(),
            &exe_b,
            &[(&orig_notices, "THIRD-PARTY-NOTICES.md")],
            "b-orig-3",
        )
        .unwrap();
        stage_pending_docs(
            dir.path(),
            &exe_b,
            &[(&fixed_notices, "THIRD-PARTY-NOTICES.md")],
            "b-fixed",
        )
        .unwrap();
        assert!(dir.path().join(PREV_DIR).is_dir());
        assert_eq!(
            install_pending_docs(dir.path(), &key_b, rename),
            Pending::Docs(vec![("THIRD-PARTY-NOTICES.md", Outcome::Installed)])
        );
        assert_eq!(
            read(dir.path(), "THIRD-PARTY-NOTICES.md"),
            "corrected notices of B"
        );
        assert!(!dir.path().join(PREV_DIR).exists());
    }

    #[test]
    fn promotion_does_not_overwrite_empty_unowned_pending_dir() {
        let dir = FixtureDir::new();
        let src = tempfile::tempdir().unwrap();
        let exe_b = src.path().join("AeroFTP.exe");
        let notices_b = src.path().join("THIRD-PARTY-NOTICES.md");
        std::fs::write(&exe_b, b"build-B-promo").unwrap();
        std::fs::write(&notices_b, "notices of B").unwrap();
        let key_b = exe_key(&exe_b).unwrap();

        // User's empty unowned directory at PENDING_DIR.
        let pending = dir.path().join(PENDING_DIR);
        std::fs::create_dir(&pending).unwrap();

        stage_pending_docs(
            dir.path(),
            &exe_b,
            &[(&notices_b, "THIRD-PARTY-NOTICES.md")],
            "b-promo",
        )
        .unwrap();

        assert_eq!(
            install_pending_docs(dir.path(), &key_b, rename),
            Pending::Docs(vec![("THIRD-PARTY-NOTICES.md", Outcome::Installed)])
        );
        assert_eq!(read(dir.path(), "THIRD-PARTY-NOTICES.md"), "notices of B");
        assert!(
            pending.is_dir(),
            "empty unowned PENDING_DIR must survive promotion and cleanup"
        );
        assert!(
            !dir.path().join(NEXT_DIR).exists(),
            "NEXT_DIR was installed directly and cleaned up"
        );

        // Control: when PENDING_DIR is free, promotion moves NEXT_DIR into
        // PENDING_DIR (keeping held documents in PENDING_DIR).
        std::fs::remove_dir(&pending).unwrap();
        stage_pending_docs(
            dir.path(),
            &exe_b,
            &[(&notices_b, "THIRD-PARTY-NOTICES.md")],
            "b-promo-free",
        )
        .unwrap();
        let busy = RefCell::new(vec!["THIRD-PARTY-NOTICES.md"]);
        assert_eq!(
            install_pending_docs(dir.path(), &key_b, held(&busy, ERROR_SHARING_VIOLATION)),
            Pending::Docs(vec![("THIRD-PARTY-NOTICES.md", Outcome::InUse)])
        );
        assert!(
            pending.join("THIRD-PARTY-NOTICES.md").is_file(),
            "free PENDING_DIR slot must permit promotion from NEXT_DIR"
        );
        assert!(!dir.path().join(NEXT_DIR).exists());

        // Race fallback: simulate an empty unowned PENDING_DIR appearing AFTER
        // the metadata check right before `rename_no_replace` executes. The
        // no-replace rename must refuse to overwrite it and `install_pass_with_promote`
        // must fall back to installing directly from NEXT_DIR while leaving the
        // newly created empty PENDING_DIR intact.
        let dir2 = FixtureDir::new();
        let pending2 = dir2.path().join(PENDING_DIR);
        stage_pending_docs(
            dir2.path(),
            &exe_b,
            &[(&notices_b, "THIRD-PARTY-NOTICES.md")],
            "b-promo-race",
        )
        .unwrap();
        let raced = install_pass_with_promote(
            dir2.path(),
            &key_b,
            None,
            rename,
            |p| std::fs::read_to_string(p),
            |from, to| {
                // Destination appears after the caller's metadata check.
                std::fs::create_dir(to)?;
                rename_no_replace(from, to)
            },
        );
        assert_eq!(
            raced,
            Pending::Docs(vec![("THIRD-PARTY-NOTICES.md", Outcome::Installed)])
        );
        assert_eq!(read(dir2.path(), "THIRD-PARTY-NOTICES.md"), "notices of B");
        assert!(
            pending2.is_dir(),
            "raced empty unowned PENDING_DIR must not be overwritten by rename_no_replace"
        );
        assert!(!dir2.path().join(NEXT_DIR).exists());
    }

    #[test]
    fn retry_held_tracks_newly_observed_held_documents_added_by_restage() {
        let dir = FixtureDir::new();
        let src = tempfile::tempdir().unwrap();
        let exe_b = src.path().join("AeroFTP.exe");
        let readme_b = src.path().join("README.txt");
        let notices_b = src.path().join("THIRD-PARTY-NOTICES.md");
        std::fs::write(&exe_b, b"build-B-retry").unwrap();
        std::fs::write(&readme_b, "readme of B").unwrap();
        std::fs::write(&notices_b, "notices of B").unwrap();
        std::fs::copy(&exe_b, dir.path().join("AeroFTP.exe")).unwrap();
        let key_b = exe_key(&exe_b).unwrap();

        // First pass: only README.txt is staged and held.
        stage_pending_docs(dir.path(), &exe_b, &[(&readme_b, "README.txt")], "b1").unwrap();
        let busy = RefCell::new(vec!["README.txt"]);
        let first = install_pending_docs(dir.path(), &key_b, held(&busy, ERROR_SHARING_VIOLATION));
        assert_eq!(first, Pending::Docs(vec![("README.txt", Outcome::InUse)]));

        // Before retry round 1, a same-exe restage adds THIRD-PARTY-NOTICES.md.
        // On round 1 README installs, while THIRD-PARTY-NOTICES.md is held.
        // On round 2 the hold clears and THIRD-PARTY-NOTICES.md installs.
        stage_pending_docs(
            dir.path(),
            &exe_b,
            &[
                (&readme_b, "README.txt"),
                (&notices_b, "THIRD-PARTY-NOTICES.md"),
            ],
            "b2",
        )
        .unwrap();
        let round = Cell::new(0u32);
        let outcome = retry_held(
            dir.path(),
            &key_b,
            first,
            4,
            Duration::ZERO,
            held(&busy, ERROR_SHARING_VIOLATION),
            |_| {
                let r = round.get() + 1;
                round.set(r);
                if r == 1 {
                    *busy.borrow_mut() = vec!["THIRD-PARTY-NOTICES.md"];
                } else if r == 2 {
                    busy.borrow_mut().clear();
                }
            },
        );
        assert_eq!(
            round.get(),
            2,
            "worker must retry for newly observed held notices and stop once installed"
        );
        assert_eq!(
            outcome,
            Pending::Docs(vec![
                ("README.txt", Outcome::Installed),
                ("THIRD-PARTY-NOTICES.md", Outcome::Installed),
            ])
        );
        assert_eq!(read(dir.path(), "README.txt"), "readme of B");
        assert_eq!(read(dir.path(), "THIRD-PARTY-NOTICES.md"), "notices of B");

        // Control: when another pass finishes all pending work, worker stops immediately.
        stage_pending_docs(dir.path(), &exe_b, &[(&readme_b, "README.txt")], "b3").unwrap();
        *busy.borrow_mut() = vec!["README.txt"];
        let first2 = install_pending_docs(dir.path(), &key_b, held(&busy, ERROR_SHARING_VIOLATION));
        busy.borrow_mut().clear();
        let rounds_ctrl = Cell::new(0u32);
        let outcome2 = retry_held(
            dir.path(),
            &key_b,
            first2,
            4,
            Duration::ZERO,
            rename,
            |_| {
                rounds_ctrl.set(rounds_ctrl.get() + 1);
            },
        );
        assert_eq!(rounds_ctrl.get(), 1);
        assert_eq!(
            outcome2,
            Pending::Docs(vec![("README.txt", Outcome::Installed)])
        );
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
