//! Size of a local folder: the bytes, files and subfolders under it.
//!
//! One engine for every surface that sizes a local folder: the Properties
//! dialog in the app, `aeroftp-cli size <folder>`, `Ctrl+S` in the TUI, and
//! AeroAgent's `local_disk_usage`. It is measured the way a file
//! manager is expected to measure it, and as fast as the disk will answer:
//!
//! - **In parallel.** A small pool of threads lists directories from one shared
//!   queue. Listing a directory is a round trip to the filesystem: with one
//!   thread the disk sits idle between round trips, with several it always has
//!   the next one waiting. Each thread keeps the subfolders it finds and walks
//!   them itself, and hands half of them over only when another thread has run
//!   out, so the queue lock is taken when there is work to balance and not once
//!   per directory.
//! - **One metadata read per file, and none at all on Windows.** The size comes
//!   from the directory entry. On Windows the listing already carries it; on
//!   Linux it is one `statx` relative to the open directory, so no path is
//!   resolved again from the root for every file (which is what a walk that
//!   stats `entry.path()` pays).
//! - **Each file once.** On Unix a file with several hard links is counted at
//!   the first name the walk meets, and a directory reached twice (a bind
//!   mount, a macOS firmlink) is walked once: the total `du` gives. Windows has
//!   no inode in a directory listing, and reading the file index would cost an
//!   open per file, the very syscall the listing saves; there a hard-linked
//!   file counts at each name, as Explorer counts it. Symbolic links and
//!   Windows junctions are not followed on any platform, so neither can loop,
//!   nor, on Linux, one put in a folder's place while the walk runs
//!   ([`read_admitted`] says what other platforms check).
//! - **Only stored data.** On Linux, a kernel pseudo-filesystem met on the way
//!   (`/proc`, `/sys`, ...) is not entered: its files are generated when read,
//!   and `/proc/kcore` alone would add the size of the address space to "/".
//!   The test is the filesystem's `statfs` magic, asked only where the walk
//!   crosses into another device, so it needs no mount table (a snap without
//!   `mount-observe` cannot read one) and costs nothing on a single disk.
//!   A folder that is itself on one of them holds nothing stored, and sizes
//!   as empty. macOS and Windows mount nothing of the kind inside a folder a
//!   user sizes.
//! - **Honest when incomplete.** An entry that cannot be read, a cap, or a
//!   cancel sets `partial`, and the counts are then lower bounds.
//! - **Stoppable.** The caller owns a flag, and every thread checks it before
//!   every entry, so closing the dialog does not leave a walk of a whole disk
//!   running behind it, even on a network share where each entry is slow.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use serde::Serialize;
#[cfg(unix)]
use std::collections::HashSet;
use std::collections::{HashMap, VecDeque};
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, LazyLock, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// Totals of a folder, or of the part of it walked so far.
#[derive(Serialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct FolderSizeResult {
    pub total_bytes: u64,
    pub file_count: u64,
    pub dir_count: u64,
    /// The walk did not cover the whole folder (one of the three reasons
    /// below); the counts are then lower bounds.
    pub partial: bool,
    /// Folders the walk could not list, wholly or in part.
    pub unreadable_dirs: u64,
    /// Entries the walk could not size: a file it could not stat, counted in
    /// `file_count` but not in `total_bytes`, or an entry whose type could not
    /// be read, counted in neither.
    pub unreadable_files: u64,
    /// The entry cap or the depth cap left part of the folder out.
    pub capped: bool,
    /// The cancel flag stopped the walk.
    pub cancelled: bool,
}

impl FolderSizeResult {
    /// Why the totals are a lower bound, in a sentence a terminal can show;
    /// `None` when they are the folder's size.
    pub fn lower_bound_reason(&self) -> Option<String> {
        let mut why = Vec::new();
        if self.unreadable_dirs > 0 {
            why.push(format!(
                "{} folder{} could not be read",
                self.unreadable_dirs,
                if self.unreadable_dirs == 1 { "" } else { "s" }
            ));
        }
        if self.unreadable_files > 0 {
            why.push(format!(
                "{} file{} could not be sized",
                self.unreadable_files,
                if self.unreadable_files == 1 { "" } else { "s" }
            ));
        }
        if self.capped {
            why.push("a scan limit was reached".to_string());
        }
        if self.cancelled {
            why.push("the scan was cancelled".to_string());
        }
        (!why.is_empty()).then(|| format!("lower bound: {}", why.join(", ")))
    }
}

/// Entries a walk visits before it stops and reports a partial result.
///
/// It bounds memory as well as time. A walk keeps one key in [`Seen`] per
/// directory and per hard-linked file, on Unix only, about 20 to 40 bytes each
/// with the hash table's slack, plus the paths of directories found and not
/// yet listed; nothing per plain file. Measured on a Linux system drive of
/// 1,644,000 entries (199,000 directories, 149,000 hard-linked files), the
/// whole walk peaked at 17 MB resident, so a tree as large as the cap, in the
/// same proportions, holds about 200 MB. That is the usual shape, not a bound:
/// at the cap, a tree made only of folders or of distinct hard-linked files
/// holds 0.4 to 0.8 GB of keys, and one directory with millions of subfolders
/// queues all their paths before it lists any of them. The cap sits above the
/// entry count of a whole system drive: at the former 1,000,000 that drive's root, the folder
/// a user most likely asks about, stopped short at 692,000 files without
/// saying so, and the walk now reports as it goes and can be stopped.
pub const MAX_ENTRIES: u64 = 20_000_000;

/// Directory levels below the folder a walk descends.
pub const MAX_DEPTH: usize = 100;

/// Entries a thread lists between two looks at the shared entry count.
const CAP_CHECK_EVERY: u64 = 1024;

/// How often a running walk reports its totals so far.
const PROGRESS_EVERY: Duration = Duration::from_millis(150);

