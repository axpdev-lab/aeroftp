//! A4 — `NativeRsyncDeltaTransport`: production-facing `DeltaTransport`
//! implementation backed by the Strada C native rsync driver.
//!
//! The module is the bridge between the prototype driver
//! (`NativeRsyncDriver` + `RsyncEventBridge`) and the production
//! `crate::delta_transport::DeltaTransport` trait consumed by the sync
//! loop. It owns:
//!
//! - Construction of the SSH transport, driver, adapter, and bridge for
//!   each individual transfer (no cross-transfer session caching — the
//!   trait is `&self`, so we avoid locking altogether).
//! - Translation of typed `NativeRsyncError` into `RsyncError` through
//!   the `fallback_policy::classify_fallback` matrix. HardError variants
//!   land in `RsyncError::HardRejection`, which
//!   `delta_sync_rsync::transfer_with_delta` now routes to
//!   `DeltaSyncResult::hard_error` instead of the usual silent fallback.
//!   This plugs the last R4 gap: HostKeyRejected (and all other
//!   HardError kinds) no longer degrade to the classic-SFTP path
//!   silently.
//! - Atomic disk write of the download result via a temp-file + rename
//!   helper with kill-9 invariant pin (`write_atomic_chunked`).
//!
//! # Q5 PreCommit / PostCommit semantics (recap)
//!
//! The driver flips `committed = true` when it writes the first outbound
//! delta byte. The A4 adapter additionally tracks a `local_committed`
//! boolean through `write_atomic_chunked`: once the temp file is open,
//! subsequent failures must NOT silently fall back to classic (the disk
//! has been touched). `WriteAtomicError::PostOpen` surfaces as a
//! `HardRejection`; `WriteAtomicError::PreOpen` surfaces as `Io` (which
//! the wrapper still treats as fallback).
//!
//! # In-memory limitations (tracked risks)
//!
//! - R2: upload reads the source file into RAM. Accepted in the
//!   prototype single-file scope; streamed upload is S8j.
//! - R3: download decodes into a `Vec<u8>` that A4 buffers before the
//!   temp-file write. Accepted for the same reason.

#![cfg(feature = "proto_native_rsync")]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::fs::{self, OpenOptions};
use tokio::io::AsyncWriteExt;

use crate::delta_transport::DeltaTransport;
use crate::rsync_native_proto::engine_adapter::CurrentDeltaSyncBridge;
use crate::rsync_native_proto::fallback_policy::{classify_fallback, FallbackVerdict};
use crate::rsync_native_proto::native_driver::NativeRsyncDriver;
use crate::rsync_native_proto::real_wire::FileListEntry;
use crate::rsync_native_proto::remote_command::RemoteCommandSpec;
use crate::rsync_native_proto::rsync_event_bridge::RsyncEventBridge;
use crate::rsync_native_proto::ssh_transport::{
    SshHostKeyPolicy, SshRemoteShellTransport, SshTransportConfig,
};
use crate::rsync_native_proto::transport::{CancelHandle, RemoteExecRequest};
use crate::rsync_native_proto::types::{NativeRsyncError, SessionStats};
use crate::rsync_output::RsyncEvent;
use crate::rsync_over_ssh::{RsyncCapability, RsyncConfig, RsyncError, RsyncStats};

/// Display name surfaced by `DeltaTransport::name()`.
const NATIVE_TRANSPORT_NAME: &str = "native-rsync-proto-31";

/// Chunk size used by `write_atomic_chunked` in production. 64 KiB
/// matches the AeroVault v2 body chunk + keeps syscall count reasonable.
const ATOMIC_WRITE_CHUNK_SIZE: usize = 64 * 1024;

/// Suffix appended to the destination path while the write is in
/// progress. The rename onto the final path is the atomic commit.
const TEMP_SUFFIX: &str = ".aerotmp";

/// `DeltaTransport` impl driven by the prototype native rsync driver.
///
/// One instance is cheap to construct and safe to share across many
/// transfers; each `upload` / `download` call builds its own SSH session
/// and driver so the trait methods can remain `&self`.
pub struct NativeRsyncDeltaTransport {
    ssh_config: SshTransportConfig,
    min_file_size: u64,
}

