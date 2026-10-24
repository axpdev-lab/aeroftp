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

/// True for a private folder a download fills before it publishes its files
/// (Proton CLI: one per bulk and one per single file). It sits in the
/// destination folder for the length of the transfer, and a crash can leave
/// it behind: scans and watchers must never take it for content.
pub fn is_download_staging_dir_name(name: &str) -> bool {
    name.starts_with(".aeroftp-proton-bulk-") || name.starts_with(".aeroftp-proton-dl-")
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
    #[serde(default)]
    provenance: Option<ResumeSource>,
}

const PART_RECORD_FORMAT: u32 = 2;

/// A durable source namespace, separate from its version validators. Only
/// non-secret provider/authority/account/resource identifiers belong here;
/// credentials, bearer tokens and signed download URLs must never be inputs.
/// Store only the digest, preserving case and field boundaries in its input.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct ResumeSource(String);

impl ResumeSource {
    /// A profile is a settings slot, not an authenticated account. Without
    /// the latter, do not let a leftover vouch for a later authorization.
    /// Identified accounts keep the same namespace across token refreshes.
    pub fn account(profile: &str, authenticated_account: Option<&str>) -> String {
        match authenticated_account.filter(|account| !account.is_empty()) {
            Some(account) => serde_json::json!([profile, account]).to_string(),
            None => format!("unidentified:{}", uuid::Uuid::new_v4()),
        }
    }

    pub fn new(provider: &str, endpoint: &str, account: &str, resource: &str) -> Self {
        use sha2::{Digest, Sha256};
        let endpoint = match reqwest::Url::parse(endpoint) {
            Ok(mut url) => {
                let _ = url.set_username("");
                let _ = url.set_password(None);
                url.set_query(None);
                url.set_fragment(None);
                url.to_string()
            }
            Err(_) => endpoint.to_string(),
        };
        let encoded = serde_json::to_vec(&(provider, endpoint, account, resource))
            .expect("source identity strings serialize");
        Self(hex::encode(Sha256::digest(encoded)))
    }
}

/// A record is a few hundred bytes; anything larger is not one.
const PART_RECORD_MAX_BYTES: u64 = 64 * 1024;

/// The version the record at `record` names, or `None` when there is no
/// readable record there. Only a regular file is read: a symlink or a FIFO
/// planted at the name is no record (and a FIFO would block the open).
async fn read_part_record(record: &Path) -> Option<PartRecord> {
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
    (parsed.format == PART_RECORD_FORMAT).then_some(parsed)
}

/// Write the record at `record`. Whatever is at the name goes first and the
/// record is then created new, so a symlink planted there is replaced, never
/// written through.
async fn write_part_record(
    record: &Path,
    source: &RangeSourceFingerprint,
    provenance: Option<&ResumeSource>,
) -> std::io::Result<()> {
    remove_part_record(record).await;
    let body = serde_json::to_vec(&PartRecord {
        format: PART_RECORD_FORMAT,
        source: source.clone(),
        provenance: provenance.cloned(),
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

/// Completion of abandoned resumable writes. A shared future keeps cleanup
/// alive if its runtime or an awaiting retry is cancelled; another retry can
/// drive the same attempt. Independently live writers are never registered.
mod part_settlement {
    use std::collections::HashMap;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex, OnceLock};

    use futures_util::future::{BoxFuture, Shared};
    use futures_util::FutureExt;

    use super::{fs, ClaimedWriter};
    use tokio::io::AsyncWriteExt;

    type Completion = Shared<BoxFuture<'static, Result<(), Arc<io::Error>>>>;

    #[derive(Clone)]
    struct Pending {
        attempt: Arc<()>,
        completion: Completion,
        retry_record: Option<PathBuf>,
    }

    fn pending() -> &'static Mutex<HashMap<PathBuf, Vec<Pending>>> {
        static PENDING: OnceLock<Mutex<HashMap<PathBuf, Vec<Pending>>>> = OnceLock::new();
        PENDING.get_or_init(Mutex::default)
    }

    /// Resolve the parent, never the part itself: aliases of the directory
    /// share a barrier, and a symlink at the part is still refused by open.
    pub(super) async fn key(path: &Path) -> io::Result<PathBuf> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let parent = fs::canonicalize(parent).await?;
        Ok(parent.join(path.file_name().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "download path has no filename")
        })?))
    }

    pub(super) async fn wait(key: &Path) -> io::Result<()> {
        loop {
            let work: Vec<_> = pending()
                .lock()
                .expect("part settlement mutex poisoned")
                .get(key)
                .cloned()
                .unwrap_or_default();
            if work.is_empty() {
                return Ok(());
            }
            for part in work {
                if let Err(error) = part.completion.await {
                    let Some(record) = part.retry_record else {
                        return Err(super::copy_io_error(&error));
                    };
                    let recovery = {
                        let mut pending = pending().lock().expect("part settlement mutex poisoned");
                        let Some(parts) = pending.get_mut(key) else {
                            continue;
                        };
                        let Some(current) = parts
                            .iter_mut()
                            .find(|p| Arc::ptr_eq(&p.attempt, &part.attempt))
                        else {
                            // Another waiter already owns recovery. Join its
                            // generation on the next pass instead of removing
                            // a record that may belong to a newer writer.
                            continue;
                        };
                        let attempt = Arc::new(());
                        let retry_record = record.clone();
                        let temp = key.to_path_buf();
                        let operation = async move {
                            // Another process may have claimed the part since
                            // the failed owner closed. Invalidate only while
                            // holding the same filesystem claim as an opener.
                            let owner = super::claimed(move || {
                                match super::temp_claim::open_to_append(&temp) {
                                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                                        super::temp_claim::create(&temp)
                                    }
                                    result => result,
                                }
                            })
                            .await
                            .map_err(Arc::new)?;
                            let result = super::forget_part_record(&record).await;
                            drop(owner);
                            result.map_err(Arc::new)
                        }
                        .boxed()
                        .shared();
                        let completion =
                            tracked_completion(key.to_path_buf(), attempt.clone(), operation, true);
                        *current = Pending {
                            attempt,
                            completion: completion.clone(),
                            retry_record: Some(retry_record),
                        };
                        completion
                    };
                    // Leave the shared recovery in the registry if this
                    // waiter is cancelled or removal still fails. Only a
                    // successful invalidation clears its own generation.
                    recovery
                        .await
                        .map_err(|error| super::copy_io_error(&error))?;
                }
            }
        }
    }

    pub(super) fn register(
        key: PathBuf,
        mut writer: ClaimedWriter,
        record: Option<PathBuf>,
        write_failed: bool,
    ) {
        let path = key.clone();
        let retry_record = record.clone();
        let completion = async move {
            let flushed = writer.flush().await;
            // A delayed write error invalidates the recorded prefix. Keep
            // the claim until this record is gone, before a retry can open.
            let invalidated = if write_failed || flushed.is_err() {
                if let Some(record) = record {
                    super::forget_part_record(&record).await
                } else {
                    Ok(())
                }
            } else {
                Ok(())
            };
            if let Err(error) = &flushed {
                tracing::warn!(
                    "abandoned download write failed at {}: {error}",
                    path.display()
                );
            }
            // flush joined the local operation. Closing this owner now
            // explicitly unlocks even if a child inherited its descriptor.
            drop(writer);
            invalidated.map_err(Arc::new)
        }
        .boxed()
        .shared();
        register_completion(key, completion, retry_record);
    }

    /// Finalization owns sync/rename/unlink as well as the file. Those local
    /// operations cannot be rolled back by dropping their await, so retain
    /// their entire owner until they end. An independently live operation is
    /// not registered; only abandoning this await installs its barrier.
    pub(super) async fn finish(
        key: PathBuf,
        operation: impl std::future::Future<Output = io::Result<()>> + Send + 'static,
    ) -> io::Result<()> {
        let completion = operation
            .map(|result| result.map_err(Arc::new))
            .boxed()
            .shared();
        let mut abandoned = Abandoned {
            key,
            completion: Some(completion.clone()),
        };
        let result = completion.await;
        abandoned.completion = None;
        result.map_err(|error| super::copy_io_error(&error))
    }

    struct Abandoned {
        key: PathBuf,
        completion: Option<Completion>,
    }

    impl Drop for Abandoned {
        fn drop(&mut self) {
            if let Some(completion) = self.completion.take() {
                register_completion(self.key.clone(), completion, None);
            }
        }
    }

    fn tracked_completion(
        path: PathBuf,
        generation: Arc<()>,
        operation: Completion,
        retain_failure: bool,
    ) -> Completion {
        async move {
            let result = operation.await;
            if result.is_ok() || !retain_failure {
                let mut pending = pending().lock().expect("part settlement mutex poisoned");
                if let Some(parts) = pending.get_mut(&path) {
                    parts.retain(|part| !Arc::ptr_eq(&part.attempt, &generation));
                    if parts.is_empty() {
                        pending.remove(&path);
                    }
                }
            }
            result
        }
        .boxed()
        .shared()
    }

    fn register_completion(key: PathBuf, operation: Completion, retry_record: Option<PathBuf>) {
        let attempt = Arc::new(());
        let completion = tracked_completion(
            key.clone(),
            attempt.clone(),
            operation,
            retry_record.is_some(),
        );
        pending()
            .lock()
            .expect("part settlement mutex poisoned")
            .entry(key)
            .or_default()
            .push(Pending {
                attempt,
                completion: completion.clone(),
                retry_record,
            });
        // A runtime is not required at Drop: the next open also polls this
        // shared completion. If this task is aborted, the registry retains it.
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                if let Err(error) = completion.await {
                    // A later waiter retries record invalidation. Until it
                    // succeeds, the failed-write prefix stays refused. Normal
                    // finalization errors have already closed their owner.
                    tracing::warn!("could not settle abandoned download part: {error}");
                }
            });
        }
    }
}