/// Bounds of one walk.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_entries: u64,
    pub max_depth: usize,
    pub threads: usize,
    pub progress_every: Duration,
    /// Makes one directory, by name, misbehave when it is listed.
    #[cfg(test)]
    pub(crate) fault: Option<(&'static str, Fault)>,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub(crate) enum Fault {
    Cancel,
    Panic,
    /// Puts a link to the folder `outside`, next to the walk's root, in the
    /// place of the named folder after the walk admitted it.
    #[cfg(unix)]
    SwapForLink,
    /// Puts a link to the folder `outside-copy`, next to the walk's root, in
    /// the place of the named folder's parent: the named folder keeps its
    /// path, now through a link.
    #[cfg(unix)]
    SwapParentForLink,
    /// Puts a copy of the named folder in its place after the walk admitted
    /// it: the same path and contents under another inode, as a mount that
    /// renumbers its inodes shows.
    ReplaceWithCopy,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_entries: MAX_ENTRIES,
            max_depth: MAX_DEPTH,
            threads: default_threads(),
            progress_every: PROGRESS_EVERY,
            #[cfg(test)]
            fault: None,
        }
    }
}

/// Threads a walk uses: one per logical CPU, from 2 to 16. Measured on 24
/// logical CPUs with an NVMe drive and a warm cache, sizing `/usr` (306,000
/// entries) took 0.36 s on 1 thread, 0.19 s on 2, 0.11 s on 4, 0.065 s on 8,
/// 0.036 to 0.044 s on 16 and 0.033 to 0.039 s on 24; past 16 the gain is
/// within the noise while the cost to everything else running keeps growing.
fn default_threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(2, 16)
}

/// Total size of the folder at `root`.
///
/// `report` is called from the calling thread with the totals so far, every
/// `limits.progress_every` while the walk runs. The folder itself must be
/// readable: a folder that cannot be listed is an error, not an empty folder.
/// A cancelled walk still returns what it counted, marked `cancelled`.
pub fn scan(
    root: &Path,
    limits: Limits,
    cancel: &AtomicBool,
    mut report: impl FnMut(&FolderSizeResult),
) -> Result<FolderSizeResult, String> {
    let meta = fs::metadata(root).map_err(|e| format!("Cannot read {}: {e}", root.display()))?;
    if !meta.is_dir() {
        return Err("Path is not a directory".to_string());
    }
    fs::read_dir(root).map_err(|e| format!("Cannot read {}: {e}", root.display()))?;
    // Sizing `/proc` itself: what it lists is generated when read, nothing of
    // it is stored, and `/proc/kcore` alone is the size of the address space.
    #[cfg(unix)]
    if is_kernel_filesystem(root) {
        return Ok(FolderSizeResult::default());
    }

    let walk = Walk::new(limits, cancel);
    #[cfg(unix)]
    walk.seen.first_visit(meta.dev(), meta.ino());
    walk.queue_lock().dirs.push(Dir {
        // The folder named, through any link on the way to it: the walk does
        // not follow links below the root, and checks each folder it lists.
        path: fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf()),
        depth: 0,
        #[cfg(unix)]
        dev: meta.dev(),
        #[cfg(unix)]
        ino: meta.ino(),
    });

    std::thread::scope(|threads| {
        for _ in 0..walk.threads {
            threads.spawn(|| {
                // A thread that panics would otherwise leave the others waiting
                // for a directory it will never hand over, and this scope would
                // never end; the guard ends the walk instead, and the scope
                // then passes the panic on to the caller.
                let _guard = EndOnPanic(&walk);
                walk.work();
            });
        }
        // A panic in `report` would otherwise reach the caller only once the
        // threads had walked the whole folder: the scope waits for them.
        let _guard = EndOnPanic(&walk);
        let mut last = Instant::now();
        let mut queue = walk.queue_lock();
        while !queue.done {
            let wait = limits.progress_every.saturating_sub(last.elapsed());
            queue = walk
                .wake
                .wait_timeout(queue, wait)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
            if !queue.done && last.elapsed() >= limits.progress_every {
                drop(queue);
                report(&walk.totals());
                last = Instant::now();
                queue = walk.queue_lock();
            }
        }
    });

    Ok(walk.totals())
}

/// A directory waiting to be listed.
struct Dir {
    path: PathBuf,
    depth: usize,
    /// Device of this directory, to notice the walk crossing into a mount.
    #[cfg(unix)]
    dev: u64,
    /// Inode of this directory: with `dev`, what the path must still name
    /// when the folder is listed ([`read_admitted`]).
    #[cfg(unix)]
    ino: u64,
}

struct Queue {
    dirs: Vec<Dir>,
    /// Threads waiting for a directory.
    idle: usize,
    /// No thread will take another directory: the walk is complete, or stopped.
    done: bool,
}

struct Walk<'a> {
    queue: Mutex<Queue>,
    wake: Condvar,
    /// `queue.idle`, readable without the lock: a busy thread checks it after
    /// every directory to decide whether to share what it found.
    idle: AtomicUsize,
    threads: usize,
    limits: Limits,
    cancel: &'a AtomicBool,
    /// The walk was ended (entry cap, cancel, a thread's panic): every thread
    /// returns at its next directory.
    ended: AtomicBool,
    /// A cap left part of the folder out.
    capped: AtomicBool,
    entries: AtomicU64,
    bytes: AtomicU64,
    files: AtomicU64,
    dirs: AtomicU64,
    unreadable_dirs: AtomicU64,
    unreadable_files: AtomicU64,
    #[cfg(unix)]
    seen: Seen,
}

/// Held by each walking thread: a thread that panics ends the walk on its way
/// out, so the others stop waiting for work it will never hand over and the
/// panic reaches the caller instead of a hang.
struct EndOnPanic<'w, 'a>(&'w Walk<'a>);

impl Drop for EndOnPanic<'_, '_> {
    /// Ends the walk only when unwinding: a thread that returns normally has
    /// already done its part.
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.finish();
        }
    }
}

/// Per-directory counts, added to the walk's totals once the directory is done
/// so the threads do not contend on the counters entry by entry.
#[derive(Default)]
struct Tally {
    bytes: u64,
    files: u64,
    dirs: u64,
    unreadable_dirs: u64,
    unreadable_files: u64,
    entries: u64,
}