impl NativeRsyncDeltaTransport {
    /// Primary constructor — takes a fully-populated SSH config and the
    /// size threshold below which delta is declined.
    pub fn new(ssh_config: SshTransportConfig, min_file_size: u64) -> Self {
        Self {
            ssh_config,
            min_file_size,
        }
    }

    /// Convenience constructor that maps the production `RsyncConfig`
    /// (used by `providers::sftp::delta_transport`) onto the prototype's
    /// `SshTransportConfig`. `host_key_policy` is provided by the caller
    /// so the factory (Zona B1) can honour whatever pinning the SFTP
    /// session established during connect.
    pub fn from_rsync_config(
        cfg: &RsyncConfig,
        host_key_policy: SshHostKeyPolicy,
    ) -> Result<Self, RsyncError> {
        let key_path = cfg
            .ssh_key_path
            .clone()
            .ok_or_else(|| RsyncError::MissingKey("no ssh key path configured".into()))?;
        let ssh_config = SshTransportConfig {
            host: cfg.ssh_host.clone(),
            port: cfg.ssh_port.unwrap_or(22),
            username: cfg.ssh_user.clone(),
            private_key_path: key_path,
            connect_timeout_ms: 10_000,
            io_timeout_ms: 30_000,
            worker_idle_poll_ms: 250,
            max_frame_size: 1 << 20,
            host_key_policy,
            // `probe_request` is unused while `probe_remote` returns a
            // hardcoded capability. Kept populated so a future real probe
            // (S8j) can drop in without touching this factory.
            probe_request: RemoteExecRequest {
                program: "/opt/rsnp/bin/rsync_proto_serve".into(),
                args: vec!["--probe".into()],
                environment: Vec::new(),
            },
        };
        Ok(Self::new(ssh_config, cfg.min_file_size))
    }
}

#[async_trait]
impl DeltaTransport for NativeRsyncDeltaTransport {
    fn name(&self) -> &'static str {
        NATIVE_TRANSPORT_NAME
    }

    async fn probe_remote(&self) -> Result<RsyncCapability, RsyncError> {
        // R7 accepted: a real exec-probe would open a second SSH session
        // solely to check that `rsync_proto_serve` is installed. In
        // practice the first `upload`/`download` discovers absence (via
        // `open_raw_stream` TransportFailure) and the fallback policy
        // routes to classic. S8j will revisit if stats show the hardcode
        // hides frequent configuration errors.
        Ok(RsyncCapability {
            version: "native-rsync-prototype".into(),
            protocol: 31,
        })
    }

    async fn probe_local(&self) -> Result<(), RsyncError> {
        Ok(())
    }

    async fn download(
        &self,
        remote_path: &str,
        local_path: &Path,
    ) -> Result<RsyncStats, RsyncError> {
        self.download_inner(remote_path, local_path).await
    }

    async fn upload(
        &self,
        local_path: &Path,
        remote_path: &str,
    ) -> Result<RsyncStats, RsyncError> {
        self.upload_inner(local_path, remote_path).await
    }
}

// --- upload flow ---------------------------------------------------------

impl NativeRsyncDeltaTransport {
    async fn upload_inner(
        &self,
        local_path: &Path,
        remote_path: &str,
    ) -> Result<RsyncStats, RsyncError> {
        let start = Instant::now();
        let metadata = fs::metadata(local_path).await.map_err(RsyncError::Io)?;
        let file_size = metadata.len();
        if file_size < self.min_file_size {
            return Err(RsyncError::TooSmall {
                size: file_size,
                threshold: self.min_file_size,
            });
        }
        // R2: buffered in-memory source. Accepted for prototype.
        let source_data = fs::read(local_path).await.map_err(RsyncError::Io)?;
        let source_entry = build_source_entry(local_path, file_size);

        let transport = SshRemoteShellTransport::new(self.ssh_config.clone());
        let cancel = CancelHandle::inert();
        let mut driver = NativeRsyncDriver::new(transport, cancel);
        let adapter = CurrentDeltaSyncBridge::new();
        let warnings = new_warnings_sink();
        let mut bridge = build_event_bridge(warnings.clone());

        let spec = RemoteCommandSpec::native_upload(remote_path);
        let drive_res = driver
            .drive_upload_through_delta(spec, source_entry, &source_data, &adapter, &mut bridge)
            .await;
        if let Err(e) = drive_res {
            return Err(map_native_error_to_rsync(e, driver.committed()));
        }
        if let Err(e) = driver.finish_session(&mut bridge).await {
            return Err(map_native_error_to_rsync(e, driver.committed()));
        }

        let duration_ms = start.elapsed().as_millis() as u64;
        let warnings = drain_warnings(warnings);
        Ok(build_stats(
            driver.session_stats(),
            file_size,
            duration_ms,
            warnings,
        ))
    }
}

