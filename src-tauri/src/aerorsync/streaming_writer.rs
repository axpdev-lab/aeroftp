//! W2.3: `StreamingAtomicWriter`: chunk-driven counterpart of
//! `write_atomic_chunked`, which lives in this file too (both atomic write
//! paths are owned by the writer module since the A1 crate tranche).
//!
//! Where `write_atomic_chunked` takes a fully-materialized `&[u8]`,
//! `StreamingAtomicWriter` exposes the `AsyncWrite` trait so producers
//! that emit reconstructed bytes incrementally
//! (`engine_adapter::apply_delta_streaming`, W2.2) can write straight to
//! disk without buffering the whole reconstructed file in memory. This
//! is the missing primitive the W2.5 `download_inner` integration needs
//! to delete the `AERORSYNC_MAX_IN_MEMORY_BYTES` cap on the download
//! path.
//!
//! # Atomicity model
//!
//! The writer opens `<target>.aerotmp` and writes to it. The caller
//! drives the bytes through `AsyncWrite::poll_write`. `finalize`
//! commits with the same shape used by `write_atomic_chunked`:
//!
//!   1. `flush` + `sync_all` on the temp.
//!   2. drop the file handle (cross-platform safety on rename).
//!   3. optional `chmod` (Unix only) and `mtime` on the temp.
//!   4. `rename` temp → target. This is the atomic cutover.
//!
//! On a kill-9 between `new()` and `finalize()`:
//! * the original `target` is untouched (we never wrote to it directly),
//! * the `.aerotmp` is left on disk as an orphan,
//! * the next CLI `cleanup` sweep removes orphans.
//!
//! **Drop intentionally does NOT remove the temp file.** Doing so would
//! require synchronous I/O in `Drop`, which is incompatible with the
//! tokio runtime, and would also paper over crashes that the cleanup
//! tool is designed to surface. The orphan is the diagnostic.
//!
//! # Temp path naming
//!
//! The plan documents `target.with_extension("aerotmp")`. That helper
//! *replaces* the extension, which would map both `data.csv` and
//! `data.json` to the same `data.aerotmp` and silently destroy one of
//! them. We instead **append** `.aerotmp` so `data.csv` becomes
//! `data.csv.aerotmp`: same naming convention used by
//! `temp_path_for` (minus its uniqueness salt).
//! The single-`.aerotmp`-per-target shape is intentional: it gives the
//! W2.3 acceptance test 7 (orphan recovery via truncate) a deterministic
//! filename to find, and it matches the kill-9 invariant the test pins.
//!
//! # Concurrency
//!
//! Two writers targeting the same `target` will race on the same
//! `.aerotmp` filename. This is acceptable because the only production
//! caller (W2.5 `download_inner`) is gated by the sync orchestration
//! layer, which never concurrently downloads the same destination file.
//! `write_atomic_chunked` carries a per-instance pid/counter/nanos salt
//! for that reason; `StreamingAtomicWriter` deliberately does not, so the
//! orphan cleanup story stays simple.

#![cfg(feature = "aerorsync")]

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::fs::{self, OpenOptions};
use tokio::io::{AsyncSeekExt, AsyncWrite, AsyncWriteExt};

/// Suffix appended to the destination path while a write is in progress.
/// The rename onto the final path is the atomic commit. Shared with
/// `delta_transport_impl` (symlink temp names and tests) so the two atomic
/// write paths keep one suffix.
pub(crate) const TEMP_SUFFIX: &str = ".aerotmp";

/// Counter used to salt the per-instance temp suffix so two concurrent
/// AeroFTP processes (or two threads in the same app) downloading to the
/// same path do not contend on the same `.aerotmp` filename.
static TEMP_SUFFIX_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Append `TEMP_SUFFIX` to `target` preserving the original extension.
/// `data.tar.gz` becomes `data.tar.gz.aerotmp`, not `data.tar.aerotmp`.
fn temp_path_for_streaming(target: &Path) -> PathBuf {
    let mut os: OsString = target.as_os_str().to_os_string();
    os.push(TEMP_SUFFIX);
    PathBuf::from(os)
}

/// Build a per-invocation temp path. U-14: the suffix carries the
/// process id, a monotonic counter, and the hi-res clock so two
/// concurrent transfers to the same `local_path` do not race on the
/// same `.aerotmp` filename. The shape is still human-readable and
/// collision-recovery friendly for the stale-temp path below.
fn temp_path_for(local: &Path) -> PathBuf {
    let counter = TEMP_SUFFIX_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or_default();
    let suffix = format!(
        "{}.{}.{}.{}",
        TEMP_SUFFIX,
        std::process::id(),
        counter,
        nanos
    );
    let mut os = local.as_os_str().to_os_string();
    os.push(suffix);
    PathBuf::from(os)
}

/// Error type for `write_atomic_chunked`. Splits "temp file never
/// opened" from "temp file partially written" so the caller can pick
/// the right `RsyncError` variant (the former still allows classic
/// fallback; the latter MUST NOT at the rename stage).
#[derive(Debug)]
pub enum WriteAtomicError {
    /// Failed before the temp file was successfully opened: includes
    /// `create_new` contention with a stale `.aerotmp` that could not be
    /// removed and re-opened, and initial metadata errors. No disk state
    /// changed on `local_path`.
    PreOpen(std::io::Error),
    /// Failed after the temp file was opened. `stage` distinguishes
    /// pre-rename failures (target untouched → classic fallback safe,
    /// U-13) from rename failures (user-visible cutover boundary →
    /// hard rejection).
    PostOpen {
        stage: &'static str,
        source: std::io::Error,
    },
}

