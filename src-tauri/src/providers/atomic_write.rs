//! Atomic file write helper for safe downloads.
//!
//! Prevents 0-byte files by writing to a `.aerotmp` temporary file first,
//! then atomically renaming to the final path only on success.
//! If the download fails mid-stream, the temp file is cleaned up.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::multi_thread::RangeSourceFingerprint;

/// Global flag: when true, skip .aerotmp and write directly to the final path.
/// Set via `set_inplace_mode(true)` from the CLI when --inplace is passed.
static INPLACE_MODE: AtomicBool = AtomicBool::new(false);

/// Enable or disable inplace mode (skip .aerotmp temp files).
pub fn set_inplace_mode(enabled: bool) {
    INPLACE_MODE.store(enabled, Ordering::Relaxed);
}

pub(crate) fn inplace_active() -> bool {
    INPLACE_MODE.load(Ordering::Relaxed)
}

/// Generate the dedicated temp path used by out-of-order read-ahead downloads.
/// It must stay distinct from `.aerotmp`: a pre-sized, out-of-order partial is
/// not safe for the length-based resumable writer to append to or commit.
pub fn readahead_temp_path_for(final_path: &Path) -> PathBuf {
    crate::util::temp_name::sibling(final_path, ".aeroardtmp")
}

/// The record a resumable part keeps of the version of the file it was
/// started from: `<final>.aerosrctmp`, next to its `<final>.aerotmp`.
pub fn part_record_path_for(final_path: &Path) -> PathBuf {
    crate::util::temp_name::sibling(final_path, ".aerosrctmp")
}

/// The record of the part at `temp`, when `temp` is a resumable part.
fn record_of_part(temp: &Path) -> Option<PathBuf> {
    (temp.extension()? == "aerotmp").then(|| temp.with_extension("aerosrctmp"))
}

/// True for local download sidecars that filesystem watchers must hide.
pub fn is_download_temp_path(path: &str) -> bool {
    path.ends_with(".aerotmp")
        || path.ends_with(".aeroardtmp")
        || path.ends_with(".aerosegtmp")
        || path.ends_with(".aerosrctmp")
}

/// What a part record holds. `format` lets a later layout be told from this
/// one: a record of another format is no record, and its part starts over.
#[derive(serde::Serialize, serde::Deserialize)]
struct PartRecord {
    format: u32,
    source: RangeSourceFingerprint,
}

const PART_RECORD_FORMAT: u32 = 1;

/// A record is a few hundred bytes; anything larger is not one.
const PART_RECORD_MAX_BYTES: u64 = 64 * 1024;

/// The version the record at `record` names, or `None` when there is no
/// readable record there. Only a regular file is read: a symlink or a FIFO
/// planted at the name is no record (and a FIFO would block the open).
async fn read_part_record(record: &Path) -> Option<RangeSourceFingerprint> {
    if !fs::symlink_metadata(record).await.ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    fs::File::open(record)
        .await
        .ok()?
        .take(PART_RECORD_MAX_BYTES)
        .read_to_end(&mut bytes)
        .await
        .ok()?;
    let parsed: PartRecord = serde_json::from_slice(&bytes).ok()?;
    (parsed.format == PART_RECORD_FORMAT).then_some(parsed.source)
}

/// Write the record at `record`. Whatever is at the name goes first and the
/// record is then created new, so a symlink planted there is replaced, never
/// written through.
async fn write_part_record(record: &Path, source: &RangeSourceFingerprint) -> std::io::Result<()> {
    remove_part_record(record).await;
    let body = serde_json::to_vec(&PartRecord {
        format: PART_RECORD_FORMAT,
        source: source.clone(),
    })
    .map_err(std::io::Error::other)?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(record)
        .await?;
    file.write_all(&body).await?;
    file.flush().await
}

/// Remove the record at `record`, if there is one.
async fn remove_part_record(record: &Path) {
    if let Err(e) = fs::remove_file(record).await {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::debug!("could not remove {}: {e}", record.display());
        }
    }
}

/// The length of the part a download of `final_path` goes on from: its
/// `.aerotmp`, when that keeps a record of the version it was started from.
/// The download then checks the record against the file as it is now, and
/// starts over when they differ.
///
/// A part with no record is removed here, unless a live download holds it,
/// and the length is 0. Such a part may be any writer's leftover (a crash, a
/// download that never keeps parts, another server), so its length is no
/// offset into this file; asking the server for the bytes after it joined
/// two versions, or wrote a tail behind a run of zeros.
pub async fn resumable_part_len(final_path: &Path) -> u64 {
    let temp = AtomicFile::temp_path_for(final_path);
    let Ok(meta) = fs::symlink_metadata(&temp).await else {
        return 0;
    };
    if meta.is_file()
        && read_part_record(&part_record_path_for(final_path))
            .await
            .is_some_and(|recorded| recorded.pins_a_version())
    {
        return meta.len();
    }
    let removed = tokio::task::spawn_blocking(move || remove_temp_unless_live(&temp)).await;
    if let Ok(Err(e)) = removed {
        tracing::debug!("a part with no record was kept: {e}");
    }
    0
}

/// How a download claims its temporary.
///
/// `AtomicFile` and `ResumableFile` write `<final>.aerotmp`, and so does the
/// delta writer (`aerorsync`, which keeps its own copy of these rules because
/// it may not name app code). Each holds an exclusive lock on the temporary it
/// writes for as long as it writes; the lock goes with the handle, so it ends
/// with the writer however the writer ends. A temporary found at the name is
/// then a live writer's while its lock is held, and stale otherwise (a
/// download killed, or dropped while its create was on its way): a live one is
/// left alone and the new download fails on it, a stale one is replaced.
///
/// Linux only, on a local filesystem. Windows locks are mandatory, and a second
/// handle of the same process could no longer write the file; Linux turns
/// flock on a CIFS/SMB mount into a whole-file SMB lock, mandatory the same
/// way on Windows servers and most NAS shares; on NFS `flock` can block even
/// when asked not to (which is why Cargo skips it there too); and on other
/// Unix systems a mount's kind is not read, so a share there could be either.
/// In all those places, and where the filesystem cannot lock at all (some
/// FUSE and VM shared folders), the temporary stays unlocked and is handled as
/// before these locks. Locks are per machine: two machines writing one path on
/// a shared mount (sshfs, NFS or SMB) do not see each other's.
pub(crate) mod temp_claim {
    use std::io::{Error, ErrorKind, Result};
    use std::path::Path;

    /// What was found at a temporary's name.
    #[derive(Debug, PartialEq, Eq)]
    enum Found {
        /// Nothing holds it: removed.
        Stale,
        /// A writer holds its lock: left alone.
        Live,
        /// This filesystem cannot say: left alone.
        Unknown,
    }

    /// Create the temporary at `temp` and claim it. One already there is
    /// replaced when stale, and refused otherwise.
    pub(crate) fn create(temp: &Path) -> Result<std::fs::File> {
        create_with(temp, try_lock)
    }

    /// Open the temporary at `temp` to go on writing it (a resume), and claim
    /// it: refused while another writer holds it.
    pub(crate) fn open_to_append(temp: &Path) -> Result<std::fs::File> {
        let file = std::fs::OpenOptions::new()
            .append(true)
            .open(temp)
            .map_err(|e| name_too_long(e, temp))?;
        claim(&file, temp, try_lock)?;
        Ok(file)
    }

    /// Create the temporary of a download that starts over (a fresh resumable
    /// part, a segmented run): a stale one is replaced, a live one refused;
    /// where the locks are not used, one found there is discarded, as before
    /// them.
    pub(crate) fn create_fresh(temp: &Path) -> Result<std::fs::File> {
        create_fresh_with(temp, try_lock, locks_usable)
    }