fn copy_io_error(error: &std::io::Error) -> std::io::Error {
    error
        .raw_os_error()
        .map(std::io::Error::from_raw_os_error)
        .unwrap_or_else(|| std::io::Error::new(error.kind(), error.to_string()))
}

async fn forget_part_record(record: &Path) -> std::io::Result<()> {
    match fs::remove_file(record).await {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
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
    let Ok(key) = part_settlement::key(&temp).await else {
        return 0;
    };
    if let Err(error) = part_settlement::wait(&key).await {
        tracing::warn!("download part is not safe to resume: {error}");
        return 0;
    }
    let Ok(meta) = fs::symlink_metadata(&temp).await else {
        return 0;
    };
    if meta.is_file()
        && read_part_record(&part_record_path_for(final_path))
            .await
            .is_some_and(|recorded| {
                recorded.provenance.is_some() && recorded.source.pins_a_version()
            })
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
/// writes for as long as it writes; the lock goes with the writer, so it ends
/// with the writer however the writer ends (see [`Claimed`] and
/// [`ClaimedWriter`](super::ClaimedWriter)). A temporary found at the name is
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

    /// A temporary this process holds: its file, and the claim on it.
    ///
    /// The claim ends when this is dropped, unlocked, not when the last
    /// descriptor of the file closes: a child another thread spawns holds a
    /// copy of every descriptor of the process until it execs, and the lock
    /// belongs to the open file description that copy shares, so a claim
    /// ended by the close alone stayed held into the next download of the
    /// file. It is unlocked only where the claim took a lock: where none was
    /// taken there is nothing to release, and on a network mount the unlock
    /// would be a call to the server.
    #[derive(Debug)]
    pub(crate) struct Claimed {
        /// Always present: taken only by [`Self::into_parts`].
        file: Option<std::fs::File>,
        locked: bool,
    }

    impl Claimed {
        fn new(file: std::fs::File, locked: bool) -> Self {
            Self {
                file: Some(file),
                locked,
            }
        }

        /// The claimed file. Only Unix code reads it: elsewhere the claim
        /// takes no lock, and nothing is done through it.
        #[cfg(unix)]
        pub(crate) fn file(&self) -> &std::fs::File {
            self.file.as_ref().expect("a claim keeps its file")
        }

        /// Whether the claim took a lock (see the module doc for where it
        /// does not).
        #[cfg(all(test, target_os = "linux"))]
        pub(crate) fn locked(&self) -> bool {
            self.locked
        }

        /// The file and whether its claim took a lock, for a holder that ends
        /// the claim itself.
        pub(crate) fn into_parts(mut self) -> (std::fs::File, bool) {
            let file = self.file.take().expect("a claim keeps its file");
            (file, self.locked)
        }
    }

    impl Drop for Claimed {
        fn drop(&mut self) {
            if let Some(file) = self.file.as_ref().filter(|_| self.locked) {
                let _ = file.unlock();
            }
        }
    }

    /// Create the temporary at `temp` and claim it. One already there is
    /// replaced when stale, and refused otherwise.
    pub(crate) fn create(temp: &Path) -> Result<Claimed> {
        create_with(temp, try_lock)
    }

    /// Open the temporary at `temp` to go on writing it (a resume), and claim
    /// it: refused while another writer holds it.
    pub(crate) fn open_to_append(temp: &Path) -> Result<Claimed> {
        let file = std::fs::OpenOptions::new()
            .append(true)
            .open(temp)
            .map_err(|e| name_too_long(e, temp))?;
        let locked = claim(&file, temp, try_lock)?;
        Ok(Claimed::new(file, locked))
    }

    /// Create the temporary of a download that starts over (a fresh resumable
    /// part, a segmented run): a stale one is replaced, a live one refused;
    /// where the locks are not used, one found there is discarded, as before
    /// them.
    pub(crate) fn create_fresh(temp: &Path) -> Result<Claimed> {
        create_fresh_with(temp, try_lock, locks_usable)
    }

    fn create_fresh_with(
        temp: &Path,
        lock: impl Fn(&std::fs::File) -> std::result::Result<(), std::fs::TryLockError>,
        usable: impl Fn(&Path) -> bool,
    ) -> Result<Claimed> {
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
        let file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(temp)
            .map_err(|e| name_too_long(e, temp))?;
        Ok(Claimed::new(file, false))
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
    ) -> Result<Claimed> {
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
                    let locked = claim(&file, temp, &lock)?;
                    return Ok(Claimed::new(file, locked));
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

    /// Lock `file`, opened at `path`, for this writer, and say whether a lock
    /// was taken. Refused when another writer holds it, or when the name no
    /// longer points at this file (it was replaced between the open and the
    /// lock); a lock this filesystem cannot take is not an error, and no lock.
    fn claim(
        file: &std::fs::File,
        path: &Path,
        lock: impl Fn(&std::fs::File) -> std::result::Result<(), std::fs::TryLockError>,
    ) -> Result<bool> {
        if !locks_usable(path) {
            return Ok(false);
        }
        match lock(file) {
            Ok(()) if same_file(file, path) => Ok(true),
            Ok(()) => Err(Error::new(
                ErrorKind::AlreadyExists,
                format!(
                    "{} was replaced by another download while it was opened",
                    path.display()
                ),
            )),
            Err(std::fs::TryLockError::WouldBlock) => Err(in_use(path)),
            Err(std::fs::TryLockError::Error(_)) => Ok(false),
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
            remove_leftover_link(temp)?;
            return Ok(Found::Stale);
        }
        if !meta.file_type().is_file() {
            return Ok(Found::Unknown);
        }
        let existing = open_to_probe(temp)?;
        match lock(&existing) {
            Ok(()) => {
                let found = if same_file(&existing, temp) {
                    std::fs::remove_file(temp).map(|()| Found::Stale)
                } else {
                    Ok(Found::Live)
                };
                // Ended by an unlock, not by the close, as a claim is (see
                // `Claimed`): a temporary whose removal failed stays at the
                // name, and a child spawned meanwhile, which holds a copy of
                // the descriptor until it execs, would keep it locked against
                // the next writer.
                let _ = existing.unlock();
                found
            }
            Err(std::fs::TryLockError::WouldBlock) => Ok(Found::Live),
            Err(std::fs::TryLockError::Error(_)) => Ok(Found::Unknown),
        }
    }

    /// Open the temporary at `temp` to take its lock. Read only: the lock
    /// needs no more, and a temporary can be read-only (a download killed
    /// after giving it the source's mode and before its rename). Never
    /// through a link, and without waiting on a FIFO put at the name since it
    /// was looked at.
    fn open_to_probe(temp: &Path) -> Result<std::fs::File> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::custom_flags(
            &mut options,
            libc::O_NOFOLLOW | libc::O_NONBLOCK,
        );
        options.open(temp)
    }

    /// Remove the leftover link at `temp`, if the name still holds one. Two
    /// writers that find the same leftover both go to remove it, and the one
    /// that looked first would remove by its name the temporary the other
    /// made there in between. A link takes no lock, so the look and the
    /// removal are made together under a lock on the folder, held for those
    /// two steps only, which every remover of such a link takes (the delta
    /// writer too, in its copy of these rules). Where the locks are not used
    /// no folder is opened (Windows cannot open one) or locked (a network
    /// mount would lock it on its server): the link is removed if the name
    /// still holds one, as the delta writer does there. So it is where the
    /// folder's lock cannot be taken (see `lock_folder`).
    fn remove_leftover_link(temp: &Path) -> Result<()> {
        remove_leftover_link_with(temp, locks_usable, || {})
    }

    /// [`remove_leftover_link`] with the check of whether the locks are
    /// used, and a step run between the look and the removal, so a test can
    /// set the one and act in the other.
    fn remove_leftover_link_with(
        temp: &Path,
        usable: impl FnOnce(&Path) -> bool,
        between: impl FnOnce(),
    ) -> Result<()> {
        let folder = if usable(temp) {
            lock_folder(temp)
        } else {
            None
        };
        let removed = remove_if_link(temp, between);
        if let Some(folder) = folder {
            // Ended by an unlock, not by the close (see `Claimed`).
            let _ = folder.unlock();
        }
        removed
    }

    /// The folder `temp` is in, opened and locked, or `None` where it cannot
    /// be: a folder this process may write but not read cannot be opened,
    /// and a filesystem can refuse the lock. The removal then goes on
    /// without it, as a claim goes on without a lock it cannot take, rather
    /// than leaving the link to fail every later download of the file.
    fn lock_folder(temp: &Path) -> Option<std::fs::File> {
        let folder = std::fs::File::open(folder_of(temp)).ok()?;
        folder.lock().ok()?;
        Some(folder)
    }

    /// Remove `temp` if the name holds a link, after `between`.
    fn remove_if_link(temp: &Path, between: impl FnOnce()) -> Result<()> {
        if !std::fs::symlink_metadata(temp).is_ok_and(|found| found.is_symlink()) {
            return Ok(());
        }
        between();
        match std::fs::remove_file(temp) {
            Err(gone) if gone.kind() == ErrorKind::NotFound => Ok(()),
            removed => removed,
        }
    }

    /// The folder `path` is in.
    fn folder_of(path: &Path) -> &Path {
        match path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent,
            _ => Path::new("."),
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
        let Ok(dir) = std::ffi::CString::new(folder_of(path).as_os_str().as_bytes()) else {
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

        /// What the name holds when a leftover link is removed is looked at
        /// again: a regular file there now is the temporary another writer
        /// made after removing the same leftover, and is kept.
        #[test]
        fn a_leftover_link_replaced_before_its_removal_is_kept() {
            let dir = tempfile::tempdir().unwrap();
            let temp = dir.path().join("f.bin.aerotmp");
            std::fs::write(&temp, b"the other writer").unwrap();
            remove_leftover_link(&temp).unwrap();
            assert_eq!(std::fs::read(&temp).unwrap(), b"the other writer");
        }

        /// CodeRabbit on #1290: two writers that found the same leftover link
        /// both removed it by name, and the one that looked first could remove
        /// the live temporary the other had made there meanwhile. The other
        /// writer here starts between the first one's look and its removal,
        /// and is given the time to remove the link and make its temporary:
        /// it waits for the folder lock instead.
        #[test]
        fn a_leftover_link_is_removed_while_no_other_writer_can_replace_it() {
            let dir = tempfile::tempdir().unwrap();
            let temp = dir.path().join("f.bin.aerotmp");
            std::os::unix::fs::symlink("sibling.bin", &temp).unwrap();
            let mut other = None;
            remove_leftover_link_with(&temp, locks_usable, || {
                let temp = temp.clone();
                other = Some(std::thread::spawn(move || {
                    remove_leftover_link(&temp).unwrap();
                    std::fs::write(&temp, b"the other writer").unwrap();
                }));
                std::thread::sleep(std::time::Duration::from_millis(200));
            })
            .unwrap();
            other.unwrap().join().unwrap();
            assert_eq!(
                std::fs::read(&temp).map_err(|e| e.kind()),
                Ok(b"the other writer".to_vec()),
                "the other writer's temporary was removed"
            );
        }

        /// Orca on #1290: the folder was opened and locked for a leftover link
        /// wherever the link was found, and Windows cannot open a folder: the
        /// link, which was removed by its name before, would have failed the
        /// download (out of reach while every caller asks first whether the
        /// locks are used). Where they are not, the link is removed while the
        /// folder's lock stays free, and a regular file at the name is still
        /// kept.
        #[test]
        fn a_leftover_link_where_the_locks_are_not_used_is_removed_without_the_folder_lock() {
            let dir = tempfile::tempdir().unwrap();
            let temp = dir.path().join("f.bin.aerotmp");
            std::os::unix::fs::symlink("sibling.bin", &temp).unwrap();
            let other = dir.path().join("g.bin.aerotmp");
            std::fs::write(&other, b"the other writer").unwrap();
            let mut folder_free = None;
            let removed = remove_leftover_link_with(
                &temp,
                |_| false,
                || {
                    let folder = std::fs::File::open(dir.path()).unwrap();
                    let free = folder.try_lock().is_ok();
                    if free {
                        let _ = folder.unlock();
                    }
                    folder_free = Some(free);
                },
            );
            let kept = remove_leftover_link_with(&other, |_| false, || {});
            assert_eq!(removed.map_err(|e| e.kind()), Ok(()));
            assert_eq!(
                folder_free,
                Some(true),
                "the folder was locked where the locks are not used"
            );
            assert_eq!(
                std::fs::symlink_metadata(&temp)
                    .map(|_| ())
                    .map_err(|e| e.kind()),
                Err(ErrorKind::NotFound),
                "the leftover link was kept"
            );
            assert_eq!(kept.map_err(|e| e.kind()), Ok(()));
            assert_eq!(std::fs::read(&other).unwrap(), b"the other writer");
        }

        /// CodeRabbit on #1290: a folder this process may write but not read
        /// cannot be opened, and that made the folder lock an error: a
        /// leftover link there failed every download of the file for as long
        /// as it stayed, where the link was removed by its name before. Such
        /// a folder is one whose lock cannot be taken, and the link is
        /// removed without it, as a claim goes on without a lock it cannot
        /// take.
        #[test]
        fn a_leftover_link_in_a_folder_that_cannot_be_opened_is_replaced() {
            use std::os::unix::fs::PermissionsExt;
            let dir = tempfile::tempdir().unwrap();
            let folder = dir.path().join("write-only");
            std::fs::create_dir(&folder).unwrap();
            let sibling = folder.join("sibling.bin");
            std::fs::write(&sibling, b"the sibling").unwrap();
            let temp = folder.join("f.bin.aerotmp");
            std::os::unix::fs::symlink("sibling.bin", &temp).unwrap();
            std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o300)).unwrap();
            let readable = |folder: &Path| {
                std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o755)).unwrap()
            };
            // Root opens a folder it cannot read: nothing to check there.
            if std::fs::File::open(&folder).is_ok() {
                eprintln!("skipping: this user opens a folder it cannot read");
                readable(&folder);
                return;
            }

            let claim = create(&temp).map(drop);
            readable(&folder);
            assert_eq!(claim.map_err(|e| e.kind()), Ok(()));
            assert!(
                std::fs::symlink_metadata(&temp).is_ok_and(|found| found.is_file()),
                "the claim did not make its temporary in place of the link"
            );
            assert_eq!(std::fs::read(&sibling).unwrap(), b"the sibling");
        }

        /// CodeRabbit on #1290 (the delta writer's copy of these rules): a
        /// stale temporary can be read-only, and its lock was taken through a
        /// writable open, refused: every later download of the file failed on
        /// it until it was removed by hand.
        #[test]
        fn a_read_only_stale_temporary_is_replaced() {
            use std::os::unix::fs::PermissionsExt;
            let dir = tempfile::tempdir().unwrap();
            let temp = dir.path().join("f.bin.aerotmp");
            std::fs::write(&temp, b"left by a killed download").unwrap();
            std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o444)).unwrap();
            let claim = create(&temp).expect("a read-only stale temporary was refused");
            assert_eq!(std::fs::read(&temp).unwrap(), b"");
            drop(claim);
        }

        /// A stale temporary whose removal fails stays at the name. Its lock
        /// ended with the close alone, and a copy of the descriptor (a child
        /// another thread spawned, until it execs; the duplicate kept here)
        /// kept it locked against the next writer of the file.
        #[test]
        fn a_stale_temporary_that_cannot_be_removed_is_left_unlocked() {
            use std::os::unix::fs::PermissionsExt;
            let dir = tempfile::tempdir().unwrap();
            let folder = dir.path().join("read-only");
            std::fs::create_dir(&folder).unwrap();
            let temp = folder.join("f.bin.aerotmp");
            std::fs::write(&temp, b"left by a killed download").unwrap();
            std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o555)).unwrap();
            let writable = |folder: &Path| {
                std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o755)).unwrap()
            };
            // A folder without write permission still takes a removal from
            // root: nothing to check there.
            if std::fs::write(folder.join("probe"), b"").is_ok() {
                eprintln!("skipping: this user removes files from a read-only folder");
                writable(&folder);
                return;
            }
            let inherited = std::cell::RefCell::new(None);
            let lock_and_copy = |file: &std::fs::File| {
                *inherited.borrow_mut() = Some(file.try_clone().unwrap());
                file.try_lock()
            };

            let kept = take_if_stale(&temp, lock_and_copy);
            let probe = std::fs::File::open(&temp).unwrap();
            let free = probe.try_lock().is_ok();
            drop(inherited);
            writable(&folder);
            assert_eq!(
                kept.err().map(|e| e.kind()),
                Some(ErrorKind::PermissionDenied)
            );
            assert!(
                free,
                "the kept temporary stayed locked while a copy of its descriptor was open"
            );
        }

        /// The full `aeroftp-cli` suite of #1279: a claim ended by the close
        /// alone stayed held by a child another test thread spawned, until
        /// that child exec'd, and the next claim of the part was refused. The
        /// copy of the descriptor kept open here is that child's: the claim
        /// still ends when it is dropped.
        #[test]
        fn a_dropped_claim_ends_while_a_copy_of_its_descriptor_is_open() {
            let dir = tempfile::tempdir().unwrap();
            let temp = dir.path().join("f.bin.aerotmp");
            let claim = create(&temp).unwrap();
            assert!(claim.locked(), "a local temporary is locked");
            let inherited = claim.file().try_clone().unwrap();
            drop(claim);
            let again = open_to_append(&temp);
            assert!(
                again.is_ok(),
                "a dropped claim was still held: {:?}",
                again.err()
            );
            drop(inherited);
        }

        /// Where the claim takes no lock it says so, and its drop has nothing
        /// to release (on NFS and SMB, where none is taken, an unlock would be
        /// a call to the server).
        #[test]
        fn a_claim_that_takes_no_lock_says_so() {
            let dir = tempfile::tempdir().unwrap();
            let temp = dir.path().join("f.bin.aerotmp");
            assert!(!create_with(&temp, failing_lock).unwrap().locked());
            let fresh = create_fresh_with(&temp, try_lock, |_| false).unwrap();
            assert!(!fresh.locked());
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
            drop(live);
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
            drop(live);
            let claim = open_to_append(&temp).expect("a stale part");
            drop(claim);
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
    claim: impl FnOnce() -> std::io::Result<temp_claim::Claimed> + Send + 'static,
) -> std::io::Result<ClaimedWriter> {
    let (file, locked) = tokio::task::spawn_blocking(claim)
        .await
        .map_err(std::io::Error::other)??
        .into_parts();
    Ok(ClaimedWriter {
        file: Some(fs::File::from_std(file)),
        locked,
    })
}