/// Atomic-ish write of `data` to `local_path`:
///
/// 1. Open `<local_path>.aerotmp.<pid>.<counter>.<nanos>` with
///    `create_new` (U-14 uniqueness). If it already exists (stale from
///    a prior crash), remove it once and retry.
/// 2. Write `data` in chunks of `chunk_size` bytes; optionally sleep
///    `inter_chunk_delay` between chunks (test-only knob used to
///    reproduce a stable mid-write drop window).
/// 3. `sync_all()` the temp file: durability commit on the temp before
///    the rename that makes the new data visible under `local_path`.
/// 4. If `preserve_mode` is provided, apply it to the temp before
///    rename (U-09) so the final inode keeps the caller-specified
///    perms. Skipped silently on non-unix.
/// 5. If `preserve_mtime` is provided, apply it to the temp before
///    rename so the final inode reflects the remote file-list metadata.
/// 6. `rename` onto `local_path`. Atomic within the same filesystem; an
///    `EXDEV` error surfaces as `PostOpen { stage: "rename" }`.
///
/// On any post-open failure the function best-effort `remove_file`s the
/// temp to avoid leaking it. If the caller's future is dropped mid-write
/// the temp may survive on disk but `local_path` is guaranteed to still
/// hold either the original contents or the new contents complete -
/// never half-written bytes (rename-last invariant).
pub async fn write_atomic_chunked(
    local_path: &Path,
    data: &[u8],
    chunk_size: usize,
    inter_chunk_delay: Option<Duration>,
    preserve_mode: Option<u32>,
    preserve_mtime: Option<(i64, Option<i32>)>,
) -> Result<(), WriteAtomicError> {
    write_atomic_chunked_core(
        local_path,
        data,
        chunk_size,
        inter_chunk_delay,
        preserve_mode,
        preserve_mtime,
        false,
    )
    .await
}

/// Sparse variant of [`write_atomic_chunked`]. Identical atomicity,
/// metadata-preservation and kill-9 invariants, but chunks that are
/// entirely zero are turned into filesystem holes (`seek` past them
/// instead of writing zeros) and the final length is fixed with
/// `set_len`, so a trailing run of zeros is also a hole.
///
/// This is the AeroRsync analogue of rsync's `--sparse`: the output is
/// byte-identical on read (a hole reads back as zeros) but consumes
/// fewer allocated blocks for files with large zero regions (VM images,
/// pre-allocated DB files, core dumps). Hole granularity is `chunk_size`
/// (sub-chunk zero runs are written literally), matching rsync's
/// block-granular sparse behaviour. Opt-in only: callers that want the
/// dense representation keep using [`write_atomic_chunked`].
pub async fn write_atomic_chunked_sparse(
    local_path: &Path,
    data: &[u8],
    chunk_size: usize,
    inter_chunk_delay: Option<Duration>,
    preserve_mode: Option<u32>,
    preserve_mtime: Option<(i64, Option<i32>)>,
) -> Result<(), WriteAtomicError> {
    write_atomic_chunked_core(
        local_path,
        data,
        chunk_size,
        inter_chunk_delay,
        preserve_mode,
        preserve_mtime,
        true,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn write_atomic_chunked_core(
    local_path: &Path,
    data: &[u8],
    chunk_size: usize,
    inter_chunk_delay: Option<Duration>,
    preserve_mode: Option<u32>,
    preserve_mtime: Option<(i64, Option<i32>)>,
    sparse: bool,
) -> Result<(), WriteAtomicError> {
    if chunk_size == 0 {
        return Err(WriteAtomicError::PreOpen(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "chunk_size must be > 0",
        )));
    }

    let tmp_path = temp_path_for(local_path);

    // Open with create_new. If a stale `.aerotmp` is in the way, remove
    // it once (this recovers from a prior crash between temp open and
    // rename) and retry. A second `AlreadyExists` is a real conflict -
    // another process is writing concurrently: and we bail with
    // `PreOpen` so the caller can pick a fallback.
    let mut file = match OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&tmp_path)
        .await
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            if let Err(remove_err) = fs::remove_file(&tmp_path).await {
                return Err(WriteAtomicError::PreOpen(remove_err));
            }
            OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&tmp_path)
                .await
                .map_err(WriteAtomicError::PreOpen)?
        }
        Err(e) => return Err(WriteAtomicError::PreOpen(e)),
    };

    let write_result = async {
        let mut offset = 0usize;
        while offset < data.len() {
            let end = (offset + chunk_size).min(data.len());
            let chunk = &data[offset..end];
            if sparse && chunk.iter().all(|&b| b == 0) {
                // Hole: advance the file cursor without writing. The gap
                // becomes an unallocated extent on sparse-capable
                // filesystems. `set_len` below fixes the final size so a
                // trailing hole keeps the correct length. Reads still
                // return zeros, so the file is byte-identical to `data`.
                file.seek(std::io::SeekFrom::Current(chunk.len() as i64))
                    .await
                    .map_err(|e| WriteAtomicError::PostOpen {
                        stage: "seek",
                        source: e,
                    })?;
            } else {
                file.write_all(chunk)
                    .await
                    .map_err(|e| WriteAtomicError::PostOpen {
                        stage: "write",
                        source: e,
                    })?;
            }
            offset = end;
            if let Some(d) = inter_chunk_delay {
                if offset < data.len() {
                    tokio::time::sleep(d).await;
                }
            }
        }
        if sparse {
            // Materialise the exact file length. Required when the file
            // ends on a hole (the last op was a seek, not a write, so
            // the on-disk size would stop at the last written byte). A
            // no-op when the final chunk was written densely.
            file.set_len(data.len() as u64)
                .await
                .map_err(|e| WriteAtomicError::PostOpen {
                    stage: "set_len",
                    source: e,
                })?;
        }
        file.flush().await.map_err(|e| WriteAtomicError::PostOpen {
            stage: "flush",
            source: e,
        })?;
        file.sync_all()
            .await
            .map_err(|e| WriteAtomicError::PostOpen {
                stage: "sync_all",
                source: e,
            })?;
        // Drop the handle before rename: on some Linux kernels a
        // pending-for-rename target behind a still-open write handle can
        // exhibit cache-coherency oddities. Cheap to drop explicitly.
        drop(file);
        // U-09: restore the caller-supplied mode onto the temp file
        // before the rename cutover. Post-rename chmod would be a race;
        // pre-rename chmod is fully atomic with the final inode.
        #[cfg(unix)]
        if let Some(mode) = preserve_mode {
            use std::os::unix::fs::PermissionsExt;
            // Mask to the ordinary rwx bits. `preserve_mode` reaches us from
            // the peer's file list, so the setuid/setgid/sticky bits in
            // 0o7000 are attacker-supplied: a hostile sender could ask us to
            // land a setuid binary. Harmless when we run as an ordinary user
            // (the file is ours, so setuid grants nothing new), but a real
            // escalation when aerorsync runs as root or a service account.
            // rclone reached the same conclusion in GHSA-945v-v9p3-v5xw.
            let perms = std::fs::Permissions::from_mode(mode & 0o0777);
            fs::set_permissions(&tmp_path, perms).await.map_err(|e| {
                WriteAtomicError::PostOpen {
                    stage: "chmod",
                    source: e,
                }
            })?;
        }
        #[cfg(not(unix))]
        let _ = preserve_mode;
        if let Some((secs, nanos)) = preserve_mtime {
            let nanos = nanos
                .filter(|n| (0..1_000_000_000).contains(n))
                .unwrap_or(0) as u32;
            let file_time = filetime::FileTime::from_unix_time(secs, nanos);
            filetime::set_file_mtime(&tmp_path, file_time).map_err(|e| {
                WriteAtomicError::PostOpen {
                    stage: "mtime",
                    source: e,
                }
            })?;
        }
        fs::rename(&tmp_path, local_path)
            .await
            .map_err(|e| WriteAtomicError::PostOpen {
                stage: "rename",
                source: e,
            })?;
        Ok(())
    }
    .await;

    if write_result.is_err() {
        // Best-effort cleanup; errors are swallowed (we are already on
        // the failure path). If rename already succeeded, `tmp_path`
        // is gone and this is a no-op.
        let _ = fs::remove_file(&tmp_path).await;
    }
    write_result
}