// --- download flow -------------------------------------------------------

impl NativeRsyncDeltaTransport {
    async fn download_inner(
        &self,
        remote_path: &str,
        local_path: &Path,
    ) -> Result<RsyncStats, RsyncError> {
        let start = Instant::now();
        // R3: read existing local file as delta baseline. `unwrap_or_default`
        // handles the "file does not exist yet" case with an empty baseline
        // (classic full download behaviour).
        let destination_data = fs::read(local_path).await.unwrap_or_default();

        let transport = SshRemoteShellTransport::new(self.ssh_config.clone());
        let cancel = CancelHandle::inert();
        let mut driver = NativeRsyncDriver::new(transport, cancel);
        let adapter = CurrentDeltaSyncBridge::new();
        let warnings = new_warnings_sink();
        let mut bridge = build_event_bridge(warnings.clone());

        let spec = RemoteCommandSpec::native_download(remote_path);
        let drive_res = driver
            .drive_download_through_delta(spec, &destination_data, &adapter, &mut bridge)
            .await;
        if let Err(e) = drive_res {
            return Err(map_native_error_to_rsync(e, driver.committed()));
        }
        if let Err(e) = driver.finish_session(&mut bridge).await {
            return Err(map_native_error_to_rsync(e, driver.committed()));
        }

        let reconstructed = driver
            .reconstructed()
            .ok_or_else(|| {
                RsyncError::HardRejection(
                    "native driver finished without reconstructed bytes".into(),
                )
            })?
            .to_vec();
        let file_size = reconstructed.len() as u64;

        write_atomic_chunked(
            local_path,
            &reconstructed,
            ATOMIC_WRITE_CHUNK_SIZE,
            None,
        )
        .await
        .map_err(map_write_atomic_error)?;

        let duration_ms = start.elapsed().as_millis() as u64;
        let warnings = drain_warnings(warnings);
        Ok(build_stats(
            driver.session_stats(),
            file_size,
            duration_ms,
            warnings,
        ))
    }
}

// --- helpers -------------------------------------------------------------

/// Build the single-file `FileListEntry` for the upload path. Flags
/// mirror the A2.x `sample_file_list_entry` baseline so the server sees
/// a shape that exercises the same encoder we pinned against the frozen
/// oracle.
fn build_source_entry(local_path: &Path, size: u64) -> FileListEntry {
    // Flag values match `rsync 3.2.7 flist.c`: XMIT_LONG_NAME (0x0040),
    // XMIT_SAME_MODE (0x0002), XMIT_SAME_UID (0x0008), XMIT_SAME_GID
    // (0x0010), XMIT_SAME_TIME (0x0080). Cumulative = 0x00DA.
    const BASELINE_FLAGS: u32 = 0x0040 | 0x0002 | 0x0008 | 0x0010 | 0x0080;
    let name = local_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("source.bin")
        .to_string();
    FileListEntry {
        flags: BASELINE_FLAGS,
        path: name,
        size: size as i64,
        mtime: 0,
        mtime_nsec: None,
        mode: 0,
        uid: None,
        uid_name: None,
        gid: None,
        gid_name: None,
        checksum: Vec::new(),
    }
}

fn new_warnings_sink() -> Arc<StdMutex<Vec<String>>> {
    Arc::new(StdMutex::new(Vec::new()))
}

/// Construct an `RsyncEventBridge` that funnels `RsyncEvent::Warning`
/// messages into the shared `Vec<String>`. Non-warning events are still
/// emitted to the bridge's internal counters but discarded here (the
/// production UI wiring for them is Zona B4 scope).
fn build_event_bridge(
    warnings: Arc<StdMutex<Vec<String>>>,
) -> RsyncEventBridge<impl FnMut(RsyncEvent) + Send> {
    let warnings_for_closure = warnings;
    RsyncEventBridge::new(move |ev: RsyncEvent| {
        if let RsyncEvent::Warning { message } = ev {
            if let Ok(mut v) = warnings_for_closure.lock() {
                v.push(message);
            }
        }
    })
}