    fn create_fresh_with(
        temp: &Path,
        lock: impl Fn(&std::fs::File) -> std::result::Result<(), std::fs::TryLockError>,
        usable: impl Fn(&Path) -> bool,
    ) -> Result<std::fs::File> {
        if usable(temp) {
            return create_with(temp, lock);
        }
        // Where nothing can tell a live writer's part from a stale one, the
        // part is discarded and created again, as before these locks: the one
        // an interrupted resumable download keeps would otherwise fail every
        // later fresh download of the file until removed by hand. A file open
        // elsewhere on Windows cannot be removed, and the create then fails.
        if let Err(e) = std::fs::remove_file(temp) {
            if e.kind() != ErrorKind::NotFound {
                tracing::debug!("could not discard {}: {e}", temp.display());
            }
        }
        std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(temp)
            .map_err(|e| name_too_long(e, temp))
    }

    /// Remove the temporary at `temp` unless a live writer holds it: a stale
    /// one is removed while this call holds its lock, a live one is refused.
    /// Where locks are not used it is removed by name, as before them.
    pub(crate) fn remove_unless_live(temp: &Path) -> Result<()> {
        match take_if_stale(temp, try_lock) {
            Ok(Found::Stale) => Ok(()),
            Ok(Found::Live) => Err(in_use(temp)),
            Ok(Found::Unknown) => match std::fs::remove_file(temp) {
                Err(gone) if gone.kind() == ErrorKind::NotFound => Ok(()),
                removed => removed,
            },
            Err(gone) if gone.kind() == ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn create_with(
        temp: &Path,
        lock: impl Fn(&std::fs::File) -> std::result::Result<(), std::fs::TryLockError>,
    ) -> Result<std::fs::File> {
        let mut probed = false;
        loop {
            match std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(temp)
            {
                Ok(file) => {
                    // A lock this filesystem cannot take keeps the file, as
                    // before these locks: the file this call made is never
                    // left behind by a failed claim.
                    claim(&file, temp, &lock)?;
                    return Ok(file);
                }
                Err(taken) if taken.kind() == ErrorKind::AlreadyExists && !probed => {
                    probed = true;
                    match take_if_stale(temp, &lock) {
                        Ok(Found::Stale) => continue,
                        Ok(Found::Live) => return Err(in_use(temp)),
                        Ok(Found::Unknown) => return Err(cannot_tell(temp)),
                        // The other writer committed or removed it meanwhile.
                        Err(gone) if gone.kind() == ErrorKind::NotFound => continue,
                        Err(e) => return Err(e),
                    }
                }
                Err(taken) if taken.kind() == ErrorKind::AlreadyExists => return Err(in_use(temp)),
                Err(e) => return Err(name_too_long(e, temp)),
            }
        }
    }

    /// `e`, from creating or opening the temporary at `temp`, told plainly
    /// when the file system refused the name as too long. The temporary is
    /// the local name plus a suffix of up to 11 bytes (`.aerosegtmp`), cut to
    /// the 255 bytes most file systems take (`util::temp_name`); one with a
    /// lower limit (eCryptfs takes 143) can still refuse it, and the bare
    /// "File name too long" reads as if the name were refused. The name is
    /// not cut further here: a resumed download finds its part again by the
    /// name `util::temp_name` gives, on every file system alike.
    pub(crate) fn name_too_long(e: Error, temp: &Path) -> Error {
        #[cfg(unix)]
        let too_long = e.raw_os_error() == Some(libc::ENAMETOOLONG);
        // ERROR_FILENAME_EXCED_RANGE (206) is only a length error.
        // ERROR_INVALID_NAME (123) is a syntax error too: a short name with a
        // character Windows forbids (`?`, `*`, `:`) fails with it, and
        // "shorten the name" is wrong there, so 123 is a length error only
        // for a name past the 255 UTF-16 unit component limit (#987: the bare
        // 123 reads "The filename, directory name, or volume label syntax is
        // incorrect", which says nothing about the name).
        #[cfg(windows)]
        let too_long = match e.raw_os_error() {
            Some(206) => true,
            Some(123) => {
                use std::os::windows::ffi::OsStrExt;
                temp.file_name()
                    .is_some_and(|name| name.encode_wide().count() > 255)
            }
            _ => false,
        };
        #[cfg(not(any(unix, windows)))]
        let too_long = false;
        if !too_long {
            return e;
        }
        let bytes = temp.file_name().map_or(0, |name| name.len());
        // A name within the folder's own name limit was not refused for its
        // length: the whole path was (a fixed-length temporary in a deep
        // folder), and "shorten the file name" would not help.
        #[cfg(unix)]
        if name_fits_its_folder(temp, bytes) {
            return Error::new(
                ErrorKind::InvalidFilename,
                format!(
                    "Cannot create the download temporary {}: the path is {} bytes, \
                     too long for this file system; download into a shorter folder ({e})",
                    temp.display(),
                    temp.as_os_str().len()
                ),
            );
        }
        Error::new(
            ErrorKind::InvalidFilename,
            format!(
                "Cannot create the download temporary {}: its name is {bytes} bytes, \
                 too long for this file system once the suffix is added to the file \
                 name; shorten the local file name ({e})",
                temp.display()
            ),
        )
    }

    /// Whether a name of `bytes` bytes fits the name limit of the folder that
    /// holds `temp` (`pathconf(_PC_NAME_MAX)`). False when the limit cannot be
    /// read, so the name is then assumed to be what was too long.
    #[cfg(unix)]
    fn name_fits_its_folder(temp: &Path, bytes: usize) -> bool {
        use std::os::unix::ffi::OsStrExt;
        let Some(parent) = temp.parent().filter(|p| !p.as_os_str().is_empty()) else {
            return false;
        };
        let Ok(parent) = std::ffi::CString::new(parent.as_os_str().as_bytes()) else {
            return false;
        };
        // SAFETY: `parent` is a valid NUL-terminated C string that outlives
        // the call; pathconf only reads it.
        let limit = unsafe { libc::pathconf(parent.as_ptr(), libc::_PC_NAME_MAX) };
        limit > 0 && bytes <= limit as usize
    }

    /// `name_too_long` on the raw codes of each platform: only the codes that
    /// mean a name too long are rewritten, everything else passes through.
    #[cfg(all(test, any(unix, windows)))]
    mod name_too_long_tests {
        use super::*;

        /// The temporary of a 250-character name: with ".aerotmp" it is 258
        /// UTF-16 units, past the 255-unit component limit.
        fn over_long_temp() -> std::path::PathBuf {
            Path::new("dir").join(format!("{}.aerotmp", "n".repeat(250)))
        }

        /// Windows CI of #987: a name with no room for the temporary's suffix
        /// fails there with ERROR_INVALID_NAME (123) or
        /// ERROR_FILENAME_EXCED_RANGE (206), never ENAMETOOLONG, and only 206
        /// was read: the download failed with the bare "The filename,
        /// directory name, or volume label syntax is incorrect". 123 is a
        /// syntax error too, so a short name keeps it: a `?` in the file name
        /// is what is wrong there, not the length.
        #[cfg(windows)]
        #[test]
        fn on_windows_a_too_long_name_is_read_from_either_code() {
            for code in [123, 206] {
                let told = name_too_long(Error::from_raw_os_error(code), &over_long_temp());
                assert!(
                    told.to_string().contains("shorten"),
                    "os error {code} was not read as a name too long"
                );
            }
            let short = Path::new("dir").join("report?.txt.aerotmp");
            // 206 is only a length error: a long path under a short name is
            // still told as too long.
            let told = name_too_long(Error::from_raw_os_error(206), &short);
            assert!(told.to_string().contains("shorten"));
            for code in [5, 123] {
                let other = name_too_long(Error::from_raw_os_error(code), &short);
                assert_eq!(
                    other.raw_os_error(),
                    Some(code),
                    "os error {code} on a short name was rewritten"
                );
            }
        }

        /// Only ENAMETOOLONG is rewritten: another error passes through as is.
        #[cfg(unix)]
        #[test]
        fn on_unix_only_a_name_too_long_is_rewritten() {
            let told = name_too_long(
                Error::from_raw_os_error(libc::ENAMETOOLONG),
                &over_long_temp(),
            );
            assert!(told.to_string().contains("shorten"));
            let other = name_too_long(Error::from_raw_os_error(libc::EACCES), &over_long_temp());
            assert_eq!(other.raw_os_error(), Some(libc::EACCES));
        }

        /// CodeRabbit on #987: a short temporary name in a real folder was
        /// told to shorten its file name when ENAMETOOLONG came from the
        /// whole path. It says the path is too long.
        #[cfg(unix)]
        #[test]
        fn on_unix_a_short_name_in_a_long_path_blames_the_path() {
            let dir = tempfile::tempdir().unwrap();
            let temp = dir.path().join(".aeroftp-readahead-0123456789");
            let told =
                name_too_long(Error::from_raw_os_error(libc::ENAMETOOLONG), &temp).to_string();
            assert!(told.contains("the path is"), "{told}");
            assert!(!told.contains("shorten the local file name"), "{told}");
        }
    }

    /// Lock `file`, opened at `path`, for this writer. Refused when another
    /// writer holds it, or when the name no longer points at this file (it
    /// was replaced between the open and the lock); a lock this filesystem
    /// cannot take is not an error.
    fn claim(
        file: &std::fs::File,
        path: &Path,
        lock: impl Fn(&std::fs::File) -> std::result::Result<(), std::fs::TryLockError>,
    ) -> Result<()> {
        if !locks_usable(path) {
            return Ok(());
        }
        match lock(file) {
            Ok(()) if same_file(file, path) => Ok(()),
            Ok(()) => Err(Error::new(
                ErrorKind::AlreadyExists,
                format!(
                    "{} was replaced by another download while it was opened",
                    path.display()
                ),
            )),
            Err(std::fs::TryLockError::WouldBlock) => Err(in_use(path)),
            Err(std::fs::TryLockError::Error(_)) => Ok(()),
        }
    }

    /// Remove the temporary at `temp` if nothing holds it. It is removed
    /// while this call holds its lock, and only if the name still points at
    /// the file that was locked: two calls that find the same stale file
    /// cannot remove the one the first of them then created.
    fn take_if_stale(
        temp: &Path,
        lock: impl Fn(&std::fs::File) -> std::result::Result<(), std::fs::TryLockError>,
    ) -> Result<Found> {
        if !locks_usable(temp) {
            return Ok(Found::Unknown);
        }
        let meta = std::fs::symlink_metadata(temp)?;
        if meta.file_type().is_symlink() {
            // Removed as a link, never followed.
            std::fs::remove_file(temp)?;
            return Ok(Found::Stale);
        }
        if !meta.file_type().is_file() {
            return Ok(Found::Unknown);
        }
        let existing = std::fs::OpenOptions::new().write(true).open(temp)?;
        match lock(&existing) {
            Ok(()) if same_file(&existing, temp) => {
                std::fs::remove_file(temp)?;
                Ok(Found::Stale)
            }
            Ok(()) => Ok(Found::Live),
            Err(std::fs::TryLockError::WouldBlock) => Ok(Found::Live),
            Err(std::fs::TryLockError::Error(_)) => Ok(Found::Unknown),
        }
    }

    fn try_lock(file: &std::fs::File) -> std::result::Result<(), std::fs::TryLockError> {
        file.try_lock()
    }

    fn in_use(temp: &Path) -> Error {
        Error::new(
            ErrorKind::AlreadyExists,
            format!(
                "another download of this file is writing {}",
                temp.display()
            ),
        )
    }

    fn cannot_tell(temp: &Path) -> Error {
        Error::new(
            ErrorKind::AlreadyExists,
            format!(
                "{} already exists: another download may be writing it, and this filesystem cannot say; remove it if none is running",
                temp.display()
            ),
        )
    }

    /// Whether the name still points at the file this handle has open.
    #[cfg(unix)]
    fn same_file(file: &std::fs::File, path: &Path) -> bool {
        use std::os::unix::fs::MetadataExt;
        match (file.metadata(), std::fs::symlink_metadata(path)) {
            (Ok(open), Ok(named)) => open.dev() == named.dev() && open.ino() == named.ino(),
            _ => false,
        }
    }

    #[cfg(not(unix))]
    fn same_file(_file: &std::fs::File, _path: &Path) -> bool {
        true
    }

    /// Whether locks are used at all where `path` lives (see the module doc).
    #[cfg(target_os = "linux")]
    fn locks_usable(path: &Path) -> bool {
        use std::os::unix::ffi::OsStrExt;
        let dir = match path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent,
            _ => Path::new("."),
        };
        let Ok(dir) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else {
            return false;
        };
        let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
        // SAFETY: `dir` is a NUL-terminated path, and `stat` is a writable
        // buffer of the size `statfs` fills.
        if unsafe { libc::statfs(dir.as_ptr(), stat.as_mut_ptr()) } != 0 {
            // The lock itself will say what it can do.
            return true;
        }
        // SAFETY: `statfs` returned 0, so it filled the buffer.
        let kind = unsafe { stat.assume_init() }.f_type;
        // A filesystem magic is 32 bits, whatever the width of `f_type`.
        locks_usable_on(kind as u32)
    }

    /// Whether a filesystem of this `statfs` type takes the locks: not
    // NFS, where `flock` can block even when asked not to (Cargo skips it
    // there too), and CIFS/SMB, where Linux 5.5 and later turn it into a
    // whole-file SMB lock that is mandatory on Windows servers and most NAS
    // shares: a second handle of the same process could no longer write, and
    // the segmented engine writes its windows through handles of their own.
    #[cfg(target_os = "linux")]
    fn locks_usable_on(kind: u32) -> bool {
        const CIFS_MAGIC_NUMBER: u32 = 0xFF53_4D42;
        const SMB2_MAGIC_NUMBER: u32 = 0xFE53_4D42;
        const SMB_SUPER_MAGIC: u32 = 0x517B;
        ![
            libc::NFS_SUPER_MAGIC as u32,
            CIFS_MAGIC_NUMBER,
            SMB2_MAGIC_NUMBER,
            SMB_SUPER_MAGIC,
        ]
        .contains(&kind)
    }

    /// Where the locks are not used, on every platform.
    #[cfg(test)]
    mod fresh_tests {
        use super::*;

        /// CodeRabbit on 15a1e76d (#951): a download that starts over (open_fresh,
        /// RESUME-01, and a segmented run) discards the part an interrupted one
        /// left, which a resumable download keeps on purpose. Where the locks
        /// are not used (Windows, NFS, SMB) the claim could not tell that part
        /// from a live writer's and refused it, so every later fresh download of
        /// the file failed until the part was removed by hand. There it is
        /// discarded, as before the locks.
        #[test]
        fn a_fresh_download_where_locks_are_not_used_discards_an_interrupted_part() {
            let dir = tempfile::tempdir().unwrap();
            let temp = dir.path().join("f.bin.aerotmp");
            std::fs::write(&temp, b"an interrupted part").unwrap();
            let fresh = create_fresh_with(&temp, try_lock, |_| false);
            assert!(
                fresh.is_ok(),
                "a fresh download was refused over an interrupted part: {:?}",
                fresh.err()
            );
            drop(fresh);
            assert_eq!(
                std::fs::read(&temp).unwrap(),
                b"",
                "the interrupted part was kept under the fresh download"
            );
        }
    }

    /// Only Linux says what a mount is here: elsewhere a network share could
    /// turn the lock into a mandatory one (macOS smbfs maps flock onto SMB
    /// locks), so the temporaries go unlocked, as before these locks.
    #[cfg(all(unix, not(target_os = "linux")))]
    fn locks_usable(_path: &Path) -> bool {
        false
    }

    #[cfg(not(unix))]
    fn locks_usable(_path: &Path) -> bool {
        false
    }

    #[cfg(all(test, target_os = "linux"))]
    mod tests {
        use super::*;

        /// End a writer's claim as `ClaimedTemp` does: unlocked, then closed.
        /// A close alone leaves the lock held by a child another test thread
        /// forks meanwhile, until that child execs: the lock belongs to the
        /// open file description, which its inherited descriptor shares.
        fn end_claim(file: std::fs::File) {
            file.unlock().unwrap();
        }

        fn failing_lock(_file: &std::fs::File) -> std::result::Result<(), std::fs::TryLockError> {
            Err(std::fs::TryLockError::Error(Error::from_raw_os_error(
                libc::EOPNOTSUPP,
            )))
        }

        /// Verification of round 4 of #951 (F1): a filesystem that cannot
        /// lock (some SMB, FUSE and VM shared folders) failed the create after
        /// it had made the file, and the empty temporary it left failed every
        /// later download of the file for good. The file stays, unlocked, and
        /// the writer goes on; a temporary found there is never taken.
        #[test]
        fn a_filesystem_without_locks_writes_as_before() {
            let dir = tempfile::tempdir().unwrap();
            let temp = dir.path().join("f.bin.aerotmp");
            let file = create_with(&temp, failing_lock).expect("the create must go on unlocked");
            drop(file);
            let again = create_with(&temp, failing_lock);
            assert_eq!(
                again.err().map(|e| e.kind()),
                Some(ErrorKind::AlreadyExists),
                "a temporary found where locks do not work was taken"
            );
            assert!(temp.exists());
        }

        /// F3: a name that no longer points at the file a handle has open is
        /// told apart, so a stale temporary is removed only while it is the
        /// one that was locked.
        #[test]
        fn a_replaced_temporary_is_not_the_one_that_was_opened() {
            let dir = tempfile::tempdir().unwrap();
            let temp = dir.path().join("f.bin.aerotmp");
            std::fs::write(&temp, b"first").unwrap();
            let opened = std::fs::File::open(&temp).unwrap();
            assert!(same_file(&opened, &temp));
            let other = dir.path().join("other");
            std::fs::write(&other, b"second").unwrap();
            std::fs::rename(&other, &temp).unwrap();
            assert!(!same_file(&opened, &temp));
        }

        /// Mutation of the final round of #951: `claim` took a file whose name
        /// had been taken over between its open and its lock (a probe removed
        /// it as stale and another download made a new one) for its own. The
        /// replacement is made inside the lock call.
        #[test]
        fn a_claim_on_a_replaced_temporary_is_refused() {
            let dir = tempfile::tempdir().unwrap();
            let temp = dir.path().join("f.bin.aerotmp");
            let other = dir.path().join("other");
            let file = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temp)
                .unwrap();
            let replaced_then_locked = |_: &std::fs::File| {
                std::fs::write(&other, b"second").unwrap();
                std::fs::rename(&other, &temp).unwrap();
                Ok(())
            };
            assert_eq!(
                claim(&file, &temp, replaced_then_locked)
                    .err()
                    .map(|e| e.kind()),
                Some(ErrorKind::AlreadyExists)
            );
        }

        /// The same between a probe's open and its lock: the file now at the
        /// name is another writer's, and the probe leaves it.
        #[test]
        fn a_probe_leaves_a_temporary_replaced_before_its_lock() {
            let dir = tempfile::tempdir().unwrap();
            let temp = dir.path().join("f.bin.aerotmp");
            let other = dir.path().join("other");
            std::fs::write(&temp, b"stale").unwrap();
            let replaced_then_locked = |_: &std::fs::File| {
                std::fs::write(&other, b"second").unwrap();
                std::fs::rename(&other, &temp).unwrap();
                Ok(())
            };
            assert_eq!(
                take_if_stale(&temp, replaced_then_locked).unwrap(),
                Found::Live
            );
            assert_eq!(std::fs::read(&temp).unwrap(), b"second");
        }

        /// Verification of 15a1e76d (#951), Major: on a CIFS/SMB mount Linux
        /// (5.5 and later) turns flock into a whole-file SMB lock, mandatory
        /// on Windows servers and most NAS shares, and the segmented engine's
        /// windows, which write through handles of their own, were refused
        /// (EACCES). SMB and CIFS take no locks, as NFS takes none.
        #[cfg(target_os = "linux")]
        #[test]
        fn smb_and_nfs_mounts_take_no_locks() {
            for (kind, usable) in [
                (0xFF53_4D42_u32, false),
                (0xFE53_4D42, false),
                (0x517B, false),
                (0x6969, false),
                (0xEF53, true),
                (0x0102_1994, true),
                (0x9123_683E, true),
            ] {
                assert_eq!(locks_usable_on(kind), usable, "{kind:#x}");
            }
        }

        /// The other side of the fresh create: where the locks are used it
        /// keeps the claim's rules, a live writer's part refused and a stale
        /// one replaced, so discarding without looking is not the fix.
        #[test]
        fn a_fresh_download_where_locks_are_used_keeps_the_claim_rules() {
            let dir = tempfile::tempdir().unwrap();
            let temp = dir.path().join("f.bin.aerotmp");
            let live = create(&temp).unwrap();
            assert_eq!(
                create_fresh(&temp).err().map(|e| e.kind()),
                Some(ErrorKind::AlreadyExists),
                "a fresh download took a live writer's part"
            );
            end_claim(live);
            std::fs::write(&temp, b"a stale part").unwrap();
            let fresh = create_fresh(&temp);
            assert!(
                fresh.is_ok(),
                "a fresh download was refused over a stale part: {:?}",
                fresh.err()
            );
            drop(fresh);
            assert_eq!(
                std::fs::read(&temp).unwrap(),
                b"",
                "the stale part was kept under the fresh download"
            );
        }

        /// The paths that discard a part by name (a part with no record, an
        /// interrupted delta attempt, a dropped transfer) remove a stale part
        /// and leave a live writer's alone, and a resume claims only a part
        /// nobody writes.
        #[test]
        fn a_part_discarded_or_resumed_by_name_is_not_a_live_writers() {
            let dir = tempfile::tempdir().unwrap();
            let temp = dir.path().join("f.bin.aerotmp");
            let live = create(&temp).unwrap();
            assert_eq!(
                super::super::remove_temp_unless_live(&temp)
                    .err()
                    .map(|e| e.kind()),
                Some(ErrorKind::AlreadyExists)
            );
            assert_eq!(
                open_to_append(&temp).err().map(|e| e.kind()),
                Some(ErrorKind::AlreadyExists)
            );
            assert!(temp.exists(), "a live writer's part was removed");
            end_claim(live);
            let claim = open_to_append(&temp).expect("a stale part");
            end_claim(claim);
            super::super::remove_temp_unless_live(&temp).expect("a stale part goes");
            assert!(!temp.exists());
            super::super::remove_temp_unless_live(&temp).expect("nothing there is fine");
        }

        /// A live writer's temporary is refused by a resume, which would
        /// otherwise append into it.
        #[test]
        fn a_resume_does_not_append_into_a_live_writers_temporary() {
            let dir = tempfile::tempdir().unwrap();
            let temp = dir.path().join("f.bin.aerotmp");
            let _live = create(&temp).unwrap();
            assert_eq!(
                open_to_append(&temp).err().map(|e| e.kind()),
                Some(ErrorKind::AlreadyExists)
            );
        }
    }
}