/// Streaming counterpart of `write_atomic_chunked`. Accepts incremental
/// `AsyncWrite` calls, commits atomically on `finalize`.
///
/// Constructed via `new(target)`. The caller then drives any
/// `AsyncWrite`-aware producer through it (e.g. the
/// `engine_adapter::apply_delta_streaming` helper, which writes into
/// the writer one delta op at a time). Once the producer signals EOS,
/// the caller invokes `finalize(mode, mtime)` to commit.
///
/// **Always call `finalize` on success.** Dropping without finalizing
/// leaves the `.aerotmp` orphan on disk by design: see the module
/// docstring. The original `target` is never modified by the writer
/// itself, only by the `rename` inside `finalize`.
pub struct StreamingAtomicWriter {
    target: PathBuf,
    temp: PathBuf,
    file: tokio::fs::File,
    bytes_written: u64,
    /// Set to `true` immediately before the rename inside `finalize`.
    /// Because `finalize` consumes `self`, no external observer can
    /// witness a `true` value through `committed()`; the field is kept
    /// for symmetry with `write_atomic_chunked`'s `local_committed`
    /// flag and so a future refactor that splits `finalize` across
    /// state transitions has a place to record the cutover.
    committed: bool,
}

impl StreamingAtomicWriter {
    /// Open `<target>.aerotmp` for writing. If a stale `.aerotmp` from a
    /// previous (crashed) session is in the way, it is truncated rather
    /// than erroring out: this is the idempotent recovery path the W2.3
    /// acceptance test 7 pins.
    ///
    /// The original `target` is **not** opened, modified, or even
    /// stat'd by `new`.
    pub async fn new(target: &Path) -> io::Result<Self> {
        let temp = temp_path_for_streaming(target);
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&temp)
            .await?;
        Ok(Self {
            target: target.to_path_buf(),
            temp,
            file,
            bytes_written: 0,
            committed: false,
        })
    }

    /// Total bytes successfully written through `AsyncWrite::poll_write`.
    /// Updated only when `poll_write` returns `Ready(Ok(n))`.
    pub fn bytes_written(&self) -> u64 {
        self.bytes_written
    }

    /// Whether `finalize` has reached the rename stage. Always `false`
    /// in user-observable scope because `finalize` consumes `self`;
    /// provided for symmetry with `write_atomic_chunked` and so the
    /// W2.3 acceptance test can pin the field's initial state.
    pub fn committed(&self) -> bool {
        self.committed
    }

    /// Path of the in-flight temp file. Exposed for tests and diagnostics.
    pub fn temp_path(&self) -> &Path {
        &self.temp
    }

    /// Commit the temp file as `target`:
    ///
    ///   1. `flush` + `sync_all` on the open handle.
    ///   2. apply `mode` with `fchmod` while the handle is still open
    ///      (Unix only). ACL apply requires chmod first: a later chmod
    ///      would rewrite the mask.
    ///   3. apply the access ACL on the same file descriptor (Linux B2).
    ///   4. drop the handle (some kernels require this before rename
    ///      for cache coherency, mirroring `write_atomic_chunked`).
    ///   5. apply `mtime` (seconds + nanoseconds) to the temp via the
    ///      `filetime` crate, matching `write_atomic_chunked` semantics.
    ///   6. apply `xattrs` to the temp (B3 / X.4). **Before rename**, so
    ///      a kill-9 never leaves a visible target without metadata.
    ///   7. `rename` temp → target.
    ///
    /// Returns any soft metadata-loss warnings (X.5 ENOTSUP path).
    /// Errors map to `WriteAtomicError::PostOpen { stage, source }` so
    /// the caller can route them through the same R3 cutover-boundary
    /// classification as `write_atomic_chunked` (a rename failure is a
    /// hard rejection, not a silent classic fallback).
    ///
    /// On any error, the temp is best-effort removed before returning.
    pub async fn finalize(
        self,
        mode: Option<u32>,
        mtime: Option<(i64, u32)>,
        xattrs: Option<Vec<crate::aerorsync::real_wire::XattrPair>>,
        acls: Option<crate::aerorsync::real_wire::FileListAcls>,
        fail_on_metadata_loss: bool,
    ) -> Result<Vec<String>, WriteAtomicError> {
        let Self {
            target,
            temp,
            file,
            bytes_written: _,
            mut committed,
        } = self;
        let result = finalize_steps(
            &target,
            &temp,
            file,
            mode,
            mtime,
            xattrs.as_deref(),
            acls.as_ref(),
            fail_on_metadata_loss,
            &mut committed,
        )
        .await;
        if result.is_err() {
            // Best-effort cleanup; we are already on the failure path.
            let _ = fs::remove_file(&temp).await;
        }
        result
    }
}