fn drain_warnings(handle: Arc<StdMutex<Vec<String>>>) -> Vec<String> {
    match Arc::try_unwrap(handle) {
        Ok(mutex) => mutex.into_inner().unwrap_or_default(),
        Err(shared) => shared
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default(),
    }
}

fn build_stats(
    stats: &SessionStats,
    total_size: u64,
    duration_ms: u64,
    warnings: Vec<String>,
) -> RsyncStats {
    let speedup = if stats.bytes_sent > 0 {
        total_size as f64 / stats.bytes_sent as f64
    } else {
        1.0
    };
    RsyncStats {
        bytes_sent: stats.bytes_sent,
        bytes_received: stats.bytes_received,
        total_size,
        speedup,
        duration_ms,
        warnings,
    }
}

/// Translate a typed `NativeRsyncError` into the production `RsyncError`
/// by consulting the fallback policy matrix. The resulting variant drives
/// downstream semantics through `delta_sync_rsync::transfer_with_delta`:
///
/// - `FallbackVerdict::Cancel` → `RsyncError::Cancelled` →
///   `DeltaSyncResult::fallback` (`transfer_with_delta` folds it into
///   the generic-fallback catch-all; the sync loop surfaces it as a
///   cancelled transfer).
/// - `FallbackVerdict::AttemptClassicSftpFallback` →
///   `RsyncError::TransferFailed { exit: -1, stderr: ... }` →
///   `DeltaSyncResult::fallback` → classic SFTP transparently.
/// - `FallbackVerdict::HardError` → `RsyncError::HardRejection(...)` →
///   `DeltaSyncResult::hard_error` → surfaced to the user, classic
///   fallback suppressed. This is the R4 solution.
fn map_native_error_to_rsync(err: NativeRsyncError, committed: bool) -> RsyncError {
    match classify_fallback(&err, committed) {
        FallbackVerdict::Cancel => RsyncError::Cancelled,
        FallbackVerdict::AttemptClassicSftpFallback => RsyncError::TransferFailed {
            exit: -1,
            stderr: format!("native fallback ({:?}): {}", err.kind, err.detail),
        },
        FallbackVerdict::HardError => RsyncError::HardRejection(format!(
            "native hard rejection ({:?}): {}",
            err.kind, err.detail
        )),
    }
}

fn map_write_atomic_error(err: WriteAtomicError) -> RsyncError {
    match err {
        // Pre-open: nothing touched on disk yet → treat as Io, the
        // wrapper degrades to classic fallback for free.
        WriteAtomicError::PreOpen(io) => RsyncError::Io(io),
        // Post-open: the temp file may have partial contents. Retrying
        // via classic-SFTP on the same destination path is unsafe
        // without explicit user acknowledgement → HardRejection.
        WriteAtomicError::PostOpen { stage, source } => RsyncError::HardRejection(format!(
            "atomic write failed at {}: {}",
            stage, source
        )),
    }
}

fn temp_path_for(local: &Path) -> PathBuf {
    let mut os = local.as_os_str().to_os_string();
    os.push(TEMP_SUFFIX);
    PathBuf::from(os)
}

/// Error type for `write_atomic_chunked`. Splits "temp file never
/// opened" from "temp file partially written" so the caller can pick
/// the right `RsyncError` variant (the former still allows classic
/// fallback; the latter MUST NOT).
#[derive(Debug)]
pub enum WriteAtomicError {
    /// Failed before the temp file was successfully opened — includes
    /// `create_new` contention with a stale `.aerotmp` that could not be
    /// removed and re-opened, and initial metadata errors. No disk state
    /// changed on `local_path`.
    PreOpen(std::io::Error),
    /// Failed after the temp file was opened. The temp may or may not
    /// still exist on disk; `local_path` is guaranteed untouched because
    /// the rename is the last operation. Retrying via classic SFTP on
    /// the same destination is unsafe.
    PostOpen {
        stage: &'static str,
        source: std::io::Error,
    },
}