impl<'a> Walk<'a> {
    /// An empty walk under `limits`, stopped early by `cancel`; the caller
    /// queues the root.
    fn new(limits: Limits, cancel: &'a AtomicBool) -> Self {
        Self {
            queue: Mutex::new(Queue {
                dirs: Vec::new(),
                idle: 0,
                done: false,
            }),
            wake: Condvar::new(),
            idle: AtomicUsize::new(0),
            threads: limits.threads.max(1),
            limits,
            cancel,
            ended: AtomicBool::new(false),
            capped: AtomicBool::new(false),
            entries: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            files: AtomicU64::new(0),
            dirs: AtomicU64::new(0),
            unreadable_dirs: AtomicU64::new(0),
            unreadable_files: AtomicU64::new(0),
            #[cfg(unix)]
            seen: Seen::default(),
        }
    }

    /// The queue, also after a thread panicked holding it: the counts it
    /// guards stay consistent, and the walk is ending anyway.
    fn queue_lock(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The totals so far, `partial` when any of its reasons already holds.
    fn totals(&self) -> FolderSizeResult {
        let unreadable_dirs = self.unreadable_dirs.load(Ordering::Relaxed);
        let unreadable_files = self.unreadable_files.load(Ordering::Relaxed);
        let capped = self.capped.load(Ordering::Relaxed);
        let cancelled = self.cancel.load(Ordering::Relaxed);
        FolderSizeResult {
            total_bytes: self.bytes.load(Ordering::Relaxed),
            file_count: self.files.load(Ordering::Relaxed),
            dir_count: self.dirs.load(Ordering::Relaxed),
            partial: unreadable_dirs + unreadable_files > 0 || capped || cancelled,
            unreadable_dirs,
            unreadable_files,
            capped,
            cancelled,
        }
    }

    /// The walk was ended or cancelled: a thread stops at its next entry.
    fn halted(&self) -> bool {
        self.ended.load(Ordering::Relaxed) || self.cancel.load(Ordering::Relaxed)
    }

    /// One thread of the pool: its own subfolders first, the shared queue
    /// when it has none left.
    fn work(&self) {
        let mut own: Vec<Dir> = Vec::new();
        while let Some(dir) = own.pop().or_else(|| self.take()) {
            if self.halted() {
                self.finish();
                return;
            }
            self.list(dir, &mut own);
            if own.len() > 1 && self.idle.load(Ordering::Relaxed) > 0 {
                self.share(&mut own);
            }
        }
    }

    /// The next directory from the shared queue, waiting while another thread
    /// may still produce one. `None` once every thread is waiting with nothing
    /// queued, which is the walk being complete, or once the walk was ended.
    fn take(&self) -> Option<Dir> {
        let mut queue = self.queue_lock();
        loop {
            if queue.done {
                return None;
            }
            if let Some(dir) = queue.dirs.pop() {
                return Some(dir);
            }
            // A thread only waits here with nothing of its own left, so when
            // every thread is here no directory exists anywhere.
            queue.idle += 1;
            if queue.idle == self.threads {
                queue.done = true;
                self.wake.notify_all();
                return None;
            }
            self.idle.store(queue.idle, Ordering::Relaxed);
            queue = self
                .wake
                .wait(queue)
                .unwrap_or_else(PoisonError::into_inner);
            queue.idle -= 1;
            self.idle.store(queue.idle, Ordering::Relaxed);
        }
    }

    /// Hands the older half of `own` to the waiting threads. The older half is
    /// the shallower one, and a shallower folder is the bigger piece of work.
    fn share(&self, own: &mut Vec<Dir>) {
        let half = own.len() / 2;
        let mut queue = self.queue_lock();
        queue.dirs.extend(own.drain(..half));
        self.wake.notify_all();
    }

    /// Ends the walk: every waiting thread wakes and returns, every busy one
    /// returns at its next directory.
    fn finish(&self) {
        self.ended.store(true, Ordering::Relaxed);
        let mut queue = self.queue_lock();
        queue.done = true;
        self.wake.notify_all();
    }

    /// The entry cap: the rest of the folder is left out.
    fn reach_entry_cap(&self) {
        self.capped.store(true, Ordering::Relaxed);
        self.ended.store(true, Ordering::Relaxed);
    }

    /// Counts the entries of `dir` and queues its subfolders on `own`.
    fn list(&self, dir: Dir, own: &mut Vec<Dir>) {
        #[cfg(test)]
        self.inject_fault(&dir.path);
        let mut tally = Tally::default();
        // `_open` keeps the folder open while its entries are read.
        match read_admitted(&dir) {
            Ok((entries, _open)) => {
                for entry in entries {
                    if self.halted() {
                        break;
                    }
                    tally.entries += 1;
                    if tally.entries.is_multiple_of(CAP_CHECK_EVERY) {
                        let so_far = self.entries.load(Ordering::Relaxed) + tally.entries;
                        if so_far > self.limits.max_entries {
                            self.reach_entry_cap();
                            break;
                        }
                    }
                    self.count(&dir, entry, &mut tally, own);
                }
            }
            Err(_) => tally.unreadable_dirs += 1,
        }
        self.bytes.fetch_add(tally.bytes, Ordering::Relaxed);
        self.files.fetch_add(tally.files, Ordering::Relaxed);
        self.dirs.fetch_add(tally.dirs, Ordering::Relaxed);
        self.unreadable_dirs
            .fetch_add(tally.unreadable_dirs, Ordering::Relaxed);
        self.unreadable_files
            .fetch_add(tally.unreadable_files, Ordering::Relaxed);
        let entries = self.entries.fetch_add(tally.entries, Ordering::Relaxed) + tally.entries;
        if entries > self.limits.max_entries {
            self.reach_entry_cap();
        }
    }

    /// Counts one entry of `dir`: a file into `tally` (once per inode when it
    /// has several names), a folder into `own`, the thread's own queue, unless
    /// it was already walked, sits on a kernel pseudo-filesystem, or lies at
    /// the depth bound. Links and special files are left out.
    fn count(
        &self,
        dir: &Dir,
        entry: std::io::Result<fs::DirEntry>,
        tally: &mut Tally,
        own: &mut Vec<Dir>,
    ) {
        // The listing itself failed part way: the folder is not wholly read.
        let Ok(entry) = entry else {
            tally.unreadable_dirs += 1;
            return;
        };
        let Ok(kind) = entry.file_type() else {
            // Neither its type nor its size is known.
            tally.unreadable_files += 1;
            return;
        };
        if !kind.is_file() && !kind.is_dir() {
            // Symbolic links, junctions and special files are neither followed
            // nor counted.
            return;
        }
        // What the entry is and what it holds come from one read, so an entry
        // replaced since it was listed counts as what it is now.
        let Ok(meta) = entry.metadata() else {
            if kind.is_dir() {
                tally.dirs += 1;
                tally.unreadable_dirs += 1;
            } else {
                // There, size unknown.
                tally.files += 1;
                tally.unreadable_files += 1;
            }
            return;
        };
        if meta.is_file() {
            #[cfg(unix)]
            if meta.nlink() > 1 && !self.seen.first_visit(meta.dev(), meta.ino()) {
                return;
            }
            tally.files += 1;
            tally.bytes += meta.len();
        } else if meta.is_dir() {
            // From the folder's path, not the entry's: a folder may be read
            // through its descriptor ([`read_admitted`]).
            let path = dir.path.join(entry.file_name());
            #[cfg(unix)]
            {
                if meta.dev() != dir.dev && is_kernel_filesystem(&path) {
                    return;
                }
                if !self.seen.first_visit(meta.dev(), meta.ino()) {
                    return;
                }
            }
            tally.dirs += 1;
            let depth = dir.depth + 1;
            let child = Dir {
                path,
                depth,
                #[cfg(unix)]
                dev: meta.dev(),
                #[cfg(unix)]
                ino: meta.ino(),
            };
            if depth >= self.limits.max_depth {
                // Counted, not entered: anything inside is left out, and the
                // rest of the walk goes on.
                match read_admitted(&child).map(|(mut inside, _open)| inside.next().is_none()) {
                    Ok(true) => {}
                    Ok(false) => self.capped.store(true, Ordering::Relaxed),
                    Err(_) => tally.unreadable_dirs += 1,
                }
                return;
            }
            own.push(child);
        }
        // Otherwise it became a link or a special file since it was listed.
    }

    #[cfg(test)]
    fn inject_fault(&self, path: &Path) {
        if let Some((name, fault)) = self.limits.fault {
            if path.file_name().is_some_and(|n| n == name) {
                match fault {
                    Fault::Cancel => self.cancel.store(true, Ordering::Relaxed),
                    Fault::Panic => panic!("fault injected at {}", path.display()),
                    #[cfg(unix)]
                    Fault::SwapForLink => {
                        let root = path
                            .parent()
                            .and_then(Path::parent)
                            .expect("a nested folder");
                        fs::rename(path, path.with_extension("moved"))
                            .expect("move the folder away");
                        std::os::unix::fs::symlink(root.join("outside"), path)
                            .expect("link in its place");
                    }
                    #[cfg(unix)]
                    Fault::SwapParentForLink => {
                        let parent = path.parent().expect("a nested folder");
                        let beside_root = parent
                            .parent()
                            .and_then(Path::parent)
                            .expect("a folder two levels down");
                        fs::rename(parent, parent.with_extension("moved"))
                            .expect("move the parent away");
                        std::os::unix::fs::symlink(beside_root.join("outside-copy"), parent)
                            .expect("link in its place");
                    }
                    Fault::ReplaceWithCopy => {
                        let moved = path.with_extension("moved");
                        fs::rename(path, &moved).expect("move the folder away");
                        fs::create_dir(path).expect("a folder in its place");
                        for entry in fs::read_dir(&moved).expect("the moved folder") {
                            let entry = entry.expect("an entry");
                            fs::copy(entry.path(), path.join(entry.file_name()))
                                .expect("copy a file");
                        }
                    }
                }
            }
        }
    }
}

/// Opens a queued folder for listing, if its path still names the folder the
/// walk admitted. Between the two another process can put a symbolic link in
/// its place, and `read_dir` on the path would follow it out of the tree.
///
/// On Unix the folder is opened without following a link at its name, and
/// its device and inode are compared with the admitted ones. When they differ,
/// which some mounts do on their own (FUSE without fixed inode numbers, such
/// as sshfs or rclone; CIFS with `noserverino`), the folder is kept only if no
/// component of its path is a link ([`link_on_the_way`]). On Linux with
/// `/proc` mounted the folder is then listed through its descriptor and each
/// entry examined relative to it, so nothing swapped in afterwards is
/// followed. Elsewhere, and on Linux without `/proc`, the path is listed and
/// each entry examined by its path: the check narrows the window to the
/// listing, it does not close it. Windows refuses a folder that has turned
/// into a link at its name; a link higher up its path is not checked there.
/// The returned handle keeps the folder open while it is read.
#[cfg(unix)]
fn read_admitted(dir: &Dir) -> std::io::Result<(fs::ReadDir, fs::File)> {
    use std::os::unix::fs::OpenOptionsExt;
    let open = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
        .open(&dir.path)?;
    let meta = open.metadata()?;
    if (meta.dev() != dir.dev || meta.ino() != dir.ino) && link_on_the_way(&dir.path) {
        return Err(std::io::Error::other(
            "the folder was replaced during the walk",
        ));
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        if let Ok(entries) = fs::read_dir(format!("/proc/self/fd/{}", open.as_raw_fd())) {
            return Ok((entries, open));
        }
    }
    Ok((fs::read_dir(&dir.path)?, open))
}

/// Whether a component of `path` is a symbolic link. The walk's paths start at
/// a canonical root and add the names of folders it met, so a link on the way
/// was put there while the walk ran.
#[cfg(unix)]
fn link_on_the_way(path: &Path) -> bool {
    path.ancestors()
        .any(|a| fs::symlink_metadata(a).is_ok_and(|m| m.file_type().is_symlink()))
}

#[cfg(not(unix))]
fn read_admitted(dir: &Dir) -> std::io::Result<(fs::ReadDir, ())> {
    if fs::symlink_metadata(&dir.path)?.file_type().is_symlink() {
        return Err(std::io::Error::other(
            "the folder was replaced during the walk",
        ));
    }
    Ok((fs::read_dir(&dir.path)?, ()))
}

/// Locks [`Seen`] is split over.
#[cfg(unix)]
const SEEN_SHARDS: usize = 16;

/// Inodes already counted, by `(device, inode)`. Split over several locks so
/// the threads do not queue on one; the inode picks the shard.
#[cfg(unix)]
struct Seen([Mutex<HashSet<(u64, u64)>>; SEEN_SHARDS]);

#[cfg(unix)]
impl Default for Seen {
    fn default() -> Self {
        Self(std::array::from_fn(|_| Mutex::new(HashSet::new())))
    }
}

#[cfg(unix)]
impl Seen {
    /// True the first time `(dev, ino)` is offered.
    fn first_visit(&self, dev: u64, ino: u64) -> bool {
        self.0[(ino % SEEN_SHARDS as u64) as usize]
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert((dev, ino))
    }
}

/// Filesystems whose files the kernel generates when they are read: none of it
/// is stored on a disk. From `linux/magic.h`.
#[cfg(target_os = "linux")]
const KERNEL_FILESYSTEMS: [u32; 13] = [
    0x9fa0,      // proc
    0x6265_6572, // sysfs
    0x6462_6720, // debugfs
    0x7472_6163, // tracefs
    0x7363_6673, // securityfs
    0x0027_e0eb, // cgroup
    0x6367_7270, // cgroup2
    0xcafe_4a11, // bpf
    0x6165_676c, // pstore
    0x1cd1,      // devpts
    0x4249_4e4d, // binfmt_misc
    0xde5e_81e4, // efivarfs
    0x0187,      // autofs: listing an indirect map's folder (`/net`) mounts each entry
];

/// Whether `path` is on one of [`KERNEL_FILESYSTEMS`].
#[cfg(target_os = "linux")]
fn is_kernel_filesystem(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: c_path is a valid NUL-terminated CString, stat is zero-initialized
    // and passed by mutable pointer. statfs only writes to the provided buffer.
    let fs_type = unsafe {
        let mut stat: libc::statfs = std::mem::zeroed();
        if libc::statfs(c_path.as_ptr(), &mut stat) != 0 {
            return false;
        }
        stat.f_type
    };
    // `f_type` is a signed word whose width depends on the architecture; the
    // magic numbers all fit in 32 bits.
    #[allow(clippy::unnecessary_cast)]
    let fs_type = fs_type as u32;
    KERNEL_FILESYSTEMS.contains(&fs_type)
}

#[cfg(all(unix, not(target_os = "linux")))]
fn is_kernel_filesystem(_path: &Path) -> bool {
    false
}

/// A registered walk: the serial of its [`Registration`] and its cancel flag.
type Running = (u64, Arc<AtomicBool>);

/// Ids remembered as cancelled before their walk registered.
const EARLY_CANCELS: usize = 64;

/// The walks the frontend can stop, by the id it chose, so closing a dialog
/// can stop the walk the dialog started.
#[derive(Default)]
struct Registry {
    running: HashMap<String, Running>,
    /// Ids cancelled before their walk registered, oldest first. The walk and
    /// its cancel are two commands, and the runtime may run the cancel first:
    /// the walk then starts stopped instead of running on unseen. An id whose
    /// walk never comes (its path was refused, or it ended as the cancel was
    /// sent) is forgotten after [`EARLY_CANCELS`] newer ones.
    cancelled_early: VecDeque<String>,
}

static REGISTRY: LazyLock<Mutex<Registry>> = LazyLock::new(Mutex::default);
static NEXT_REGISTRATION: AtomicU64 = AtomicU64::new(0);

/// The registry of running walks, still usable after a thread panicked
/// holding it: it only maps ids to flags, which a panic cannot leave torn.
fn registry() -> MutexGuard<'static, Registry> {
    REGISTRY.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A walk's entry in the registry, removed when the walk ends.
pub struct Registration {
    id: Option<String>,
    serial: u64,
    flag: Arc<AtomicBool>,
}

impl Registration {
    /// Registers a walk under `id`, or keeps its flag private when there is no
    /// id. A second walk under the same id takes the id over; a walk whose id
    /// was cancelled before it got here starts cancelled.
    pub fn new(id: Option<String>) -> Self {
        let serial = NEXT_REGISTRATION.fetch_add(1, Ordering::Relaxed);
        let flag = Arc::new(AtomicBool::new(false));
        if let Some(id) = &id {
            let mut registry = registry();
            if let Some(at) = registry.cancelled_early.iter().position(|c| c == id) {
                registry.cancelled_early.remove(at);
                flag.store(true, Ordering::Relaxed);
            }
            registry.running.insert(id.clone(), (serial, flag.clone()));
        }
        Self { id, serial, flag }
    }