/// The file a download writer writes through, and its claim on it (see
/// [`temp_claim::Claimed`], whose rules this keeps for a tokio file).
///
/// The claim ends when this is dropped, unlocked, unless a write is still
/// landing: tokio answers a write once it has handed it to a blocking thread,
/// which holds the file until the write is done, and an unlock then would let
/// another download take the part while that write goes into it. The claim
/// then ends with the close, once the write is done.
pub(crate) struct ClaimedWriter {
    /// Always present: taken only by `Drop`.
    file: Option<fs::File>,
    locked: bool,
}

impl ClaimedWriter {
    /// A file written with no claim (in place, the destination itself).
    fn unclaimed(file: fs::File) -> Self {
        Self {
            file: Some(file),
            locked: false,
        }
    }
}

impl std::ops::Deref for ClaimedWriter {
    type Target = fs::File;

    fn deref(&self) -> &fs::File {
        self.file.as_ref().expect("a writer keeps its file")
    }
}

impl std::ops::DerefMut for ClaimedWriter {
    fn deref_mut(&mut self) -> &mut fs::File {
        self.file.as_mut().expect("a writer keeps its file")
    }
}

impl Drop for ClaimedWriter {
    fn drop(&mut self) {
        let Some(file) = self.file.take().filter(|_| self.locked) else {
            return;
        };
        // `try_into_std` fails exactly while an operation holds the file.
        if let Ok(file) = file.try_into_std() {
            let _ = file.unlock();
        }
    }
}