/// Drives the commit pipeline. Split into a free function so `finalize`
/// can consume `self` cleanly (destructuring up front) and still recover
/// the temp path for cleanup on the error arm.
#[allow(clippy::too_many_arguments)]
async fn finalize_steps(
    target: &Path,
    temp: &Path,
    mut file: tokio::fs::File,
    mode: Option<u32>,
    mtime: Option<(i64, u32)>,
    xattrs: Option<&[crate::aerorsync::real_wire::XattrPair]>,
    acls: Option<&crate::aerorsync::real_wire::FileListAcls>,
    fail_on_metadata_loss: bool,
    committed: &mut bool,
) -> Result<Vec<String>, WriteAtomicError> {
    use tokio::io::AsyncWriteExt;

    file.flush().await.map_err(|e| WriteAtomicError::PostOpen {
        stage: "flush",
        source: e,
    })?;
    file.sync_all()
        .await
        .map_err(|e| WriteAtomicError::PostOpen {
            stage: "sync_all",
            source: e,
        })?;

    let mut warnings = Vec::new();
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        if let Some(mode) = mode {
            // Same masking rule as `write_atomic_chunked_core`: `mode` is the
            // peer's, so the setuid/setgid/sticky bits in 0o7000 are
            // attacker-supplied and must not survive onto the file we land.
            let masked = mode & 0o0777;
            // SAFETY: `file` is still open; fchmod on its fd cannot follow a
            // swapped path. The mode has already had setuid/setgid/sticky
            // stripped.
            let rc = unsafe { libc::fchmod(file.as_raw_fd(), masked as libc::mode_t) };
            if rc != 0 {
                return Err(WriteAtomicError::PostOpen {
                    stage: "chmod",
                    source: std::io::Error::last_os_error(),
                });
            }
        }
        if let Some(acls) = acls {
            use crate::aerorsync::acl_fs::{apply_access_acl_fd, AclApplyOutcome};
            // Reconstructing omitted USER/GROUP/OTHER/MASK bits needs the
            // file-list mode. Inventing 0 would fake a 000 ACL and can
            // clobber a chmod we just applied. Download always has a mode
            // when `-A` is on; refuse any other caller.
            let Some(acl_mode) = mode else {
                return Err(WriteAtomicError::PostOpen {
                    stage: "acl",
                    source: std::io::Error::other(
                        "ACL apply requires a file mode to reconstruct omitted object bits",
                    ),
                });
            };
            match apply_access_acl_fd(file.as_raw_fd(), acls, acl_mode, fail_on_metadata_loss) {
                AclApplyOutcome::Applied => {}
                AclApplyOutcome::Unsupported { warning } => warnings.push(warning),
                AclApplyOutcome::Failed { message } => {
                    return Err(WriteAtomicError::PostOpen {
                        stage: "acl",
                        source: std::io::Error::other(message),
                    });
                }
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = mode;
        if let Some(_acls) = acls {
            // Non-Unix never applies POSIX.1e ACLs. Fail closed rather than
            // reconstructing from a synthetic mode 0.
            return Err(WriteAtomicError::PostOpen {
                stage: "acl",
                source: std::io::Error::other(
                    crate::aerorsync::acl_fs::AclFsError::Unsupported.to_string(),
                ),
            });
        }
    }

    // Drop the live handle before rename. Mirrors the comment in
    // `write_atomic_chunked`: some Linux kernels exhibit cache-coherency
    // oddities when renaming a path with an open writer pinned to its
    // inode. Cheap to drop explicitly.
    drop(file);
    finalize_after_acl(
        target,
        temp,
        mtime,
        xattrs,
        fail_on_metadata_loss,
        committed,
        warnings,
    )
    .await
}

async fn finalize_after_acl(
    target: &Path,
    temp: &Path,
    mtime: Option<(i64, u32)>,
    xattrs: Option<&[crate::aerorsync::real_wire::XattrPair]>,
    fail_on_metadata_loss: bool,
    committed: &mut bool,
    mut warnings: Vec<String>,
) -> Result<Vec<String>, WriteAtomicError> {
    if let Some((secs, nanos)) = mtime {
        let nanos = if nanos < 1_000_000_000 { nanos } else { 0 };
        let ft = filetime::FileTime::from_unix_time(secs, nanos);
        filetime::set_file_mtime(temp, ft).map_err(|e| WriteAtomicError::PostOpen {
            stage: "mtime",
            source: e,
        })?;
    }

    // X.4: xattrs on the temp, before rename. Never after: that would
    // open a window where the target is visible without metadata.
    if let Some(pairs) = xattrs {
        use crate::aerorsync::xattr_fs::{apply_xattrs, XattrApplyOutcome};
        match apply_xattrs(temp, pairs, fail_on_metadata_loss) {
            XattrApplyOutcome::Applied { .. } => {}
            XattrApplyOutcome::Unsupported { warnings: w } => warnings.extend(w),
            XattrApplyOutcome::Failed { message } => {
                return Err(WriteAtomicError::PostOpen {
                    stage: "xattr",
                    source: std::io::Error::other(message),
                });
            }
        }
    }

    *committed = true;
    fs::rename(temp, target)
        .await
        .map_err(|e| WriteAtomicError::PostOpen {
            stage: "rename",
            source: e,
        })?;
    Ok(warnings)
}

impl AsyncWrite for StreamingAtomicWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let me = self.get_mut();
        let result = Pin::new(&mut me.file).poll_write(cx, buf);
        if let Poll::Ready(Ok(n)) = &result {
            me.bytes_written += *n as u64;
        }
        result
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let me = self.get_mut();
        Pin::new(&mut me.file).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let me = self.get_mut();
        Pin::new(&mut me.file).poll_shutdown(cx)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aerorsync::engine_adapter::{apply_delta_streaming, EngineDeltaOp, MemoryBaseline};
    use std::time::Duration;
    use tempfile::TempDir;
    use tokio::io::AsyncWriteExt;

    fn fresh_tempdir() -> TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    /// Test 1: the writer's `AsyncWrite` impl is byte-identical to
    /// concatenating the chunks and writing them to the target path.
    #[tokio::test]
    async fn streaming_atomic_writer_round_trips() {
        let dir = fresh_tempdir();
        let target = dir.path().join("out.bin");

        let mut w = StreamingAtomicWriter::new(&target).await.expect("new");
        w.write_all(b"hello ").await.expect("chunk1");
        w.write_all(b"streaming ").await.expect("chunk2");
        w.write_all(b"world").await.expect("chunk3");
        assert_eq!(w.bytes_written(), 21);
        let temp = w.temp_path().to_path_buf();
        w.finalize(None, None, None, None, false)
            .await
            .expect("finalize");

        let bytes = tokio::fs::read(&target).await.expect("read target");
        assert_eq!(bytes, b"hello streaming world");
        assert!(!temp.exists(), "rename must remove the temp file");
    }

    /// Test 2: pre-existing target with different bytes is overwritten
    /// by the rename cutover. The original bytes survive only until the
    /// rename completes; the test asserts the *post-finalize* state.
    #[tokio::test]
    async fn streaming_atomic_writer_overwrite_target() {
        let dir = fresh_tempdir();
        let target = dir.path().join("doc.txt");
        tokio::fs::write(&target, b"OLD BYTES")
            .await
            .expect("seed target");

        let mut w = StreamingAtomicWriter::new(&target).await.expect("new");
        w.write_all(b"NEW PAYLOAD").await.expect("write");
        w.finalize(None, None, None, None, false)
            .await
            .expect("finalize");

        let bytes = tokio::fs::read(&target).await.expect("read");
        assert_eq!(bytes, b"NEW PAYLOAD");
    }

    /// Test 3: kill-9 invariant: drop without finalize must leave the
    /// original target untouched and the `.aerotmp` orphan on disk.
    #[tokio::test]
    async fn streaming_atomic_writer_kill9_invariant_keeps_target() {
        let dir = fresh_tempdir();
        let target = dir.path().join("important.bin");
        tokio::fs::write(&target, b"ORIGINAL_DO_NOT_LOSE")
            .await
            .expect("seed target");

        let temp = temp_path_for_streaming(&target);
        {
            let mut w = StreamingAtomicWriter::new(&target).await.expect("new");
            w.write_all(b"PARTIAL_NEW_BYTES").await.expect("write");
            // Force the in-flight bytes to disk so the orphan assertion
            // below sees the "drop mid-write after partial flush" shape.
            w.flush().await.expect("flush");
            // No finalize: drop the writer here.
        }

        let bytes = tokio::fs::read(&target).await.expect("read target");
        assert_eq!(
            bytes, b"ORIGINAL_DO_NOT_LOSE",
            "target must survive a drop without finalize"
        );
        assert!(
            temp.exists(),
            ".aerotmp must remain as the orphan diagnostic"
        );
    }

    /// Test 4: `finalize(Some(mode), Some(mtime))` reflects on the
    /// final inode. Unix-gated because Windows file mode bits are not
    /// faithful.
    #[cfg(unix)]
    #[tokio::test]
    async fn streaming_atomic_writer_preserves_mode_and_mtime() {
        use std::os::unix::fs::PermissionsExt;

        let dir = fresh_tempdir();
        let target = dir.path().join("perms.bin");

        let mut w = StreamingAtomicWriter::new(&target).await.expect("new");
        w.write_all(b"data").await.expect("write");
        // Use a fixed historical timestamp so the assertion is exact.
        let mtime = (1_700_000_000_i64, 123_456_000_u32);
        w.finalize(Some(0o600), Some(mtime), None, None, false)
            .await
            .expect("finalize");

        let meta = tokio::fs::metadata(&target).await.expect("metadata");
        let mode_bits = meta.permissions().mode() & 0o777;
        assert_eq!(mode_bits, 0o600, "mode must be applied pre-rename");

        // Verify mtime: read it back through the same `filetime` crate
        // we used to set it, to avoid platform discrepancies.
        let ft = filetime::FileTime::from_last_modification_time(&meta);
        assert_eq!(ft.unix_seconds(), mtime.0, "mtime seconds must match");
        assert_eq!(ft.nanoseconds(), mtime.1, "mtime nanoseconds must match");
    }

    /// The streaming path takes the same peer-supplied `preserve_mode` that
    /// `write_atomic_chunked_core` does (see the `.finalize(preserve_mode, ..)`
    /// call in `delta_transport_impl`), so it needs the same guarantee: the
    /// setuid/setgid/sticky bits never survive onto the landed file. The
    /// sibling test above masks with 0o777 and so cannot see these bits;
    /// this one looks at all of 0o7777 on purpose.
    #[cfg(unix)]
    #[tokio::test]
    async fn streaming_atomic_writer_strips_peer_supplied_setuid_setgid_sticky() {
        use std::os::unix::fs::PermissionsExt;

        let dir = fresh_tempdir();

        for (peer_mode, expected) in [
            (0o4755_u32, 0o0755_u32), // setuid
            (0o2755, 0o0755),         // setgid
            (0o1755, 0o0755),         // sticky
            (0o7777, 0o0777),         // all three at once
            (0o0640, 0o0640),         // ordinary bits pass through untouched
        ] {
            let target = dir.path().join(format!("mode-{peer_mode:o}.bin"));
            let mut w = StreamingAtomicWriter::new(&target).await.expect("new");
            w.write_all(b"data").await.expect("write");
            w.finalize(Some(peer_mode), None, None, None, false)
                .await
                .expect("finalize");

            let meta = tokio::fs::metadata(&target).await.expect("metadata");
            let actual = meta.permissions().mode() & 0o7777;
            assert_eq!(
                actual, expected,
                "peer-supplied mode {peer_mode:o} must land as {expected:o}, got {actual:o}"
            );
        }
    }

    /// Test 5: `bytes_written` accumulates accurately across N writes,
    /// including a zero-length write (poll_write may legitimately
    /// return Ready(Ok(0)) for empty buffers; the counter must not
    /// over-count).
    #[tokio::test]
    async fn streaming_atomic_writer_bytes_written_accumulates() {
        let dir = fresh_tempdir();
        let target = dir.path().join("counter.bin");

        let mut w = StreamingAtomicWriter::new(&target).await.expect("new");
        assert_eq!(w.bytes_written(), 0);
        w.write_all(b"abcde").await.expect("w1");
        assert_eq!(w.bytes_written(), 5);
        w.write_all(b"").await.expect("w2-empty");
        assert_eq!(w.bytes_written(), 5);
        w.write_all(b"fghij").await.expect("w3");
        assert_eq!(w.bytes_written(), 10);
        w.finalize(None, None, None, None, false)
            .await
            .expect("finalize");

        let bytes = tokio::fs::read(&target).await.expect("read");
        assert_eq!(bytes, b"abcdefghij");
    }

    /// Test 6: defensive programming pin. `finalize` consumes `self`,
    /// so a "double finalize" is a compile-time impossibility. The test
    /// instead verifies (a) `committed()` returns `false` on a fresh
    /// writer and (b) the writer can be dropped without finalize and a
    /// fresh writer afterwards lands the target normally: i.e. the
    /// no-finalize path leaves no lingering state that would block a
    /// retry.
    #[tokio::test]
    async fn streaming_atomic_writer_double_finalize_errors() {
        let dir = fresh_tempdir();
        let target = dir.path().join("commit.bin");

        let w = StreamingAtomicWriter::new(&target).await.expect("new");
        assert!(!w.committed(), "fresh writer must not report committed");
        // Drop without finalize.
        drop(w);
        assert!(!target.exists(), "target was never written");

        // Build a fresh writer, finalize it, ensure the target lands.
        let w2 = StreamingAtomicWriter::new(&target).await.expect("new2");
        w2.finalize(None, None, None, None, false)
            .await
            .expect("finalize");
        assert!(target.exists(), "target landed after finalize");
    }

    /// Test 7: orphan `.aerotmp` from a previous (crashed) session is
    /// truncated by `new()` rather than failing: the idempotent
    /// recovery path that the cleanup CLI is the long-term solution
    /// for, but that the per-instance `new()` must not block on.
    #[tokio::test]
    async fn streaming_atomic_writer_temp_collision() {
        let dir = fresh_tempdir();
        let target = dir.path().join("contested.bin");
        let temp = temp_path_for_streaming(&target);

        // Pre-seed the temp with junk to simulate a crashed session.
        tokio::fs::write(&temp, b"STALE_JUNK_FROM_PRIOR_SESSION")
            .await
            .expect("seed temp");

        let mut w = StreamingAtomicWriter::new(&target).await.expect("new");
        w.write_all(b"FRESH").await.expect("write");
        w.finalize(None, None, None, None, false)
            .await
            .expect("finalize");

        let bytes = tokio::fs::read(&target).await.expect("read");
        assert_eq!(
            bytes, b"FRESH",
            "stale temp must be truncated, not appended to"
        );
    }

    /// Integration pin: `apply_delta_streaming` (W2.2) drives the
    /// writer end-to-end. This is the production wiring W2.5 will land,
    /// minus the `download_inner` plumbing. Verifies that the producer
    /// + sink composition produces the expected reconstructed file.
    #[tokio::test]
    async fn apply_delta_streaming_into_writer_produces_expected_file() {
        let dir = fresh_tempdir();
        let target = dir.path().join("reconstructed.bin");

        let baseline_bytes: Vec<u8> = (0u8..=200u8).cycle().take(8 * 1024).collect();
        let mut baseline = MemoryBaseline::new(baseline_bytes.clone());
        let block_size: usize = 1024;

        // Mixed op stream: literal head + copy two blocks + literal tail.
        let head_lit: Vec<u8> = b"PROLOGUE-".to_vec();
        let tail_lit: Vec<u8> = b"-EPILOGUE".to_vec();
        let ops = vec![
            EngineDeltaOp::Literal(head_lit.clone()),
            EngineDeltaOp::CopyBlock(0),
            EngineDeltaOp::CopyBlock(2),
            EngineDeltaOp::Literal(tail_lit.clone()),
        ];

        let expected: Vec<u8> = head_lit
            .iter()
            .copied()
            .chain(baseline_bytes[0..block_size].iter().copied())
            .chain(
                baseline_bytes[2 * block_size..3 * block_size]
                    .iter()
                    .copied(),
            )
            .chain(tail_lit.iter().copied())
            .collect();

        let mut writer = StreamingAtomicWriter::new(&target).await.expect("new");
        let n = apply_delta_streaming(&mut baseline, ops, block_size, &mut writer)
            .await
            .expect("apply_delta_streaming");
        assert_eq!(n as usize, expected.len(), "byte count matches");
        assert_eq!(writer.bytes_written(), n);
        writer
            .finalize(None, None, None, None, false)
            .await
            .expect("finalize");

        let on_disk = tokio::fs::read(&target).await.expect("read target");
        assert_eq!(on_disk, expected);
    }

    /// Diagnostic helper: `temp_path_for_streaming` preserves the full
    /// extension chain (the deviation from the plan documented in the
    /// module docstring).
    #[test]
    fn temp_path_appends_suffix_preserves_extension() {
        let target = PathBuf::from("/tmp/data.tar.gz");
        let temp = temp_path_for_streaming(&target);
        assert_eq!(temp, PathBuf::from("/tmp/data.tar.gz.aerotmp"));

        let target_no_ext = PathBuf::from("/tmp/binary");
        let temp_no_ext = temp_path_for_streaming(&target_no_ext);
        assert_eq!(temp_no_ext, PathBuf::from("/tmp/binary.aerotmp"));
    }

    /// Smoke pin against any future buffering regression on
    /// `tokio::fs::File`: interleave writes with tokio yields and
    /// verify the final bytes are intact and ordered.
    #[tokio::test]
    async fn streaming_atomic_writer_survives_tokio_yields() {
        let dir = fresh_tempdir();
        let target = dir.path().join("yielded.bin");

        let mut w = StreamingAtomicWriter::new(&target).await.expect("new");
        for i in 0..16u8 {
            w.write_all(&[i; 256]).await.expect("write");
            tokio::time::sleep(Duration::from_millis(0)).await;
        }
        w.finalize(None, None, None, None, false)
            .await
            .expect("finalize");

        let bytes = tokio::fs::read(&target).await.expect("read");
        assert_eq!(bytes.len(), 16 * 256);
        for (i, chunk) in bytes.chunks(256).enumerate() {
            assert!(
                chunk.iter().all(|b| *b == i as u8),
                "chunk {i} must be uniform"
            );
        }
    }

    /// R4: `apply_xattrs` runs on the temp file **before** the rename, so a
    /// kill-9 never exposes a target that is missing its metadata. Nothing on
    /// the success path can tell the two orders apart, since either way the
    /// final file ends up carrying the attributes. The failure path can: feed
    /// a blob that `apply_xattrs` rejects, and a target that exists afterwards
    /// can only mean the rename had already happened, which is precisely the
    /// block sitting in the wrong place.
    ///
    /// The rejection is `sanitize_xattrs_for_apply` refusing a name outside
    /// the `user.` namespace, which happens before any platform-specific code,
    /// so this pin holds on Windows as well as Unix.
    #[tokio::test]
    async fn xattr_apply_precedes_the_rename_so_a_rejection_leaves_no_target() {
        use crate::aerorsync::real_wire::XattrPair;

        let dir = fresh_tempdir();
        let target = dir.path().join("payload.bin");

        let mut w = StreamingAtomicWriter::new(&target).await.expect("new");
        w.write_all(b"NEW PAYLOAD").await.expect("write");
        let temp = w.temp_path().to_path_buf();

        let poisoned = vec![XattrPair::inline("trusted.nope", b"v".to_vec())];
        let err = w
            .finalize(None, None, Some(poisoned), None, false)
            .await
            .expect_err("a rejected xattr blob must fail finalize");
        match err {
            WriteAtomicError::PostOpen { stage, .. } => assert_eq!(stage, "xattr"),
            other => panic!("expected PostOpen{{stage: \"xattr\"}}, got {other:?}"),
        }

        assert!(
            !target.exists(),
            "target exists after a rejected xattr apply: the rename ran first"
        );
        assert!(
            !temp.exists(),
            "temp must be cleaned up on the failure path"
        );
    }

    /// Same invariant seen from the other side: when the target already
    /// exists, a rejected xattr blob must leave the old bytes in place. If the
    /// apply moved after the rename, the cutover would have happened and the
    /// old content would be gone even though finalize reported a failure.
    #[tokio::test]
    async fn xattr_rejection_does_not_cut_over_an_existing_target() {
        use crate::aerorsync::real_wire::XattrPair;

        let dir = fresh_tempdir();
        let target = dir.path().join("doc.txt");
        tokio::fs::write(&target, b"OLD BYTES")
            .await
            .expect("seed target");

        let mut w = StreamingAtomicWriter::new(&target).await.expect("new");
        w.write_all(b"NEW PAYLOAD").await.expect("write");

        let poisoned = vec![XattrPair::inline("trusted.nope", b"v".to_vec())];
        w.finalize(None, None, Some(poisoned), None, false)
            .await
            .expect_err("a rejected xattr blob must fail finalize");

        let bytes = tokio::fs::read(&target).await.expect("read target");
        assert_eq!(
            bytes, b"OLD BYTES",
            "the target was cut over despite the xattr rejection"
        );
    }

    #[tokio::test]
    async fn acl_apply_without_mode_fails_closed_and_leaves_target() {
        use crate::aerorsync::acl_fs::filesystem_acl_to_wire;
        use crate::aerorsync::real_wire::{AclNamedEntry, AclPrincipal, RsyncAcl};

        let dir = fresh_tempdir();
        let target = dir.path().join("doc.txt");
        tokio::fs::write(&target, b"OLD BYTES")
            .await
            .expect("seed target");

        let mut w = StreamingAtomicWriter::new(&target).await.expect("new");
        w.write_all(b"NEW PAYLOAD").await.expect("write");
        let temp = w.temp_path().to_path_buf();
        let acls = filesystem_acl_to_wire(
            RsyncAcl {
                user_obj: Some(6),
                group_obj: Some(4),
                mask_obj: Some(4),
                other_obj: Some(4),
                names: vec![AclNamedEntry {
                    id: 65534,
                    principal: AclPrincipal::User,
                    access: 4,
                    name: None,
                }],
            },
            0o100_644,
        )
        .expect("wire");
        let err = w
            .finalize(None, None, None, Some(acls), true)
            .await
            .expect_err("ACL apply without a mode must fail closed");
        match err {
            WriteAtomicError::PostOpen { stage, source } => {
                assert_eq!(stage, "acl");
                assert!(
                    source.to_string().contains("file mode"),
                    "error should name the missing mode, got {source}"
                );
            }
            other => panic!("expected PostOpen{{stage: \"acl\"}}, got {other:?}"),
        }
        let bytes = tokio::fs::read(&target).await.expect("read target");
        assert_eq!(bytes, b"OLD BYTES");
        assert!(!temp.exists(), "temp must be cleaned up on ACL failure");
    }

    #[tokio::test]
    async fn acl_hard_failure_leaves_existing_target_and_removes_temp() {
        use crate::aerorsync::real_wire::{AclWireEntry, FileListAcls};

        let dir = fresh_tempdir();
        let target = dir.path().join("doc.txt");
        tokio::fs::write(&target, b"OLD BYTES")
            .await
            .expect("seed target");

        let mut w = StreamingAtomicWriter::new(&target).await.expect("new");
        w.write_all(b"NEW PAYLOAD").await.expect("write");
        let temp = w.temp_path().to_path_buf();
        let acls = FileListAcls {
            access: AclWireEntry::Reference(0),
            default: None,
        };
        let err = w
            .finalize(Some(0o644), None, None, Some(acls), true)
            .await
            .expect_err("unresolved ACL reference must fail finalize");
        match err {
            WriteAtomicError::PostOpen { stage, .. } => assert_eq!(stage, "acl"),
            other => panic!("expected PostOpen{{stage: \"acl\"}}, got {other:?}"),
        }
        let bytes = tokio::fs::read(&target).await.expect("read target");
        assert_eq!(bytes, b"OLD BYTES");
        assert!(!temp.exists(), "temp must be cleaned up on ACL failure");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn finalize_applies_chmod_then_acl_before_rename() {
        use crate::aerorsync::acl_fs::{
            apply_access_acl_fd, filesystem_acl_to_wire, read_access_acl_model_fd, AclApplyOutcome,
        };
        use crate::aerorsync::real_wire::{AclNamedEntry, AclPrincipal, RsyncAcl};
        use std::os::unix::fs::PermissionsExt;
        use std::os::unix::io::AsRawFd;

        let dir = fresh_tempdir();
        let probe = tempfile::tempfile().expect("probe");
        let src = RsyncAcl {
            user_obj: Some(6),
            group_obj: Some(4),
            mask_obj: Some(4),
            other_obj: Some(0),
            names: vec![AclNamedEntry {
                id: 65534,
                principal: AclPrincipal::User,
                access: 4,
                name: None,
            }],
        };
        let acls = filesystem_acl_to_wire(src, 0o100_640).expect("wire");
        match apply_access_acl_fd(probe.as_raw_fd(), &acls, 0o100_640, true) {
            AclApplyOutcome::Applied => {}
            AclApplyOutcome::Unsupported { warning } => {
                eprintln!("skipping finalize ACL test: {warning}");
                return;
            }
            AclApplyOutcome::Failed { message } => {
                eprintln!("skipping finalize ACL test: {message}");
                return;
            }
        }

        let target = dir.path().join("payload.bin");
        let mut w = StreamingAtomicWriter::new(&target).await.expect("new");
        w.write_all(b"ACL DATA").await.expect("write");
        w.finalize(Some(0o640), None, None, Some(acls.clone()), true)
            .await
            .expect("finalize");

        let file = std::fs::File::open(&target).expect("open target");
        let mode = file.metadata().expect("meta").permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o640,
            "chmod then ACL with matching mask must keep 0640 through rename"
        );
        let reread = read_access_acl_model_fd(file.as_raw_fd()).expect("reread");
        assert_eq!(reread.mask_obj, Some(4));
        assert!(reread.names.iter().any(|n| n.id == 65534 && n.access == 4));
    }

    #[tokio::test]
    async fn temp_path_for_is_unique_per_invocation() {
        // U-14 regression pin: two calls with the same target produce
        // distinct temp paths so concurrent writers do not race.
        let target = Path::new("/tmp/does-not-exist.bin");
        let a = temp_path_for(target);
        let b = temp_path_for(target);
        assert_ne!(a, b, "concurrent writers must get distinct temp paths");
    }
}