    /// The id the walk was registered under, the one its progress events carry.
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    /// The flag [`cancel`] sets for this walk: pass it to [`scan`].
    pub fn flag(&self) -> &AtomicBool {
        &self.flag
    }
}

impl Drop for Registration {
    /// Leaves the registry, unless a later walk under the same id has taken
    /// the id over: that walk's entry stays.
    fn drop(&mut self) {
        let Some(id) = &self.id else { return };
        let mut registry = registry();
        if registry
            .running
            .get(id)
            .is_some_and(|(serial, _)| *serial == self.serial)
        {
            registry.running.remove(id);
        }
    }
}

/// Stops the walk registered under `id`, or the one about to register under
/// it. True when a running walk was found.
pub fn cancel(id: &str) -> bool {
    let mut registry = registry();
    if let Some((_, flag)) = registry.running.get(id) {
        flag.store(true, Ordering::Relaxed);
        return true;
    }
    if registry.cancelled_early.len() == EARLY_CANCELS {
        registry.cancelled_early.pop_front();
    }
    registry.cancelled_early.push_back(id.to_string());
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits(threads: usize) -> Limits {
        Limits {
            threads,
            ..Limits::default()
        }
    }

    fn size_of(root: &Path, limits: Limits) -> Result<FolderSizeResult, String> {
        scan(root, limits, &AtomicBool::new(false), |_| {})
    }

    fn write(path: &Path, bytes: usize) {
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(path, vec![b'x'; bytes]).expect("write");
    }

    /// A tree wide and deep enough that every thread gets work and shares it:
    /// `fanout^depth` leaf folders, each with `files` files of known sizes.
    fn tree(root: &Path, fanout: usize, depth: usize, files: usize) -> FolderSizeResult {
        let mut expected = FolderSizeResult::default();
        let mut level = vec![root.to_path_buf()];
        for _ in 0..depth {
            let mut next = Vec::new();
            for dir in &level {
                for i in 0..fanout {
                    let child = dir.join(format!("d{i}"));
                    fs::create_dir(&child).expect("mkdir");
                    expected.dir_count += 1;
                    for f in 0..files {
                        let bytes = 1 + (f * 7 + i) % 13;
                        write(&child.join(format!("f{f}")), bytes);
                        expected.total_bytes += bytes as u64;
                        expected.file_count += 1;
                    }
                    next.push(child);
                }
            }
            level = next;
        }
        expected
    }

    #[test]
    fn counts_every_file_and_folder_once() {
        let dir = tempfile::tempdir().expect("tempdir");
        let expected = tree(dir.path(), 3, 3, 2);
        assert_eq!(size_of(dir.path(), limits(1)).unwrap(), expected);
    }

    /// The pool shares work while it walks. Whatever the interleaving, the
    /// answer is the one a single thread gives: no folder lost when it changes
    /// hands, none walked twice, and the walk ends (an empty queue with a
    /// thread still listing is not the end).
    #[test]
    fn many_threads_agree_with_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let expected = tree(dir.path(), 6, 3, 3);
        assert_eq!(size_of(dir.path(), limits(1)).unwrap(), expected);
        for threads in [2, 3, 8, 16] {
            for _ in 0..10 {
                assert_eq!(size_of(dir.path(), limits(threads)).unwrap(), expected);
            }
        }
    }