/// A guard that writes to a temp file and renames on commit.
/// If dropped without calling `commit()`, the temp file is deleted.
/// In inplace mode, writes directly to the final path (no temp, no rename).
pub struct AtomicFile {
    temp_path: PathBuf,
    final_path: PathBuf,
    file: ClaimedWriter,
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

        let settlement_key = part_settlement::key(&temp_path).await?;
        part_settlement::wait(&settlement_key).await?;
        let file = if inplace {
            ClaimedWriter::unclaimed(
                fs::OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(true)
                    .open(&temp_path)
                    .await?,
            )
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
/// On failure: settles and keeps `.aerotmp` for resume. A failed local write
/// invalidates its version record so that it cannot be resumed as healthy.
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
    file: ClaimedWriter,
    settlement_key: PathBuf,
    needs_settlement: bool,
    write_error: Option<std::io::Error>,
    committed: bool,
    /// Byte offset we are resuming from (0 = fresh download).
    offset: u64,
    /// The version the part found on open was started from, by its record.
    recorded: Option<RangeSourceFingerprint>,
    /// The source that owns this part, including its account and remote path.
    provenance: Option<ResumeSource>,
    inplace: bool,
}

impl ResumableFile {
    /// Bind a part to a source before comparing its version. A part of a
    /// different source (or an older record with no provenance) starts over.
    pub async fn bind_source(mut self, source: &ResumeSource) -> std::io::Result<Self> {
        if !self.inplace && self.offset > 0 && self.provenance.as_ref() != Some(source) {
            self = self.start_over().await?;
        }
        self.provenance = Some(source.clone());
        Ok(self)
    }