/// Remove the download temporary at `temp` unless a live writer holds it (see
/// [`temp_claim`]). For the paths that discard a part by name: a stale part
/// goes, another download's is left alone and refused.
pub fn remove_temp_unless_live(temp: &Path) -> std::io::Result<()> {
    temp_claim::remove_unless_live(temp)?;
    // The part is gone: its record names nothing any more.
    if let Some(record) = record_of_part(temp) {
        if let Err(e) = std::fs::remove_file(&record) {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::debug!("could not remove {}: {e}", record.display());
            }
        }
    }
    Ok(())
}

/// Run a claim, which is filesystem work, off the async runtime.
async fn claimed(
    claim: impl FnOnce() -> std::io::Result<std::fs::File> + Send + 'static,
) -> std::io::Result<fs::File> {
    let file = tokio::task::spawn_blocking(claim)
        .await
        .map_err(std::io::Error::other)??;
    Ok(fs::File::from_std(file))
}

/// A guard that writes to a temp file and renames on commit.
/// If dropped without calling `commit()`, the temp file is deleted.
/// In inplace mode, writes directly to the final path (no temp, no rename).
pub struct AtomicFile {
    temp_path: PathBuf,
    final_path: PathBuf,
    file: tokio::fs::File,
    committed: bool,
    inplace: bool,
}