    #[test]
    fn an_empty_folder_is_zero_and_complete() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            size_of(dir.path(), limits(4)).unwrap(),
            FolderSizeResult::default()
        );
    }

    #[test]
    fn a_file_or_a_missing_path_is_not_a_folder() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("f");
        write(&file, 3);
        assert_eq!(
            size_of(&file, limits(2)).unwrap_err(),
            "Path is not a directory"
        );
        assert!(size_of(&dir.path().join("absent"), limits(2)).is_err());
    }

    #[test]
    fn a_cancel_before_the_walk_counts_nothing_and_says_so() {
        let dir = tempfile::tempdir().expect("tempdir");
        tree(dir.path(), 4, 2, 1);
        let result = scan(dir.path(), limits(4), &AtomicBool::new(true), |_| {}).unwrap();
        assert!(result.cancelled && result.partial, "{result:?}");
        assert_eq!(result.file_count, 0);
    }

    /// Cancelled half way, from inside the walk: the threads stop instead of
    /// finishing the tree, and the totals they reached come back marked.
    #[test]
    fn a_cancel_mid_walk_stops_every_thread() {
        let dir = tempfile::tempdir().expect("tempdir");
        let expected = tree(dir.path(), 8, 3, 2);
        for threads in [1, 4] {
            let cancel = AtomicBool::new(false);
            let walk = Limits {
                fault: Some(("d3", Fault::Cancel)),
                ..limits(threads)
            };
            let result = scan(dir.path(), walk, &cancel, |_| {}).unwrap();
            assert!(result.cancelled && result.partial, "{result:?}");
            assert!(
                result.file_count < expected.file_count,
                "{threads} threads walked the whole tree after the cancel: {result:?}"
            );
        }
    }

    /// A folder replaced by a link after the walk admitted it, before it is
    /// listed: the link is not followed, and the folder counts as unreadable.
    #[cfg(unix)]
    #[test]
    fn a_folder_swapped_for_a_link_while_queued_is_not_followed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("root");
        fs::create_dir_all(root.join("a")).unwrap();
        fs::write(root.join("a").join("kept.bin"), [0u8; 10]).unwrap();
        fs::write(root.join("b.bin"), [0u8; 1]).unwrap();
        fs::create_dir(dir.path().join("outside")).unwrap();
        fs::write(
            dir.path().join("outside").join("elsewhere.bin"),
            [0u8; 1000],
        )
        .unwrap();
        let walk = Limits {
            fault: Some(("a", Fault::SwapForLink)),
            ..limits(1)
        };
        let result = scan(&root, walk, &AtomicBool::new(false), |_| {}).unwrap();
        assert!(fs::symlink_metadata(root.join("a"))
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            result.total_bytes, 1,
            "followed the link swapped in: {result:?}"
        );
        assert_eq!(result.unreadable_dirs, 1, "{result:?}");
        assert!(result.partial, "{result:?}");
    }

    /// A link put in the place of a folder's parent: the folder keeps its
    /// path, but the path now leads out of the tree, to a folder of the same
    /// name. Refused, though its inode is the one thing that tells it apart.
    #[cfg(unix)]
    #[test]
    fn a_folder_reached_through_a_link_swapped_in_above_it_is_not_followed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("root");
        fs::create_dir_all(root.join("a").join("c")).unwrap();
        fs::write(root.join("a").join("c").join("kept.bin"), [0u8; 10]).unwrap();
        fs::write(root.join("b.bin"), [0u8; 1]).unwrap();
        let copy = dir.path().join("outside-copy");
        fs::create_dir_all(copy.join("c")).unwrap();
        fs::write(copy.join("c").join("elsewhere.bin"), [0u8; 1000]).unwrap();
        let walk = Limits {
            fault: Some(("c", Fault::SwapParentForLink)),
            ..limits(1)
        };
        let result = scan(&root, walk, &AtomicBool::new(false), |_| {}).unwrap();
        assert!(fs::symlink_metadata(root.join("a"))
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            result.total_bytes, 1,
            "followed the link swapped in above the folder: {result:?}"
        );
        assert_eq!(result.unreadable_dirs, 1, "{result:?}");
        assert!(result.partial, "{result:?}");
    }

    /// A folder whose inode changed with no link on its path, as on a mount
    /// that renumbers inodes (FUSE, CIFS with `noserverino`), is counted, not
    /// reported unreadable.
    #[test]
    fn a_folder_under_another_inode_on_the_same_path_is_counted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("root");
        fs::create_dir_all(root.join("a")).unwrap();
        fs::write(root.join("a").join("kept.bin"), [0u8; 10]).unwrap();
        fs::write(root.join("b.bin"), [0u8; 1]).unwrap();
        let walk = Limits {
            fault: Some(("a", Fault::ReplaceWithCopy)),
            ..limits(1)
        };
        let result = scan(&root, walk, &AtomicBool::new(false), |_| {}).unwrap();
        assert_eq!(
            (result.total_bytes, result.unreadable_dirs, result.partial),
            (11, 0, false),
            "{result:?}"
        );
    }

    /// The folder named may be reached through a link: only links below it
    /// are refused.
    #[cfg(unix)]
    #[test]
    fn the_folder_named_may_be_a_link() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::create_dir(dir.path().join("real")).unwrap();
        let expected = tree(&dir.path().join("real"), 3, 2, 1);
        std::os::unix::fs::symlink(dir.path().join("real"), dir.path().join("via")).unwrap();
        let result = scan(
            &dir.path().join("via"),
            limits(2),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        assert_eq!(
            (result.total_bytes, result.file_count),
            (expected.total_bytes, expected.file_count)
        );
        assert!(!result.partial, "{result:?}");
    }

    /// Runs `scan` on its own thread and says whether it panicked, failing
    /// the test instead of hanging the runner if the walk never ends.
    fn panics_within_a_minute(
        root: &Path,
        limits: Limits,
        report: impl FnMut(&FolderSizeResult) + Send + 'static,
    ) -> bool {
        let root = root.to_path_buf();
        let (done, outcome) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                scan(&root, limits, &AtomicBool::new(false), report)
            }));
            let _ = done.send(result.is_err());
        });
        outcome
            .recv_timeout(Duration::from_secs(60))
            .expect("the walk hung instead of ending")
    }

    /// A thread that panics ends the walk and the panic reaches the caller; the
    /// other threads are not left waiting for its directories.
    #[test]
    fn a_panicking_thread_ends_the_walk_instead_of_hanging_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        tree(dir.path(), 4, 3, 1);
        for threads in [1, 4] {
            let walk = Limits {
                fault: Some(("d2", Fault::Panic)),
                ..limits(threads)
            };
            assert!(
                panics_within_a_minute(dir.path(), walk, |_| {}),
                "{threads} threads: the panic was lost"
            );
        }
    }

    /// A panic in the progress report reaches the caller too.
    #[test]
    fn a_panicking_progress_report_reaches_the_caller() {
        let dir = tempfile::tempdir().expect("tempdir");
        tree(dir.path(), 4, 3, 1);
        let walk = Limits {
            progress_every: Duration::ZERO,
            ..limits(4)
        };
        assert!(panics_within_a_minute(dir.path(), walk, |_| panic!(
            "report"
        )));
    }

    #[test]
    fn the_entry_cap_stops_the_walk_and_says_so() {
        let dir = tempfile::tempdir().expect("tempdir");
        let expected = tree(dir.path(), 4, 2, 3);
        let capped = size_of(
            dir.path(),
            Limits {
                max_entries: 5,
                ..limits(1)
            },
        )
        .unwrap();
        assert!(capped.capped && capped.partial, "{capped:?}");
        assert!(capped.file_count < expected.file_count, "{capped:?}");
        assert!(!size_of(dir.path(), limits(1)).unwrap().partial);
    }

    /// Only what was left out makes the result partial: a folder at the depth
    /// cap with nothing in it hides nothing.
    #[test]
    fn the_depth_cap_is_partial_only_when_it_hides_something() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(dir.path().join("a/empty")).expect("mkdir");
        let shallow = Limits {
            max_depth: 2,
            ..limits(1)
        };
        let result = size_of(dir.path(), shallow).unwrap();
        assert_eq!((result.dir_count, result.partial), (2, false));

        write(&dir.path().join("a/empty/hidden"), 5);
        // A sibling branch after the cut one is still walked: the depth cap
        // leaves out one branch, it does not end the walk.
        for branch in ["b", "c", "d"] {
            write(&dir.path().join(branch).join("f"), 7);
        }
        let result = size_of(dir.path(), shallow).unwrap();
        assert_eq!(result.total_bytes, 21);
        assert!(result.capped && result.partial, "{result:?}");
        assert_eq!(size_of(dir.path(), limits(1)).unwrap().total_bytes, 26);
    }

    /// A folder at the depth cap that cannot be read is reported as unreadable,
    /// not as a limit reached: raising a limit would not help.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_folder_at_the_depth_cap_is_unreadable_not_capped() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let locked = dir.path().join("a/locked");
        fs::create_dir_all(&locked).expect("mkdir");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).expect("chmod");
        if fs::read_dir(&locked).is_ok() {
            // Running as root: nothing is unreadable.
            fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).expect("chmod");
            return;
        }
        let result = size_of(
            dir.path(),
            Limits {
                max_depth: 2,
                ..limits(1)
            },
        )
        .unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).expect("chmod");
        assert_eq!((result.dir_count, result.unreadable_dirs), (2, 1));
        assert!(result.partial && !result.capped, "{result:?}");
    }

    #[cfg(unix)]
    #[test]
    fn a_hard_linked_file_counts_once() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(&dir.path().join("a/data"), 100);
        fs::create_dir(dir.path().join("b")).expect("mkdir");
        fs::hard_link(dir.path().join("a/data"), dir.path().join("b/same")).expect("link");
        fs::hard_link(dir.path().join("a/data"), dir.path().join("b/again")).expect("link");
        for threads in [1, 4] {
            let result = size_of(dir.path(), limits(threads)).unwrap();
            assert_eq!((result.total_bytes, result.file_count), (100, 1));
        }
    }

    /// A link back up the tree is the classic way a walk loops forever; links
    /// are not followed, so it is one entry and nothing more.
    #[cfg(unix)]
    #[test]
    fn symbolic_links_are_not_followed_even_into_a_loop() {
        let outside = tempfile::tempdir().expect("tempdir");
        write(&outside.path().join("big"), 1000);
        let dir = tempfile::tempdir().expect("tempdir");
        write(&dir.path().join("sub/own"), 10);
        std::os::unix::fs::symlink(outside.path(), dir.path().join("to-dir")).expect("symlink");
        std::os::unix::fs::symlink(outside.path().join("big"), dir.path().join("to-file"))
            .expect("symlink");
        std::os::unix::fs::symlink(dir.path(), dir.path().join("sub/loop")).expect("symlink");
        let result = size_of(dir.path(), limits(2)).unwrap();
        assert_eq!(
            (result.total_bytes, result.file_count, result.dir_count),
            (10, 1, 1)
        );
        assert!(!result.partial);
    }

    /// A directory met a second time is not walked again. On a real system that
    /// is a bind mount or a firmlink, which a test cannot create unprivileged;
    /// what decides it is this set.
    #[cfg(unix)]
    #[test]
    fn an_inode_is_seen_once() {
        let seen = Seen::default();
        assert!(seen.first_visit(1, 42));
        assert!(!seen.first_visit(1, 42));
        assert!(seen.first_visit(2, 42), "another device is another inode");
    }

    /// A subfolder the user cannot read leaves the rest counted and the result
    /// marked; the folder asked about being unreadable is an error.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_folder_is_reported_not_skipped() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        write(&dir.path().join("open/f"), 4);
        write(&dir.path().join("locked/f"), 8);
        write(&dir.path().join("sealed/f"), 16);
        let locked = dir.path().join("locked");
        let sealed = dir.path().join("sealed");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).expect("chmod");
        fs::set_permissions(&sealed, fs::Permissions::from_mode(0o400)).expect("chmod");
        // Root reads through any mode: nothing to observe then.
        let readable_anyway = fs::read_dir(&locked).is_ok();
        let sub = size_of(dir.path(), limits(2));
        let root = size_of(&locked, limits(2));
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).expect("chmod back");
        fs::set_permissions(&sealed, fs::Permissions::from_mode(0o755)).expect("chmod back");
        if readable_anyway {
            return;
        }
        let sub = sub.unwrap();
        // `locked` cannot be listed; `sealed` lists `f` but cannot stat it, so
        // `f` is a file of unknown size.
        assert_eq!((sub.total_bytes, sub.file_count, sub.partial), (4, 2, true));
        assert_eq!((sub.unreadable_dirs, sub.unreadable_files), (1, 1));
        assert!(root.unwrap_err().starts_with("Cannot read"));
    }

    #[test]
    fn the_lower_bound_reason_names_every_cause() {
        assert_eq!(FolderSizeResult::default().lower_bound_reason(), None);
        let result = FolderSizeResult {
            partial: true,
            unreadable_dirs: 2,
            unreadable_files: 1,
            capped: true,
            cancelled: true,
            ..FolderSizeResult::default()
        };
        assert_eq!(
            result.lower_bound_reason().as_deref(),
            Some("lower bound: 2 folders could not be read, 1 file could not be sized, a scan limit was reached, the scan was cancelled")
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn proc_is_a_kernel_filesystem_and_a_temp_dir_is_not() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(!is_kernel_filesystem(dir.path()));
        if Path::new("/proc/self").exists() {
            assert!(is_kernel_filesystem(Path::new("/proc")));
        }
    }

    /// Sizing a kernel pseudo-filesystem itself: nothing in it is stored.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_folder_on_a_kernel_filesystem_holds_nothing() {
        if Path::new("/proc/self").exists() {
            assert_eq!(
                size_of(Path::new("/proc"), limits(2)).unwrap(),
                FolderSizeResult::default()
            );
        }
    }

    /// The tests that fill `cancelled_early` or rely on an id staying in it,
    /// one at a time: the queue is global, and one test filling it would push
    /// out the id another just cancelled.
    static EARLY_CANCEL_TESTS: Mutex<()> = Mutex::new(());

    /// The walk and its cancel are two commands and may run in either order:
    /// a cancel that arrives first still stops the walk.
    #[test]
    fn a_cancel_sent_before_the_walk_registers_still_stops_it() {
        let _one_at_a_time = EARLY_CANCEL_TESTS
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        assert!(!cancel("folder-size-test-early"));
        let late = Registration::new(Some("folder-size-test-early".into()));
        assert!(late.flag().load(Ordering::Relaxed));
        let dir = tempfile::tempdir().expect("tempdir");
        tree(dir.path(), 2, 2, 1);
        let result = scan(dir.path(), limits(2), late.flag(), |_| {}).unwrap();
        assert!(result.cancelled && result.partial, "{result:?}");
        assert_eq!(result.file_count, 0);
        // Taken by the walk it was for: the next one under the id runs.
        drop(late);
        let again = Registration::new(Some("folder-size-test-early".into()));
        assert!(!again.flag().load(Ordering::Relaxed));
    }

    /// A cancel whose walk never comes is not kept for ever.
    #[test]
    fn early_cancels_are_forgotten_oldest_first() {
        let _one_at_a_time = EARLY_CANCEL_TESTS
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        for n in 0..=EARLY_CANCELS {
            cancel(&format!("folder-size-test-forgotten-{n}"));
        }
        let first = Registration::new(Some("folder-size-test-forgotten-0".into()));
        let last = Registration::new(Some(format!("folder-size-test-forgotten-{EARLY_CANCELS}")));
        assert!(!first.flag().load(Ordering::Relaxed));
        assert!(last.flag().load(Ordering::Relaxed));
    }

    #[test]
    fn cancel_reaches_the_walk_registered_under_the_id() {
        let first = Registration::new(Some("folder-size-test-a".into()));
        let private = Registration::new(None);
        assert!(cancel("folder-size-test-a"));
        assert!(first.flag().load(Ordering::Relaxed));
        assert!(!private.flag().load(Ordering::Relaxed));
        drop(first);
        assert!(!cancel("folder-size-test-a"), "removed when the walk ends");
    }

    /// The id passes to the newer walk; the older one ending must not take the
    /// newer one's entry with it.
    #[test]
    fn an_older_walk_ending_keeps_the_newer_registration() {
        let older = Registration::new(Some("folder-size-test-b".into()));
        let newer = Registration::new(Some("folder-size-test-b".into()));
        drop(older);
        assert!(cancel("folder-size-test-b"));
        assert!(newer.flag().load(Ordering::Relaxed));
    }
}
