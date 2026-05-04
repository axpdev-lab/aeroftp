//! W2.3: `StreamingAtomicWriter`: chunk-driven counterpart of
//! `delta_transport_impl::write_atomic_chunked`.
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
//! `delta_transport_impl::temp_path_for` (minus its uniqueness salt).
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

use tokio::fs::{self, OpenOptions};
use tokio::io::AsyncWrite;

use crate::aerorsync::delta_transport_impl::WriteAtomicError;

/// Fixed temp suffix appended to the destination path.
const TEMP_SUFFIX: &str = ".aerotmp";

/// Append `TEMP_SUFFIX` to `target` preserving the original extension.
/// `data.tar.gz` becomes `data.tar.gz.aerotmp`, not `data.tar.aerotmp`.
fn temp_path_for_streaming(target: &Path) -> PathBuf {
    let mut os: OsString = target.as_os_str().to_os_string();
    os.push(TEMP_SUFFIX);
    PathBuf::from(os)
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
    ///   2. drop the handle (some kernels require this before rename
    ///      for cache coherency, mirroring `write_atomic_chunked`).
    ///   3. apply `mode` to the temp (Unix only: silently ignored on
    ///      other platforms because the underlying `set_permissions`
    ///      cannot map the bits faithfully).
    ///   4. apply `mtime` (seconds + nanoseconds) to the temp via the
    ///      `filetime` crate, matching `write_atomic_chunked` semantics.
    ///   5. `rename` temp → target.
    ///
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
    ) -> Result<(), WriteAtomicError> {
        let Self {
            target,
            temp,
            file,
            bytes_written: _,
            mut committed,
        } = self;
        let result = finalize_steps(&target, &temp, file, mode, mtime, &mut committed).await;
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
async fn finalize_steps(
    target: &Path,
    temp: &Path,
    mut file: tokio::fs::File,
    mode: Option<u32>,
    mtime: Option<(i64, u32)>,
    committed: &mut bool,
) -> Result<(), WriteAtomicError> {
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
    // Drop the live handle before rename. Mirrors the comment in
    // `write_atomic_chunked`: some Linux kernels exhibit cache-coherency
    // oddities when renaming a path with an open writer pinned to its
    // inode. Cheap to drop explicitly.
    drop(file);

    #[cfg(unix)]
    if let Some(mode) = mode {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(mode & 0o7777);
        fs::set_permissions(temp, perms)
            .await
            .map_err(|e| WriteAtomicError::PostOpen {
                stage: "chmod",
                source: e,
            })?;
    }
    #[cfg(not(unix))]
    let _ = mode;

    if let Some((secs, nanos)) = mtime {
        let nanos = if nanos < 1_000_000_000 { nanos } else { 0 };
        let ft = filetime::FileTime::from_unix_time(secs, nanos);
        filetime::set_file_mtime(temp, ft).map_err(|e| WriteAtomicError::PostOpen {
            stage: "mtime",
            source: e,
        })?;
    }

    *committed = true;
    fs::rename(temp, target)
        .await
        .map_err(|e| WriteAtomicError::PostOpen {
            stage: "rename",
            source: e,
        })?;
    Ok(())
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
        w.finalize(None, None).await.expect("finalize");

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
        w.finalize(None, None).await.expect("finalize");

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
        w.finalize(Some(0o600), Some(mtime))
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
        w.finalize(None, None).await.expect("finalize");

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
        w2.finalize(None, None).await.expect("finalize");
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
        w.finalize(None, None).await.expect("finalize");

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
        writer.finalize(None, None).await.expect("finalize");

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
        w.finalize(None, None).await.expect("finalize");

        let bytes = tokio::fs::read(&target).await.expect("read");
        assert_eq!(bytes.len(), 16 * 256);
        for (i, chunk) in bytes.chunks(256).enumerate() {
            assert!(
                chunk.iter().all(|b| *b == i as u8),
                "chunk {i} must be uniform"
            );
        }
    }
}