impl AtomicFile {
    /// Create a new atomic file writer. The temp file is created immediately.
    /// In inplace mode, writes directly to the final path.
    pub async fn new(final_path: &str) -> Result<Self, std::io::Error> {
        let final_path = PathBuf::from(final_path);
        let inplace = inplace_active();
        let temp_path = if inplace {
            final_path.clone()
        } else {
            Self::temp_path_for(&final_path)
        };

        // Ensure parent directory exists
        if let Some(parent) = final_path.parent() {
            fs::create_dir_all(parent).await?;
        }

        let file = if inplace {
            fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&temp_path)
                .await?
        } else {
            let temp = temp_path.clone();
            let file = claimed(move || temp_claim::create(&temp)).await?;
            // This writer keeps no part to resume: a record left by an
            // earlier one must not vouch for the bytes it writes.
            remove_part_record(&part_record_path_for(&final_path)).await;
            file
        };

        Ok(Self {
            temp_path,
            final_path,
            file,
            committed: false,
            inplace,
        })
    }

    /// Get a mutable reference to the underlying file for writing.
    pub fn file_mut(&mut self) -> &mut tokio::fs::File {
        &mut self.file
    }

    /// Write data to the temp file.
    pub async fn write_all(&mut self, buf: &[u8]) -> Result<(), std::io::Error> {
        self.file.write_all(buf).await
    }

    /// Flush and commit: rename temp file to final path.
    /// In inplace mode, no rename is needed (already writing to final path).
    pub async fn commit(mut self) -> Result<(), std::io::Error> {
        self.file.flush().await?;
        self.file.sync_all().await?;
        self.file.shutdown().await?;

        if !self.inplace {
            fs::rename(&self.temp_path, &self.final_path).await?;
        }
        self.committed = true;
        Ok(())
    }

    /// Generate temp path for a given final path.
    fn temp_path_for(path: &Path) -> PathBuf {
        crate::util::temp_name::sibling(path, ".aerotmp")
    }
}