    pub async fn open_for_source(
        final_path: &str,
        version: &RangeSourceFingerprint,
        source: &ResumeSource,
        explicit_resume: bool,
    ) -> std::io::Result<Self> {
        let file = if explicit_resume {
            Self::open_resume(final_path).await?
        } else {
            Self::open(final_path).await?
        };
        file.bind_source(source).await?.bound_to(version).await
    }
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
        match write_part_record(&self.record_path, source, self.provenance.as_ref()).await {
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
        let provenance = self.provenance.clone();
        // A part that cannot be removed fails the fresh open below, which
        // reports it.
        let _ = self.discard().await;
        let mut fresh = Self::fresh_at(final_path, inplace).await?;
        fresh.provenance = provenance;
        Ok(fresh)
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

        let settlement_key = part_settlement::key(&temp_path).await?;
        part_settlement::wait(&settlement_key).await?;

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
                    fs::OpenOptions::new()
                        .append(true)
                        .open(&temp_path)
                        .await
                        .map(ClaimedWriter::unclaimed)
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
        let (file, offset, recorded, provenance) = if let Some((file, offset)) = resumed {
            let recorded = if inplace {
                None
            } else {
                read_part_record(&record_path).await
            };
            let (recorded, provenance) = match recorded {
                Some(record) => (Some(record.source), record.provenance),
                None => (None, None),
            };
            (file, offset, recorded, provenance)
        } else {
            // Fresh: create new file
            let file = if inplace {
                ClaimedWriter::unclaimed(
                    fs::OpenOptions::new()
                        .create(true)
                        .write(true)
                        .truncate(true)
                        .open(&temp_path)
                        .await?,
                )
            } else {
                let temp = temp_path.clone();
                let file = claimed(move || temp_claim::create(&temp)).await?;
                remove_part_record(&record_path).await;
                file
            };
            (file, 0, None, None)
        };

        Ok(Self {
            temp_path,
            final_path,
            record_path,
            file,
            settlement_key,
            needs_settlement: false,
            write_error: None,
            committed: false,
            offset,
            recorded,
            provenance,
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

        let settlement_key = part_settlement::key(&temp_path).await?;
        part_settlement::wait(&settlement_key).await?;
        let file = if inplace {
            ClaimedWriter::unclaimed(
                fs::OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(true)
                    .open(&temp_path)
                    .await?,
            )
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
            settlement_key,
            needs_settlement: false,
            write_error: None,
            committed: false,
            offset: 0,
            recorded: None,
            provenance: None,
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
        self.needs_settlement = true;
        let written = self.file.write_all(buf).await;
        if let Err(error) = &written {
            self.write_error = Some(copy_io_error(error));
        }
        written
    }

    /// Get a mutable reference to the underlying file for writing.
    pub fn file_mut(&mut self) -> &mut tokio::fs::File {
        self.needs_settlement = true;
        &mut self.file
    }

    /// Join outstanding local writes without publishing or removing the part.
    /// A delayed disk error invalidates its record and overrides the transport
    /// error: the caller must not retry an unreported failed local write.
    pub async fn keep_for_resume(mut self) -> std::io::Result<()> {
        self.flush_for_resume().await
    }

    async fn flush_for_resume(&mut self) -> std::io::Result<()> {
        if let Err(error) = self.file.flush().await {
            self.write_error = Some(error);
        }
        if self.write_error.is_some() && !self.inplace {
            // Leave needs_settlement set until invalidation completes. A
            // cancellation here hands that obligation to the Drop barrier.
            forget_part_record(&self.record_path).await?;
            self.recorded = None;
        }
        self.needs_settlement = false;
        match self.write_error.take() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// Flush and commit: rename temp file to final path.
    /// In inplace mode, no rename is needed. Once local finalization starts,
    /// dropping its await lets it finish; a retry waits before taking the name.
    pub async fn commit(mut self) -> Result<(), std::io::Error> {
        self.flush_for_resume().await?;
        let key = self.settlement_key.clone();
        part_settlement::finish(key, async move {
            self.file.sync_all().await?;
            self.file.shutdown().await?;
            if !self.inplace {
                // The record goes first, while this writer still holds the
                // part: a record removed after rename could be a new writer's.
                remove_part_record(&self.record_path).await;
                fs::rename(&self.temp_path, &self.final_path).await?;
            }
            self.committed = true;
            drop(self);
            Ok(())
        })
        .await
    }

    /// Discard partial data and remove the temp file. If its await is dropped
    /// during local removal, the owner still finishes before a retry can open.
    pub async fn discard(mut self) -> Result<(), std::io::Error> {
        self.flush_for_resume().await?;
        let key = self.settlement_key.clone();
        part_settlement::finish(key, async move {
            self.committed = true;
            self.file.shutdown().await?;
            let removed = if !self.inplace {
                remove_part_record(&self.record_path).await;
                fs::remove_file(&self.temp_path).await
            } else {
                fs::remove_file(&self.final_path).await
            };
            drop(self);
            removed
        })
        .await
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
        // Drop cannot await. An error/cancel/abort may leave a write still
        // landing, so move its owner into registered cleanup before releasing
        // this guard. The next same-process measurement/open awaits only this
        // abandoned attempt; an independent live writer is still refused.
        if self.needs_settlement {
            if let Some(file) = self.file.file.take() {
                let writer = ClaimedWriter {
                    file: Some(file),
                    locked: self.file.locked,
                };
                part_settlement::register(
                    self.settlement_key.clone(),
                    writer,
                    (!self.inplace).then(|| self.record_path.clone()),
                    self.write_error.is_some(),
                );
            }
        }
        // Explicit keep_for_resume joins and closes before it returns. A
        // process/runtime crash instead leaves the usual recoverable part.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_source_is_stable_across_auth_refresh_and_separates_namespaces() {
        assert_eq!(
            ResumeSource::account("profile", Some("account")),
            ResumeSource::account("profile", Some("account"))
        );
        assert_ne!(
            ResumeSource::account("profile", Some("account")),
            ResumeSource::account("profile", Some("other"))
        );
        assert_ne!(
            ResumeSource::account("profile", None),
            ResumeSource::account("profile", None)
        );
        let source = ResumeSource::new(
            "webdav",
            "https://user:password@example.test/base?token=old",
            "Alice",
            "/Folder/file",
        );
        assert_eq!(
            source,
            ResumeSource::new(
                "webdav",
                "https://example.test/base?token=refreshed",
                "Alice",
                "/Folder/file"
            )
        );
        for other in [
            ResumeSource::new("sftp", "https://example.test/base", "Alice", "/Folder/file"),
            ResumeSource::new("webdav", "https://other.test/base", "Alice", "/Folder/file"),
            ResumeSource::new(
                "webdav",
                "https://example.test/base",
                "alice",
                "/Folder/file",
            ),
            ResumeSource::new(
                "webdav",
                "https://example.test/base",
                "Alice",
                "/folder/file",
            ),
        ] {
            assert_ne!(source, other);
        }
        let stored = serde_json::to_string(&source).unwrap();
        assert!(
            !stored.contains("password") && !stored.contains("token") && !stored.contains("Alice")
        );
    }

    #[tokio::test]
    async fn equal_versions_of_different_sources_never_share_a_part() {
        let version = version(10, None, Some("2026-10-19 10:00:00"));
        let original = ResumeSource::new("sftp", "host:22", "Alice", "/a");
        for other in [
            ResumeSource::new("sftp", "host:22", "Alice", "/b"),
            ResumeSource::new("sftp", "other:22", "Alice", "/a"),
            ResumeSource::new("sftp", "host:23", "Alice", "/a"),
            ResumeSource::new("sftp", "host:22", "Bob", "/a"),
        ] {
            for bytes in [&b"AAAA"[..], &b"AAAAAAAAAA"[..]] {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("target");
                let path = path.to_str().unwrap();
                std::fs::write(path, b"old destination").unwrap();
                let mut part = ResumableFile::open_for_source(path, &version, &original, false)
                    .await
                    .unwrap();
                part.write_all(bytes).await.unwrap();
                part.file_mut().flush().await.unwrap();
                drop(part);
                let same = ResumableFile::open_for_source(path, &version, &original, false)
                    .await
                    .unwrap();
                assert_eq!(same.offset(), bytes.len() as u64);
                drop(same);
                let mut fresh = ResumableFile::open_for_source(path, &version, &other, false)
                    .await
                    .unwrap();
                assert_eq!(fresh.offset(), 0);
                assert_eq!(std::fs::read(path).unwrap(), b"old destination");
                fresh.write_all(b"BBBBBBBBBB").await.unwrap();
                fresh.commit().await.unwrap();
                assert_eq!(std::fs::read(path).unwrap(), b"BBBBBBBBBB");
            }
        }
    }

    #[tokio::test]
    async fn legacy_resume_records_restart_without_losing_the_destination() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        let path = target.to_str().unwrap();
        let version = version(10, Some("same-tag"), None);
        let source = ResumeSource::new("sftp", "host:22", "user", "/file");
        for format in [1, PART_RECORD_FORMAT] {
            std::fs::write(path, b"old destination").unwrap();
            std::fs::write(AtomicFile::temp_path_for(&target), b"AAAAAAAAAA").unwrap();
            let record = PartRecord {
                format,
                source: version.clone(),
                provenance: None,
            };
            std::fs::write(
                part_record_path_for(&target),
                serde_json::to_vec(&record).unwrap(),
            )
            .unwrap();
            let fresh = ResumableFile::open_for_source(path, &version, &source, false)
                .await
                .unwrap();
            assert_eq!(fresh.offset(), 0);
            assert_eq!(std::fs::read(path).unwrap(), b"old destination");
            fresh.discard().await.unwrap();
        }
    }

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

    /// Follow-up of #1279: a resumable writer dropped with its part ended its
    /// claim by the close alone, and a child another thread spawned meanwhile,
    /// which holds a copy of every descriptor until it execs, kept it held: the
    /// next download of the file, a retry for one, was refused as another
    /// download writing the part. The copy kept open here is that child's.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_dropped_writers_part_is_resumed_while_a_copy_of_its_descriptor_is_open() {
        use std::os::fd::AsFd;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.bin");
        let path = path.to_str().unwrap();
        let mut part = ResumableFile::open_in(path, false).await.unwrap();
        part.write_all(b"first").await.unwrap();
        part.file_mut().flush().await.unwrap();
        let inherited = part.file_mut().as_fd().try_clone_to_owned().unwrap();
        drop(part);
        let resumed = ResumableFile::open_in(path, false)
            .await
            .expect("the dropped writer's claim was still held");
        assert_eq!(resumed.offset(), 5);
        drop(inherited);
    }

    /// The other side: tokio answers a write once a blocking thread has it,
    /// and that thread holds the file until the write is done. A writer
    /// dropped meanwhile keeps its claim until then; released at the drop, it
    /// let another download take the part while the write still went into it.
    /// A FIFO nobody reads holds the write in flight for as long as needed.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_writer_dropped_with_a_write_in_flight_keeps_its_claim_until_the_write_lands() {
        use std::io::Read;
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::OpenOptionsExt;
        let dir = tempfile::tempdir().unwrap();
        let pipe = dir.path().join("pipe");
        let name = std::ffi::CString::new(pipe.as_os_str().as_bytes()).unwrap();
        // SAFETY: `name` is a NUL-terminated path that outlives the call.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        // Opened to read too, so the open does not wait for a reader.
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&pipe)
            .unwrap();
        file.try_lock().unwrap();
        let mut writer = ClaimedWriter {
            file: Some(fs::File::from_std(file)),
            locked: true,
        };
        // Far more than the pipe holds: the write blocks until it is read.
        let sent = writer.write(&vec![0u8; 1 << 20]).await.unwrap();
        drop(writer);
        let mut probe = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&pipe)
            .unwrap();
        let held_while_writing = matches!(probe.try_lock(), Err(std::fs::TryLockError::WouldBlock));
        // Drained before any assertion: a write left blocked would hang the
        // runtime's shutdown instead of failing the test.
        let mut read = 0;
        let mut buf = vec![0u8; 64 * 1024];
        let mut released = false;
        for _ in 0..10_000 {
            if let Ok(n) = probe.read(&mut buf) {
                read += n;
            }
            if read >= sent && probe.try_lock().is_ok() {
                released = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        assert!(
            held_while_writing,
            "the claim ended while a write was still going into the file"
        );
        assert!(
            released,
            "the claim outlived the write ({read} of {sent} read)"
        );
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
        let provenance = ResumeSource::new("test", "fixture", "account", path);
        let mut part = ResumableFile::open_for_source(path, source, &provenance, false)
            .await
            .unwrap();
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
        assert_eq!(
            read_part_record(&record).await.map(|record| record.source),
            Some(v1.clone())
        );
        std::fs::copy(&record, &target).unwrap();
        std::fs::remove_file(&record).unwrap();
        std::os::unix::fs::symlink(&target, &record).unwrap();
        assert!(read_part_record(&record).await.is_none());
    }
}

/// Requested 95: a retry must observe settled local writes, including when
/// cancellation drops the future rather than returning through its error arm.
#[cfg(test)]
mod resumable_settlement_tests {
    use super::*;
    #[cfg(target_os = "linux")]
    use std::future::pending;
    use std::task::Poll;

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .unwrap()
    }