/// Atomic-ish write of `data` to `local_path`:
///
/// 1. Open `<local_path>.aerotmp` with `create_new`. If it already
///    exists (stale from a prior crash), remove it once and retry.
/// 2. Write `data` in chunks of `chunk_size` bytes; optionally sleep
///    `inter_chunk_delay` between chunks (test-only knob used to
///    produce a stable kill-9 simulation window).
/// 3. `sync_all()` the temp file — durability commit on the temp before
///    the rename that makes the new data visible under `local_path`.
/// 4. `rename` onto `local_path`. Atomic within the same filesystem; an
///    `EXDEV` error surfaces as `PostOpen { stage: "rename" }`.
///
/// On any post-open failure the function best-effort `remove_file`s the
/// temp to avoid leaking it. If the caller's future is dropped mid-write
/// (kill-9 simulation), the temp may survive on disk but `local_path` is
/// guaranteed to still hold either the original contents or the new
/// contents complete — never half-written bytes.
pub async fn write_atomic_chunked(
    local_path: &Path,
    data: &[u8],
    chunk_size: usize,
    inter_chunk_delay: Option<Duration>,
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
    // rename) and retry. A second `AlreadyExists` is a real conflict —
    // another process is writing concurrently — and we bail with
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
            file.write_all(&data[offset..end]).await.map_err(|e| {
                WriteAtomicError::PostOpen {
                    stage: "write",
                    source: e,
                }
            })?;
            offset = end;
            if let Some(d) = inter_chunk_delay {
                if offset < data.len() {
                    tokio::time::sleep(d).await;
                }
            }
        }
        file.flush()
            .await
            .map_err(|e| WriteAtomicError::PostOpen {
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

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rsync_native_proto::types::NativeRsyncErrorKind;
    use std::io::Write;
    use tempfile::TempDir;
    use tokio::time::timeout;

    fn fresh_tempdir() -> TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    // -- map_native_error_to_rsync -----------------------------------------

    #[test]
    fn map_cancel_maps_to_rsync_cancelled_regardless_of_committed() {
        for committed in [false, true] {
            let err = NativeRsyncError::cancelled("user abort");
            let rs = map_native_error_to_rsync(err, committed);
            assert!(
                matches!(rs, RsyncError::Cancelled),
                "committed={committed} → expected RsyncError::Cancelled, got {rs:?}"
            );
        }
    }

    #[test]
    fn map_pre_commit_environmental_errors_land_in_transfer_failed_minus_one() {
        let kinds = [
            NativeRsyncErrorKind::UnsupportedVersion,
            NativeRsyncErrorKind::NegotiationFailed,
            NativeRsyncErrorKind::TransportFailure,
            NativeRsyncErrorKind::RemoteError,
        ];
        for kind in kinds {
            let err = NativeRsyncError::new(kind, "env");
            let rs = map_native_error_to_rsync(err, false);
            match rs {
                RsyncError::TransferFailed { exit, stderr } => {
                    assert_eq!(exit, -1, "pre-commit {kind:?} must use sentinel -1");
                    assert!(stderr.contains("native fallback"));
                    assert!(stderr.contains(&format!("{kind:?}")));
                }
                other => panic!("pre-commit {kind:?} → expected TransferFailed, got {other:?}"),
            }
        }
    }

    #[test]
    fn map_pre_commit_host_key_rejected_is_hard_rejection() {
        // R4 pin: HostKeyRejected MUST produce HardRejection even pre-commit,
        // so `transfer_with_delta` routes it to `hard_error` and the user
        // sees the failure — no silent classic fallback.
        let err = NativeRsyncError::host_key_rejected("fingerprint mismatch");
        let rs = map_native_error_to_rsync(err, false);
        match rs {
            RsyncError::HardRejection(msg) => {
                assert!(msg.contains("HostKeyRejected"));
                assert!(msg.contains("fingerprint mismatch"));
            }
            other => panic!("expected HardRejection, got {other:?}"),
        }
    }

    #[test]
    fn map_post_commit_non_cancel_is_always_hard_rejection() {
        let kinds = [
            NativeRsyncErrorKind::UnsupportedVersion,
            NativeRsyncErrorKind::InvalidFrame,
            NativeRsyncErrorKind::TransportFailure,
            NativeRsyncErrorKind::NegotiationFailed,
            NativeRsyncErrorKind::PlannerRejected,
            NativeRsyncErrorKind::IllegalStateTransition,
            NativeRsyncErrorKind::RemoteError,
            NativeRsyncErrorKind::UnexpectedMessage,
            NativeRsyncErrorKind::HostKeyRejected,
            NativeRsyncErrorKind::Internal,
        ];
        for kind in kinds {
            let err = NativeRsyncError::new(kind, "post-commit");
            let rs = map_native_error_to_rsync(err, true);
            match rs {
                RsyncError::HardRejection(msg) => {
                    assert!(
                        msg.contains(&format!("{kind:?}")),
                        "post-commit {kind:?} message missing kind tag: {msg}"
                    );
                }
                other => panic!("post-commit {kind:?} → expected HardRejection, got {other:?}"),
            }
        }
    }

    #[test]
    fn map_pre_commit_protocol_bugs_are_hard_rejection() {
        let kinds = [
            NativeRsyncErrorKind::InvalidFrame,
            NativeRsyncErrorKind::IllegalStateTransition,
            NativeRsyncErrorKind::PlannerRejected,
            NativeRsyncErrorKind::UnexpectedMessage,
            NativeRsyncErrorKind::Internal,
        ];
        for kind in kinds {
            let err = NativeRsyncError::new(kind, "proto-bug");
            let rs = map_native_error_to_rsync(err, false);
            match rs {
                RsyncError::HardRejection(_) => {}
                other => panic!("pre-commit {kind:?} → expected HardRejection, got {other:?}"),
            }
        }
    }

    // -- build_source_entry -------------------------------------------------

    #[test]
    fn build_source_entry_extracts_basename_and_sets_size() {
        let path = Path::new("/tmp/some/deep/path/payload.bin");
        let entry = build_source_entry(path, 1_234_567);
        assert_eq!(entry.path, "payload.bin");
        assert_eq!(entry.size, 1_234_567);
        assert_eq!(entry.mtime, 0);
        assert!(entry.uid.is_none());
        assert!(entry.gid.is_none());
        assert!(entry.checksum.is_empty());
        // Baseline flags exercised by the A2.x frozen-oracle pins.
        assert_eq!(entry.flags & 0x0040, 0x0040, "XMIT_LONG_NAME");
        assert_eq!(entry.flags & 0x0002, 0x0002, "XMIT_SAME_MODE");
    }

    #[test]
    fn build_source_entry_fallback_name_when_no_file_name() {
        let entry = build_source_entry(Path::new("/"), 0);
        assert_eq!(entry.path, "source.bin");
    }

    // -- build_stats --------------------------------------------------------

    #[test]
    fn build_stats_handles_zero_bytes_sent_without_div_by_zero() {
        let ss = SessionStats::default();
        let stats = build_stats(&ss, 100, 50, vec!["w1".into()]);
        assert_eq!(stats.bytes_sent, 0);
        assert_eq!(stats.total_size, 100);
        assert_eq!(stats.speedup, 1.0);
        assert_eq!(stats.duration_ms, 50);
        assert_eq!(stats.warnings, vec!["w1".to_string()]);
    }

    #[test]
    fn build_stats_speedup_is_total_over_bytes_sent_when_nonzero() {
        let ss = SessionStats {
            bytes_sent: 25,
            bytes_received: 10,
            ..SessionStats::default()
        };
        let stats = build_stats(&ss, 100, 200, Vec::new());
        assert!((stats.speedup - 4.0).abs() < 1e-9);
        assert_eq!(stats.bytes_sent, 25);
        assert_eq!(stats.bytes_received, 10);
    }

    // -- write_atomic_chunked happy path -----------------------------------

    #[tokio::test]
    async fn write_atomic_commits_new_contents_on_success() {
        let dir = fresh_tempdir();
        let target = dir.path().join("result.bin");
        // Pre-populate with OLD so the test proves a real overwrite.
        std::fs::File::create(&target)
            .unwrap()
            .write_all(b"OLD")
            .unwrap();

        write_atomic_chunked(&target, b"NEW_CONTENTS", 4096, None)
            .await
            .expect("atomic write must succeed");

        let actual = fs::read(&target).await.unwrap();
        assert_eq!(actual, b"NEW_CONTENTS");
        let tmp = temp_path_for(&target);
        assert!(
            !tmp.exists(),
            "temp file must be removed by rename on success: {tmp:?}"
        );
    }

    #[tokio::test]
    async fn write_atomic_creates_missing_target_file() {
        let dir = fresh_tempdir();
        let target = dir.path().join("fresh.bin");
        assert!(!target.exists());
        write_atomic_chunked(&target, b"NEW", 4096, None)
            .await
            .unwrap();
        assert_eq!(fs::read(&target).await.unwrap(), b"NEW");
    }

    #[tokio::test]
    async fn write_atomic_rejects_zero_chunk_size_pre_open() {
        let dir = fresh_tempdir();
        let target = dir.path().join("x.bin");
        let err = write_atomic_chunked(&target, b"DATA", 0, None)
            .await
            .expect_err("zero chunk must be rejected");
        assert!(matches!(err, WriteAtomicError::PreOpen(_)));
        // Pre-open rejection must never leave a temp file on disk.
        assert!(!temp_path_for(&target).exists());
    }

    #[tokio::test]
    async fn write_atomic_recovers_from_stale_temp_once() {
        let dir = fresh_tempdir();
        let target = dir.path().join("stale.bin");
        // Simulate a stale temp from a prior crashed run.
        let tmp = temp_path_for(&target);
        std::fs::File::create(&tmp)
            .unwrap()
            .write_all(b"STALE_LEFTOVER")
            .unwrap();
        // Still expected to succeed after one remove+retry.
        write_atomic_chunked(&target, b"FRESH", 4096, None)
            .await
            .expect("stale temp must be cleaned and retry must succeed");
        assert_eq!(fs::read(&target).await.unwrap(), b"FRESH");
    }

    // -- write_atomic_chunked kill-9 invariant pin -------------------------

    #[tokio::test]
    async fn write_atomic_preserves_old_on_future_drop_mid_write_kill9_simulation() {
        // The gate of merge for Zona B: if this test ever goes red, the
        // atomicity contract A0 is broken and Zona B cannot land.
        //
        // Setup: a 1 MiB payload split into 128 B chunks with a 1 ms
        // inter-chunk sleep → ~8 seconds of nominal wall time. Wrap the
        // write future in `tokio::time::timeout` with a short budget
        // (5–50 ms). The timeout wins, the future is dropped mid-write,
        // and the invariant under test is that `local_path` still holds
        // the exact OLD contents — never a partial mix.
        let dir = fresh_tempdir();
        let target = dir.path().join("large.bin");
        let old = {
            let mut v = Vec::with_capacity(1024);
            for i in 0..1024u32 {
                v.extend_from_slice(&i.to_le_bytes());
            }
            v
        };
        std::fs::File::create(&target)
            .unwrap()
            .write_all(&old)
            .unwrap();

        let new_data = vec![0xFFu8; 1024 * 1024];

        // Run the invariant pin five times at different interruption
        // points so we cover the full window (early / mid / late).
        for interrupt_ms in [5u64, 12, 20, 35, 50] {
            // Restore the OLD file before each iteration in case a prior
            // iteration's temp was left orphan in the directory (which
            // is OK per contract) — we only check `target` integrity.
            std::fs::File::create(&target)
                .unwrap()
                .write_all(&old)
                .unwrap();

            let res = timeout(
                Duration::from_millis(interrupt_ms),
                write_atomic_chunked(
                    &target,
                    &new_data,
                    128,
                    Some(Duration::from_millis(1)),
                ),
            )
            .await;

            // The timeout must have fired — the nominal duration is ~8 s,
            // far above every budget we chose. If it completed, the
            // test's chunking tuning is off (flakiness guardrail).
            assert!(
                res.is_err(),
                "iteration {interrupt_ms}ms: write completed before timeout — chunking tuning off"
            );

            let after = fs::read(&target).await.unwrap();
            assert_eq!(
                after, old,
                "iteration {interrupt_ms}ms: target MUST hold OLD contents intact after mid-write drop"
            );
        }
    }
}