impl Drop for AtomicFile {
    fn drop(&mut self) {
        if !self.committed && !self.inplace {
            // Best-effort cleanup of temp file (skip in inplace mode: file is the final path)
            let temp = self.temp_path.clone();
            let _ = std::fs::remove_file(&temp);
        }
    }
}

/// A download file guard that preserves partial data on failure for later resume.
///
/// On fresh download: writes to `.aerotmp` (like `AtomicFile`).
/// On resume: detects existing `.aerotmp`, opens in append mode at the existing offset.
/// On failure: keeps `.aerotmp` intact so the next download can resume from where it left off.
/// On success: renames `.aerotmp` to the final path (same as `AtomicFile`).
///
/// Out of place a part keeps a record of the version of the file it was
/// started from (`<final>.aerosrctmp`, see [`part_record_path_for`]). A resume
/// goes on from the part only when the file still is that version: the bytes
/// after the part belong to the version the server has now, and appending
/// them to a part of an older one published a file made of two versions with
/// every check green, or, when the part already had the new length, the old
/// version itself without a byte read.
pub struct ResumableFile {
    temp_path: PathBuf,
    final_path: PathBuf,
    record_path: PathBuf,
    file: tokio::fs::File,
    committed: bool,
    /// Byte offset we are resuming from (0 = fresh download).
    offset: u64,
    /// The version the part found on open was started from, by its record.
    recorded: Option<RangeSourceFingerprint>,
    inplace: bool,
}

impl ResumableFile {
    /// Open the writer of a plain download. Out of place it goes on from the
    /// download's own `.aerotmp`, left by an interrupted one. In place it
    /// starts from zero and truncates the destination: a file already there
    /// is the user's, not a part of this download, and taking it for one
    /// reported a file as long as the remote one complete with its old
    /// content, and appended the remote's tail to a shorter one.
    pub async fn open(final_path: &str) -> Result<Self, std::io::Error> {
        Self::open_in(final_path, inplace_active()).await
    }

    /// Open the writer of an explicit resume (`resume_download`, which the
    /// CLI calls for `--partial`): out of place the `.aerotmp`, in place the
    /// destination itself, which the caller measured as the part to go on
    /// from.
    pub async fn open_resume(final_path: &str) -> Result<Self, std::io::Error> {
        Self::open_resume_in(final_path, inplace_active()).await
    }

    /// [`Self::open`] for a download of `source`: a part found out of place
    /// is gone on from only when its record names the version `source` reads
    /// now, and started over otherwise. See [`Self::bound_to`].
    pub async fn open_for(
        final_path: &str,
        source: &RangeSourceFingerprint,
    ) -> Result<Self, std::io::Error> {
        Self::open_in(final_path, inplace_active())
            .await?
            .bound_to(source)
            .await
    }

    /// [`Self::open_resume`] for a download of `source`, bound the same way.
    pub async fn open_resume_for(
        final_path: &str,
        source: &RangeSourceFingerprint,
    ) -> Result<Self, std::io::Error> {
        Self::open_resume_in(final_path, inplace_active())
            .await?
            .bound_to(source)
            .await
    }

    /// Keep the part this writer opened only if it was started from the
    /// version `source` reads now, and record that version for a part that
    /// starts now, before its first byte.
    ///
    /// In place the destination is the part and keeps no record: it goes on
    /// as the caller measured it, as before.
    pub async fn bound_to(mut self, source: &RangeSourceFingerprint) -> std::io::Result<Self> {
        if self.inplace {
            return Ok(self);
        }
        if self.offset > 0 {
            if let Some(why) = source.refuses_part(self.recorded.as_ref(), self.offset) {
                tracing::info!(
                    "partial download {} started over: {}",
                    self.temp_path.display(),
                    why
                );
                self = self.start_over().await?;
            }
        }
        if self.offset == 0 {
            self.record_source(source).await;
        }
        Ok(self)
    }

    /// The version the part found on open was started from, as its record
    /// names it; `None` for a fresh part, a part with no record, and in place.
    pub fn recorded_source(&self) -> Option<&RangeSourceFingerprint> {
        self.recorded.as_ref()
    }

    /// Record `source` as the version this part is started from. Called
    /// before the first byte is written. A source that names no version is
    /// not recorded, and a record that cannot be written leaves the part with
    /// none: either way a later download starts the part over rather than
    /// resume it blind.
    pub async fn record_source(&mut self, source: &RangeSourceFingerprint) {
        if self.inplace {
            return;
        }
        self.recorded = None;
        if !source.pins_a_version() {
            remove_part_record(&self.record_path).await;
            return;
        }
        match write_part_record(&self.record_path, source).await {
            Ok(()) => self.recorded = Some(source.clone()),
            Err(e) => tracing::debug!(
                "{} not recorded, the part will not be resumed: {e}",
                self.record_path.display()
            ),
        }
    }