    /// Queue file writes behind a worker controlled by the test. Always
    /// release it on unwind so a failed assertion cannot hang runtime Drop.
    struct ParkedIo {
        release: Option<std::sync::mpsc::Sender<()>>,
        worker: tokio::task::JoinHandle<()>,
    }

    impl ParkedIo {
        async fn new() -> Self {
            let (release, wait) = std::sync::mpsc::channel();
            let (started, ready) = tokio::sync::oneshot::channel();
            let worker = tokio::task::spawn_blocking(move || {
                let _ = started.send(());
                let _ = wait.recv();
            });
            ready.await.unwrap();
            Self {
                release: Some(release),
                worker,
            }
        }

        async fn finish(mut self) {
            self.release.take().unwrap().send(()).unwrap();
            (&mut self.worker).await.unwrap();
        }
    }

    impl Drop for ParkedIo {
        fn drop(&mut self) {
            if let Some(release) = self.release.take() {
                let _ = release.send(());
            }
        }
    }

    fn broken_response() -> reqwest::Response {
        let body = reqwest::Body::wrap_stream(futures_util::stream::iter([
            Ok::<_, std::io::Error>(b"accepted prefix".to_vec()),
            Err(std::io::Error::other("injected response failure")),
        ]));
        http::Response::builder()
            .status(200)
            .header("Content-Length", 100)
            .header("ETag", "\"recorded-version\"")
            .body(body)
            .unwrap()
            .into()
    }