    /// Discard this part and open a fresh one at the same name.
    pub async fn start_over(self) -> std::io::Result<Self> {
        let final_path = self.final_path.clone();
        let inplace = self.inplace;
        // A part that cannot be removed fails the fresh open below, which
        // reports it.
        let _ = self.discard().await;
        Self::fresh_at(final_path, inplace).await
    }

    /// [`Self::open`] with the in-place mode given rather than read from the
    /// process-wide flag, which a test cannot set without racing the others.
    async fn open_in(final_path: &str, inplace: bool) -> Result<Self, std::io::Error> {
        if inplace {
            return Self::open_fresh_in(final_path, true).await;
        }
        Self::open_resume_in(final_path, false).await
    }

    /// [`Self::open_resume`] with the in-place mode given.
    async fn open_resume_in(final_path: &str, inplace: bool) -> Result<Self, std::io::Error> {
        let final_path = PathBuf::from(final_path);
        let temp_path = if inplace {
            final_path.clone()
        } else {
            AtomicFile::temp_path_for(&final_path)
        };

        // Ensure parent directory exists
        if let Some(parent) = final_path.parent() {
            fs::create_dir_all(parent).await?;
        }

        // A part another writer commits between the look and the open is gone
        // by then: the download starts fresh instead of failing on NotFound.
        let resumed = match fs::symlink_metadata(&temp_path).await {
            Ok(symlink_meta) => {
                if symlink_meta.file_type().is_symlink() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "refusing to resume through symlinked .aerotmp file",
                    ));
                }
                if !symlink_meta.is_file() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "resume target is not a regular file",
                    ));
                }
                // Resume: open existing file in append mode.
                let opened = if inplace {
                    // The destination itself, which the caller measured.
                    fs::OpenOptions::new().append(true).open(&temp_path).await
                } else {
                    // The part is claimed before its length is read: while
                    // another writer holds it, it is refused, not appended to.
                    let temp = temp_path.clone();
                    claimed(move || temp_claim::open_to_append(&temp)).await
                };
                match opened {
                    Ok(file) => {
                        let offset = file.metadata().await?.len();
                        Some((file, offset))
                    }
                    Err(gone) if gone.kind() == std::io::ErrorKind::NotFound => None,
                    Err(e) => return Err(e),
                }
            }
            Err(gone) if gone.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(temp_claim::name_too_long(e, &temp_path)),
        };
        let record_path = part_record_path_for(&final_path);
        let (file, offset, recorded) = if let Some((file, offset)) = resumed {
            let recorded = if inplace {
                None
            } else {
                read_part_record(&record_path).await
            };
            (file, offset, recorded)
        } else {
            // Fresh: create new file
            let file = if inplace {
                fs::OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(true)
                    .open(&temp_path)
                    .await?
            } else {
                let temp = temp_path.clone();
                let file = claimed(move || temp_claim::create(&temp)).await?;
                remove_part_record(&record_path).await;
                file
            };
            (file, 0, None)
        };

        Ok(Self {
            temp_path,
            final_path,
            record_path,
            file,
            committed: false,
            offset,
            recorded,
            inplace,
        })
    }

    /// Create a fresh resumable file, discarding any existing partial data.
    pub async fn open_fresh(final_path: &str) -> Result<Self, std::io::Error> {
        Self::open_fresh_in(final_path, inplace_active()).await
    }

    /// [`Self::open_fresh`] with the in-place mode given.
    async fn open_fresh_in(final_path: &str, inplace: bool) -> Result<Self, std::io::Error> {
        Self::fresh_at(PathBuf::from(final_path), inplace).await
    }

    async fn fresh_at(final_path_buf: PathBuf, inplace: bool) -> Result<Self, std::io::Error> {
        let temp_path = if inplace {
            final_path_buf.clone()
        } else {
            AtomicFile::temp_path_for(&final_path_buf)
        };

        if let Some(parent) = final_path_buf.parent() {
            fs::create_dir_all(parent).await?;
        }

        let file = if inplace {
            fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&temp_path)
                .await?
        } else {
            // RESUME-01: open_fresh's contract is to discard any existing partial
            // data. A stale `.aerotmp` from a previous interrupted download would
            // otherwise make create_new fail with AlreadyExists, wedging every
            // retry until the user removes it by hand: the claim replaces a
            // stale one, and refuses one another writer still holds.
            let temp = temp_path.clone();
            let file = claimed(move || temp_claim::create_fresh(&temp)).await?;
            remove_part_record(&part_record_path_for(&final_path_buf)).await;
            file
        };

        Ok(Self {
            temp_path,
            record_path: part_record_path_for(&final_path_buf),
            final_path: final_path_buf,
            file,
            committed: false,
            offset: 0,
            recorded: None,
            inplace,
        })
    }

    /// Byte offset of existing partial data (0 = fresh download).
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Whether this writer writes the destination itself (`--inplace`).
    pub fn is_inplace(&self) -> bool {
        self.inplace
    }

    /// The last byte of the part found on open. Read through a handle of its
    /// own: the writer's is append-only.
    pub async fn last_byte(&self) -> std::io::Result<u8> {
        use tokio::io::AsyncSeekExt;
        let Some(at) = self.offset.checked_sub(1) else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "the part is empty",
            ));
        };
        let mut file = fs::File::open(&self.temp_path).await?;
        file.seek(std::io::SeekFrom::Start(at)).await?;
        let mut byte = [0u8; 1];
        file.read_exact(&mut byte).await?;
        Ok(byte[0])
    }

    /// Write data to the temp file (appending after any existing partial data).
    pub async fn write_all(&mut self, buf: &[u8]) -> Result<(), std::io::Error> {
        self.file.write_all(buf).await
    }

    /// Get a mutable reference to the underlying file for writing.
    pub fn file_mut(&mut self) -> &mut tokio::fs::File {
        &mut self.file
    }

    /// Flush and commit: rename temp file to final path.
    /// In inplace mode, no rename is needed.
    pub async fn commit(mut self) -> Result<(), std::io::Error> {
        self.file.flush().await?;
        self.file.sync_all().await?;
        self.file.shutdown().await?;

        if !self.inplace {
            // The record goes first, while this writer still holds the part:
            // after the rename the name is free, and a record removed then
            // could be the next download's.
            remove_part_record(&self.record_path).await;
            fs::rename(&self.temp_path, &self.final_path).await?;
        }
        self.committed = true;
        Ok(())
    }

    /// Discard partial data and remove the temp file.
    pub async fn discard(mut self) -> Result<(), std::io::Error> {
        self.committed = true; // prevent Drop from running
        let _ = self.file.shutdown().await;
        if !self.inplace {
            remove_part_record(&self.record_path).await;
            fs::remove_file(&self.temp_path).await
        } else {
            // In inplace mode, the temp file IS the final file: remove it
            fs::remove_file(&self.final_path).await
        }
    }
}

impl Drop for ResumableFile {
    fn drop(&mut self) {
        if !self.committed {
            // INTENTIONALLY keep .aerotmp on failure: this is the whole point
            // of ResumableFile: partial data is preserved for later resume.
            tracing::debug!(
                "ResumableFile: keeping partial download at {}",
                self.temp_path.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_classifier_recognizes_serial_and_readahead_sidecars() {
        assert!(is_download_temp_path("file.bin.aerotmp"));
        assert!(is_download_temp_path("file.bin.aeroardtmp"));
        assert!(is_download_temp_path("file.bin.aerosegtmp"));
        assert!(!is_download_temp_path("file.bin"));
        assert!(!is_download_temp_path("file.bin.aerardtmp"));
    }

    /// Verification of round 3 of #951: two downloads to one local path. The
    /// second must not take the first's `.aerotmp`: that let the first's
    /// commit publish the second's half-written file as complete. It fails
    /// while the first lives, and goes ahead once the first is gone.
    #[tokio::test]
    async fn a_live_writers_temporary_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.bin");
        let path = path.to_str().unwrap();
        let first = AtomicFile::new(path).await.expect("the first writer");
        let second = AtomicFile::new(path).await;
        assert_eq!(
            second.err().map(|e| e.kind()),
            Some(std::io::ErrorKind::AlreadyExists),
            "a live writer's temporary was taken"
        );
        drop(first);
        let mut third = AtomicFile::new(path)
            .await
            .expect("once the first writer is gone");
        third.write_all(b"new").await.unwrap();
        third.commit().await.unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"new");
    }

    /// Review of round 2 of #951: a `.aerotmp` left by a download that was
    /// killed, or dropped while its create was on its way, made every later
    /// atomic download of the file fail until it was removed by hand. Nothing
    /// holds its lock: it is stale, and replaced.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn an_atomic_download_replaces_a_stale_temporary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.bin");
        let temp = dir.path().join("f.bin.aerotmp");
        std::fs::write(&temp, b"stale").unwrap();
        let mut file = AtomicFile::new(path.to_str().unwrap())
            .await
            .expect("a stale temporary must not block the download");
        file.write_all(b"new").await.unwrap();
        file.commit().await.unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert!(!temp.exists());
    }