    /// Runs the real HTTP owner. In-place mode avoids a record write before
    /// the streamed bytes, letting the fixture park exactly the last write.
    async fn failed_http_attempt(
        path: &str,
    ) -> (
        bool,
        Result<super::super::HttpResumeStep, crate::providers::ProviderError>,
    ) {
        let part = ResumableFile::open_fresh_in(path, true).await.unwrap();
        let response = broken_response();
        let parked = ParkedIo::new().await;
        let mut progress = None;
        let mut attempt =
            Box::pin(super::super::HttpResume { part }.accept(response, path, &mut progress));
        let first = futures_util::poll!(attempt.as_mut());
        let returned_early = first.is_ready();
        parked.finish().await;
        let outcome = match first {
            Poll::Ready(result) => result,
            Poll::Pending => attempt.await,
        };
        (returned_early, outcome)
    }

    #[test]
    fn a_failed_http_download_settles_its_last_write_before_returning() {
        runtime().block_on(async {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("file");
            let (early, result) = failed_http_attempt(path.to_str().unwrap()).await;
            let resumed = ResumableFile::open_resume_in(path.to_str().unwrap(), true)
                .await
                .unwrap();
            assert!(result.is_err());
            assert!(
                !early,
                "the HTTP error escaped while its last write was queued"
            );
            assert_eq!(resumed.offset(), b"accepted prefix".len() as u64);
            assert_eq!(std::fs::read(path).unwrap(), b"accepted prefix");
        });
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_failed_http_download_reports_a_delayed_local_write_error() {
        runtime().block_on(async {
            let (_, result) = failed_http_attempt("/dev/full").await;
            assert!(
                matches!(result, Err(crate::providers::ProviderError::IoError(_))),
                "the HTTP body error hid the delayed disk error"
            );
        });
    }

    #[cfg(target_os = "linux")]
    async fn cancelled_part_retry(abort_task: bool, replacement: u8) {
        use std::os::fd::AsFd;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file");
        let name = path.to_str().unwrap();
        let mut entry = crate::providers::RemoteEntry::directory("file".into(), "/file".into());
        entry.is_dir = false;
        entry.size = 100;
        entry.metadata.insert("etag".into(), "\"v1\"".into());
        let version = RangeSourceFingerprint::of(&entry);
        let source = ResumeSource::new("test", "fixture", "account", "/file");
        let mut part = ResumableFile::open_for_source(name, &version, &source, false)
            .await
            .unwrap();
        // As with a child spawned before exec, this copy keeps a lock held
        // after the background write closes unless the writer unlocks it.
        let inherited = part.file_mut().as_fd().try_clone_to_owned().unwrap();
        let key = part.settlement_key.clone();
        let parked = ParkedIo::new().await;
        let (written, accepted) = tokio::sync::oneshot::channel();
        let transfer = async move {
            part.write_all(b"accepted prefix").await.unwrap();
            let _ = written.send(());
            pending::<()>().await;
            drop(part);
        };
        if abort_task {
            let task = tokio::spawn(transfer);
            accepted.await.unwrap();
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
        } else {
            tokio::pin!(transfer);
            tokio::select! {
                biased;
                _ = &mut transfer => unreachable!(),
                _ = accepted => {},
            }
            // transfer drops at the end of this branch, as in a timeout.
        }
        let mut cancelled_waiter = Box::pin(part_settlement::wait(&key));
        assert!(matches!(
            futures_util::poll!(cancelled_waiter.as_mut()),
            Poll::Pending
        ));
        drop(cancelled_waiter);
        let alias = dir.path().join("alias");
        std::os::unix::fs::symlink(dir.path(), &alias).unwrap();
        let alias_path = alias.join("file");
        let mut retry = Box::pin(async {
            let length = resumable_part_len(&path).await;
            let opened = match replacement {
                0 => {
                    ResumableFile::open_for_source(
                        alias_path.to_str().unwrap(),
                        &version,
                        &source,
                        false,
                    )
                    .await
                }
                1 => ResumableFile::open_fresh(name).await,
                _ => {
                    let file = AtomicFile::new(name).await?;
                    file.commit().await?;
                    return Ok::<_, std::io::Error>((length, 0));
                }
            }?;
            Ok((length, opened.offset()))
        });
        // Measure/open while the old write is still pending. No sleeps, no
        // timeout: only release the worker after polling the retry once.
        assert!(matches!(futures_util::poll!(retry.as_mut()), Poll::Pending));
        parked.finish().await;
        let result = retry.await;
        drop(inherited);
        let (length, offset) = result.expect("the abandoned writer kept its claim");
        assert_eq!(length, b"accepted prefix".len() as u64);
        assert_eq!(offset, if replacement == 0 { length } else { 0 });
    }

    #[cfg(target_os = "linux")]
    async fn a_failed_write_invalidates_its_record(explicit: bool) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file");
        std::fs::write(&path, b"previous complete file").unwrap();
        let mut entry = crate::providers::RemoteEntry::directory("file".into(), "/file".into());
        entry.is_dir = false;
        entry.size = 100;
        entry.metadata.insert("etag".into(), "\"v1\"".into());
        let version = RangeSourceFingerprint::of(&entry);
        let source = ResumeSource::new("test", "fixture", "account", "/file");
        let mut part =
            ResumableFile::open_for_source(path.to_str().unwrap(), &version, &source, false)
                .await
                .unwrap();
        part.write_all(b"valid prefix").await.unwrap();
        part.file_mut().flush().await.unwrap();
        // Test-only fault injection: keep the part/record on disk but direct
        // its next write to a real device that reports a delayed ENOSPC.
        let full = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/full")
            .unwrap();
        part.file = ClaimedWriter::unclaimed(fs::File::from_std(full));
        part.write_all(b"unwritten tail").await.unwrap();
        if explicit {
            assert!(part.keep_for_resume().await.is_err());
        } else {
            drop(part);
        }
        assert_eq!(resumable_part_len(&path).await, 0);
        assert!(!part_record_path_for(&path).exists());
        assert_eq!(std::fs::read(path).unwrap(), b"previous complete file");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn an_explicit_failed_write_cannot_advertise_a_recorded_prefix() {
        runtime().block_on(a_failed_write_invalidates_its_record(true));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn an_abandoned_failed_write_cannot_advertise_a_recorded_prefix() {
        runtime().block_on(a_failed_write_invalidates_its_record(false));
    }

    async fn retry_after_failed_record_cleanup(replacement: u8) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file");
        let name = path.to_str().unwrap();
        std::fs::write(&path, b"previous complete file").unwrap();
        let mut entry = crate::providers::RemoteEntry::directory("file".into(), "/file".into());
        entry.is_dir = false;
        entry.size = 100;
        entry.metadata.insert("etag".into(), "\"v1\"".into());
        let version = RangeSourceFingerprint::of(&entry);
        let source = ResumeSource::new("test", "fixture", "account", "/file");
        let mut part = ResumableFile::open_for_source(name, &version, &source, false)
            .await
            .unwrap();
        part.write_all(b"unsafe prefix").await.unwrap();
        part.file_mut().flush().await.unwrap();
        let record = part.record_path.clone();
        let original_record = std::fs::read(&record).unwrap();
        // A directory deterministically refuses remove_file on every platform,
        // including privileged CI users. The failed write must stay refused.
        std::fs::remove_file(&record).unwrap();
        std::fs::create_dir(&record).unwrap();
        part.write_error = Some(std::io::Error::other("injected write failure"));
        let key = part.settlement_key.clone();
        drop(part);
        for _ in 0..2 {
            assert!(ResumableFile::open_fresh(name).await.is_err());
            assert!(
                ResumableFile::open_for_source(name, &version, &source, false)
                    .await
                    .is_err()
            );
            assert_eq!(resumable_part_len(&path).await, 0);
            assert!(record.is_dir(), "failed invalidation cleared the refusal");
        }
        // Restore removability without deleting the stale source record. The
        // next attempt must invalidate it itself, then permit a fresh owner.
        std::fs::remove_dir(&record).unwrap();
        std::fs::write(&record, &original_record).unwrap();
        #[cfg(target_os = "linux")]
        {
            // Model an independent process that bypasses this registry. Its
            // live claim must protect its source record during recovery too.
            let temp = AtomicFile::temp_path_for(&path);
            let live = claimed(move || temp_claim::open_to_append(&temp))
                .await
                .unwrap();
            if live.locked {
                assert!(ResumableFile::open_fresh(name).await.is_err());
                assert_eq!(std::fs::read(&record).unwrap(), original_record);
            }
            drop(live);
        }
        let parked = ParkedIo::new().await;
        let mut cancelled = Box::pin(part_settlement::wait(&key));
        assert!(matches!(
            futures_util::poll!(cancelled.as_mut()),
            Poll::Pending
        ));
        let mut late_waiter = Box::pin(part_settlement::wait(&key));
        assert!(matches!(
            futures_util::poll!(late_waiter.as_mut()),
            Poll::Pending
        ));
        drop(cancelled);
        parked.finish().await;
        let part = match replacement {
            0 => ResumableFile::open_for_source(name, &version, &source, false).await,
            1 => ResumableFile::open_fresh(name).await,
            _ => {
                let atomic = AtomicFile::new(name)
                    .await
                    .expect("resolved record cleanup still blocks an atomic replacement");
                assert!(!record.exists());
                late_waiter.await.unwrap();
                atomic.commit().await.unwrap();
                return;
            }
        }
        .expect("resolved record cleanup still blocks the next download");
        assert_eq!(part.offset(), 0, "resumed a failed-write prefix");
        assert_eq!(std::fs::metadata(&part.temp_path).unwrap().len(), 0);
        // Another lookup cannot remove the new attempt's source record.
        let new_record = std::fs::read(&record).ok();
        late_waiter.await.unwrap();
        part_settlement::wait(&key).await.unwrap();
        assert_eq!(std::fs::read(&record).ok(), new_record);
        assert_eq!(std::fs::read(&path).unwrap(), b"previous complete file");
        part.discard().await.unwrap();
    }

    #[test]
    fn a_resume_recovers_after_failed_record_cleanup() {
        runtime().block_on(retry_after_failed_record_cleanup(0));
    }

    #[test]
    fn a_fresh_download_recovers_after_failed_record_cleanup() {
        runtime().block_on(retry_after_failed_record_cleanup(1));
    }

    #[test]
    fn an_atomic_download_recovers_after_failed_record_cleanup() {
        runtime().block_on(retry_after_failed_record_cleanup(2));
    }

    /// A cancellation may also arrive during the local finalization, after
    /// the write flush but while sync_all still has the descriptor in a worker.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_retry_waits_for_an_abandoned_local_commit() {
        use std::os::fd::AsFd;
        runtime().block_on(async {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("file");
            let mut part = ResumableFile::open_in(path.to_str().unwrap(), false)
                .await
                .unwrap();
            part.write_all(b"complete object").await.unwrap();
            part.file_mut().flush().await.unwrap();
            let inherited = part.file_mut().as_fd().try_clone_to_owned().unwrap();
            let key = part_settlement::key(&AtomicFile::temp_path_for(&path))
                .await
                .unwrap();
            let parked = ParkedIo::new().await;
            let mut commit = Box::pin(part.commit());
            assert!(matches!(
                futures_util::poll!(commit.as_mut()),
                Poll::Pending
            ));
            drop(commit);
            // Drive cleanup while the metadata job is definitely still queued.
            let mut settlement = Box::pin(part_settlement::wait(&key));
            let first = futures_util::poll!(settlement.as_mut());
            let settled_early = first.is_ready();
            parked.finish().await;
            match first {
                Poll::Ready(result) => result,
                Poll::Pending => settlement.await,
            }
            .unwrap();
            let reopened = ResumableFile::open_fresh(path.to_str().unwrap()).await;
            drop(inherited);
            assert!(
                !settled_early,
                "the barrier forgot the pending local finalization"
            );
            reopened.expect("the cancelled commit kept its old claim held");
        });
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_cancelled_part_can_be_resumed_with_an_inherited_descriptor_open() {
        runtime().block_on(cancelled_part_retry(false, 0));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn an_aborted_part_can_be_resumed_with_an_inherited_descriptor_open() {
        runtime().block_on(cancelled_part_retry(true, 0));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_fresh_download_waits_for_an_abandoned_parts_write() {
        runtime().block_on(cancelled_part_retry(false, 1));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn an_atomic_download_waits_for_an_abandoned_parts_write() {
        runtime().block_on(cancelled_part_retry(false, 2));
    }
}