    /// Review of 04b35bac (#951): a resume opened the `.aerotmp` a live
    /// download was still writing and appended to it, and its commit then
    /// published the first writer's half, followed by its own bytes, as the
    /// complete file. The resume is refused while the first writer lives, and
    /// the first publishes what it wrote, whole.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_resume_does_not_publish_a_live_writers_temporary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.bin");
        let path = path.to_str().unwrap();
        let mut first = AtomicFile::new(path).await.expect("the first writer");
        first.write_all(b"first half").await.unwrap();
        let resumed = ResumableFile::open_in(path, false).await;
        assert_eq!(
            resumed.err().map(|e| e.kind()),
            Some(std::io::ErrorKind::AlreadyExists),
            "a resume took a live writer's temporary"
        );
        first.write_all(b", second half").await.unwrap();
        first.commit().await.unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"first half, second half");
    }

    /// Verification of round 4 of #951 (F2): the writers of `.aerotmp` that
    /// took no lock looked stale, and an atomic download removed a resumable
    /// download's part while it was being written. Every writer claims it.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_resumable_part_being_written_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.bin");
        let path = path.to_str().unwrap();
        let mut part = ResumableFile::open_in(path, false).await.unwrap();
        part.write_all(b"first part").await.unwrap();
        assert_eq!(
            AtomicFile::new(path).await.err().map(|e| e.kind()),
            Some(std::io::ErrorKind::AlreadyExists),
            "a live part was taken"
        );
        // tokio hands a write to a blocking thread: without a flush the drop
        // can come before the bytes reach the file, and the read below then
        // sees an empty part (seen on a loaded CI runner).
        part.file_mut().flush().await.unwrap();
        drop(part);
        assert_eq!(
            std::fs::read(dir.path().join("f.bin.aerotmp")).unwrap(),
            b"first part"
        );
    }

    /// F2 with the delta writer, which keeps its own copy of the rules: it
    /// truncated a temporary another download was still writing, and an
    /// atomic download removed the delta writer's.
    #[cfg(all(target_os = "linux", feature = "aerorsync"))]
    #[tokio::test]
    async fn the_delta_writer_and_an_atomic_download_leave_each_other_alone() {
        use crate::aerorsync::streaming_writer::StreamingAtomicWriter;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.bin");
        let atomic = AtomicFile::new(path.to_str().unwrap()).await.unwrap();
        assert_eq!(
            StreamingAtomicWriter::new(&path)
                .await
                .err()
                .map(|e| e.kind()),
            Some(std::io::ErrorKind::AlreadyExists),
            "the delta writer truncated a live download's temporary"
        );
        drop(atomic);
        let delta = StreamingAtomicWriter::new(&path).await.unwrap();
        assert_eq!(
            AtomicFile::new(path.to_str().unwrap())
                .await
                .err()
                .map(|e| e.kind()),
            Some(std::io::ErrorKind::AlreadyExists),
            "an atomic download took the delta writer's temporary"
        );
        drop(delta);
    }

    /// Pre-existing Major (verification of round 2 of #951): the segmented
    /// engine pre-sizes its temporary to the whole length, with holes until
    /// every window lands, and named it `.aerotmp`: one left at full size (a
    /// crash, a kill, a second Ctrl-C) was taken for a complete part by the
    /// next resumable download, and published with its holes. It has a name
    /// of its own, which a resumable download never reads.
    #[tokio::test]
    async fn a_segmented_temporary_is_not_a_resumable_part() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.bin");
        let segmented = crate::providers::multi_thread::segmented_temp_path_for(&path);
        std::fs::File::create(&segmented)
            .unwrap()
            .set_len(4096)
            .unwrap();
        let part = ResumableFile::open_in(path.to_str().unwrap(), false)
            .await
            .unwrap();
        assert_eq!(
            part.offset(),
            0,
            "the engine's temporary was taken for a part"
        );
        assert!(is_download_temp_path(&segmented.to_string_lossy()));
    }

    /// An in-place download over a file the user already has: that file is
    /// the destination, not a part of this download. It was taken for one: a
    /// file as long as the remote one was reported complete with its old
    /// content, and a shorter one had the remote's tail appended to it.
    #[tokio::test]
    async fn an_inplace_download_starts_over_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.bin");
        std::fs::write(&path, b"old content").unwrap();
        let path = path.to_str().unwrap();
        let mut file = ResumableFile::open_in(path, true).await.unwrap();
        assert_eq!(file.offset(), 0, "the user's file was taken for a part");
        file.write_all(b"new").await.unwrap();
        file.commit().await.unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"new");
    }

    /// An explicit resume in place goes on from the destination: the caller
    /// asked for it, and measured the part there.
    #[tokio::test]
    async fn an_inplace_resume_goes_on_from_the_destination() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.bin");
        std::fs::write(&path, b"first part").unwrap();
        let path = path.to_str().unwrap();
        let mut file = ResumableFile::open_resume_in(path, true).await.unwrap();
        assert_eq!(file.offset(), 10);
        file.write_all(b", the rest").await.unwrap();
        file.commit().await.unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"first part, the rest");
    }

    /// Out of place, a plain download still goes on from its own `.aerotmp`,
    /// and leaves a file already at the destination alone until it commits.
    #[tokio::test]
    async fn a_plain_download_goes_on_from_its_own_temporary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.bin");
        std::fs::write(&path, b"old content").unwrap();
        std::fs::write(dir.path().join("f.bin.aerotmp"), b"first part").unwrap();
        let path = path.to_str().unwrap();
        let mut file = ResumableFile::open_in(path, false).await.unwrap();
        assert_eq!(file.offset(), 10);
        file.write_all(b", the rest").await.unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"old content");
        file.commit().await.unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"first part, the rest");
    }

    /// A local name that fits the file system but leaves no room for the
    /// temporary's suffix failed on every attempt (4.2.1 only made the error
    /// clear): the stress test of the Proton Drive client lost a 244-byte
    /// name so. Its temporaries are cut to fit, and its download writes,
    /// resumes from its part against that part's record, and publishes.
    #[tokio::test]
    async fn a_name_with_no_room_for_the_suffix_downloads_and_resumes() {
        // 244 bytes ("ä" is two), and 250: both fit NAME_MAX (255), neither
        // with ".aerosegtmp" after it.
        for name in [format!("{}.txt", "ä".repeat(120)), "n".repeat(250)] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(&name);
            let path = path.to_str().unwrap();

            let mut file = AtomicFile::new(path).await.expect("atomic temporary");
            file.write_all(b"atomic").await.unwrap();
            file.commit().await.unwrap();
            assert_eq!(std::fs::read(path).unwrap(), b"atomic");

            let v1 = version(100, Some("\"v1\""), Some("2026-10-17 10:00:00"));
            leave_part(path, &v1, b"first part").await;
            let mut resumed = ResumableFile::open_for(path, &v1).await.unwrap();
            assert_eq!(resumed.offset(), 10, "{name}: the part was not found again");
            resumed.write_all(b", the rest").await.unwrap();
            resumed.commit().await.unwrap();
            assert_eq!(std::fs::read(path).unwrap(), b"first part, the rest");

            let segmented =
                crate::providers::multi_thread::segmented_temp_path_for(Path::new(path));
            drop(temp_claim::create_fresh(&segmented).expect("segmented temporary"));
            std::fs::remove_file(&segmented).unwrap();
            let readahead = readahead_temp_path_for(Path::new(path));
            std::fs::write(&readahead, b"window").expect("read-ahead temporary");
            std::fs::remove_file(&readahead).unwrap();

            let left: Vec<_> = std::fs::read_dir(dir.path())
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            assert_eq!(
                left,
                [std::ffi::OsString::from(&name)],
                "only the file remains"
            );
        }
    }

    /// A version of a remote file, as a provider's stat describes it.
    fn version(size: u64, tag: Option<&str>, modified: Option<&str>) -> RangeSourceFingerprint {
        let mut entry = crate::providers::RemoteEntry::directory(String::new(), String::new());
        entry.is_dir = false;
        entry.size = size;
        entry.modified = modified.map(str::to_string);
        if let Some(tag) = tag {
            entry.metadata.insert("etag".to_string(), tag.to_string());
        }
        RangeSourceFingerprint::of(&entry)
    }

    /// Leave a part of `bytes` behind, started from `source`, as an
    /// interrupted download does.
    async fn leave_part(path: &str, source: &RangeSourceFingerprint, bytes: &[u8]) {
        let mut part = ResumableFile::open_for(path, source).await.unwrap();
        assert_eq!(part.offset(), 0);
        part.write_all(bytes).await.unwrap();
        part.file_mut().flush().await.unwrap();
    }

    /// Requested 16 of #1202: a part is gone on from only while the file is
    /// the version it was started from. Replaced in the meantime, at the
    /// length the part already had, the old resume published the old version
    /// without reading a byte; at any other length it appended the new tail.
    #[tokio::test]
    async fn a_part_is_resumed_only_against_the_version_it_was_started_from() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.bin");
        let path = path.to_str().unwrap();
        let v1 = version(100, Some("\"v1\""), Some("2026-10-15 10:00:00"));
        leave_part(path, &v1, &[b'a'; 40]).await;

        let same = ResumableFile::open_for(path, &v1).await.unwrap();
        assert_eq!(
            same.offset(),
            40,
            "a part of the same version was not resumed"
        );
        drop(same);

        let replaced = version(100, Some("\"v2\""), Some("2026-10-15 10:00:00"));
        let mut fresh = ResumableFile::open_for(path, &replaced).await.unwrap();
        assert_eq!(fresh.offset(), 0, "a part of a replaced file was resumed");
        assert_eq!(fresh.recorded_source(), Some(&replaced));
        fresh.write_all(b"new").await.unwrap();
        fresh.commit().await.unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"new");
        assert!(
            !part_record_path_for(Path::new(path)).exists(),
            "the record outlived its part"
        );
    }

    /// The other doors of the same check: a part with no record, a part of a
    /// file that changed only its time, and a part longer than the file.
    #[tokio::test]
    async fn a_part_without_a_matching_record_starts_over() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.bin");
        let path = path.to_str().unwrap();
        let v1 = version(100, None, Some("2026-10-15 10:00:00"));

        std::fs::write(format!("{path}.aerotmp"), [b'a'; 40]).unwrap();
        let unrecorded = ResumableFile::open_for(path, &v1).await.unwrap();
        assert_eq!(unrecorded.offset(), 0, "a part with no record was resumed");
        drop(unrecorded);

        leave_part(path, &v1, &[b'a'; 40]).await;
        let touched = version(100, None, Some("2026-10-15 10:00:01"));
        let part = ResumableFile::open_for(path, &touched).await.unwrap();
        assert_eq!(
            part.offset(),
            0,
            "a part of a file rewritten in place was resumed"
        );
        drop(part);

        leave_part(path, &touched, &[b'a'; 40]).await;
        let shrunk = version(30, None, Some("2026-10-15 10:00:01"));
        let part = ResumableFile::open_for(path, &shrunk).await.unwrap();
        assert_eq!(part.offset(), 0, "a part longer than the file was resumed");
    }

    /// Size alone names no version: a part recorded with nothing else would
    /// accept any replacement of the same length, so none is recorded and
    /// the part is not resumed.
    #[tokio::test]
    async fn a_source_that_names_no_version_leaves_no_record() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.bin");
        let path = path.to_str().unwrap();
        let sized_only = version(100, None, None);
        leave_part(path, &sized_only, &[b'a'; 40]).await;
        assert!(!part_record_path_for(Path::new(path)).exists());
        let part = ResumableFile::open_for(path, &sized_only).await.unwrap();
        assert_eq!(part.offset(), 0);
    }

    /// The callers that measure a part before asking a provider to go on
    /// from it count only a recorded one. A part with no record may be a
    /// crash leftover of a download that keeps none, or another server's: FTP
    /// wrote the tail at its length into the destination, behind a run of
    /// zeros when the destination was missing. It is removed instead.
    #[tokio::test]
    async fn only_a_recorded_part_is_offered_for_a_resume() {
        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("f.bin");
        let temp = dir.path().join("f.bin.aerotmp");
        std::fs::write(&temp, [b'a'; 40]).unwrap();
        assert_eq!(resumable_part_len(&final_path).await, 0);
        assert!(!temp.exists(), "a part with no record was kept");

        let v1 = version(100, Some("\"v1\""), None);
        leave_part(final_path.to_str().unwrap(), &v1, &[b'a'; 40]).await;
        assert_eq!(resumable_part_len(&final_path).await, 40);
        assert!(temp.exists());

        remove_temp_unless_live(&temp).unwrap();
        assert!(
            !part_record_path_for(&final_path).exists(),
            "a part removed by name left its record"
        );
    }

    /// A writer that keeps no part (`AtomicFile`) must not leave an earlier
    /// part's record vouching for the bytes it writes, should it be killed.
    /// Linux only: it replaces the stale part only where the locks tell it
    /// from a live writer's; elsewhere it refuses the name and writes nothing.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_writer_that_keeps_no_part_drops_an_earlier_record() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.bin");
        let path = path.to_str().unwrap();
        leave_part(path, &version(100, Some("\"v1\""), None), &[b'a'; 40]).await;
        let atomic = AtomicFile::new(path).await.unwrap();
        assert!(!part_record_path_for(Path::new(path)).exists());
        drop(atomic);
    }

    /// A symlink planted at the record's name is replaced, never written
    /// through, and never read as a record.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlink_at_the_record_is_neither_followed_nor_trusted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.bin");
        let target = dir.path().join("victim");
        std::fs::write(&target, b"untouched").unwrap();
        let record = part_record_path_for(&path);
        std::os::unix::fs::symlink(&target, &record).unwrap();

        let v1 = version(100, Some("\"v1\""), None);
        leave_part(path.to_str().unwrap(), &v1, &[b'a'; 40]).await;
        assert_eq!(std::fs::read(&target).unwrap(), b"untouched");
        assert!(!std::fs::symlink_metadata(&record).unwrap().is_symlink());

        // The record just written reads back; the same bytes behind a
        // symlink do not.
        assert_eq!(read_part_record(&record).await, Some(v1.clone()));
        std::fs::copy(&record, &target).unwrap();
        std::fs::remove_file(&record).unwrap();
        std::os::unix::fs::symlink(&target, &record).unwrap();
        assert!(read_part_record(&record).await.is_none());
    }
}
