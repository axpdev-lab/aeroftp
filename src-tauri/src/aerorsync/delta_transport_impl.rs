//! A4: `AerorsyncDeltaTransport`: production-facing `DeltaTransport`
//! implementation backed by the Strada C native rsync driver.
//!
//! The module is the bridge between the prototype driver
//! (`AerorsyncDriver` + `RsyncEventBridge`) and the production
//! `crate::delta_transport::DeltaTransport` trait consumed by the sync
//! loop. It owns:
//!
//! - Construction of the SSH transport, driver, adapter, and bridge for
//!   each individual transfer (no cross-transfer session caching: the
//!   trait is `&self`, so we avoid locking altogether).
//! - Translation of typed `AerorsyncError` into `RsyncError` through
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
//! - ~~R2: upload reads the source file into RAM~~. Resolved in
//!   P3-T01 W1.3: `upload_inner` opens the source as `tokio::fs::File`
//!   and streams it through `drive_upload_through_delta_streaming`. The
//!   upload-side `AERORSYNC_MAX_IN_MEMORY_BYTES` guard was removed.
//! - ~~R3: download decodes into a `Vec<u8>` that A4 buffers before the
//!   temp-file write~~. Resolved in P3-T01 W2.5: `download_inner` opens a
//!   `FileBaseline` for `CopyBlock` dispatch and streams reconstructed
//!   bytes through a `StreamingAtomicWriter` (`<target>.aerotmp` →
//!   atomic rename on `finalize`). The download-side
//!   `AERORSYNC_MAX_IN_MEMORY_BYTES` guard was removed.
//!
//!   The signature phase still bulk-reads `local_path` via `tokio::fs::read`
//!   because `DeltaEngineAdapter::build_signatures` is bulk-only; a
//!   `build_signatures_streaming` adapter API is the post-P3-T01
//!   follow-up that brings the resident set to `O(window)` regardless of
//!   baseline size. Until then, baselines are still read once into RAM
//!   for signatures, but the reconstructed buffer is gone: RSS scales
//!   with `O(baseline + writer_buffer)` instead of
//!   `O(baseline + reconstructed)`.

#![cfg(feature = "aerorsync")]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::fs::{self, OpenOptions};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

use crate::aerorsync::engine_adapter::{
    BaselineSource, CurrentDeltaSyncBridge, FileBaseline, MemoryBaseline,
};
use crate::aerorsync::fallback_policy::{classify_fallback, FallbackVerdict};
use crate::aerorsync::native_driver::{AerorsyncDriver, PreambleProfile};
use crate::aerorsync::real_wire::FileListEntry;
use crate::aerorsync::remote_command::RemoteCommandSpec;
use crate::aerorsync::rsync_event_bridge::RsyncEventBridge;
use crate::aerorsync::russh_session_transport::RusshSessionTransport;
use crate::aerorsync::ssh_transport::{
    SshHostKeyPolicy, SshRemoteShellTransport, SshTransportConfig,
};
use crate::aerorsync::streaming_writer::StreamingAtomicWriter;
use crate::aerorsync::transport::{
    CancelHandle, RawRemoteShellTransport, RemoteExecRequest, RemoteShellTransport,
};
use crate::aerorsync::types::{AerorsyncError, AerorsyncErrorKind, SessionStats};
use crate::delta_transport::{BatchStats, DeltaBatch, DeltaTransport};
use crate::rsync_output::RsyncEvent;
use crate::rsync_over_ssh::{RsyncCapability, RsyncConfig, RsyncError, RsyncStats};

/// Display name surfaced by `DeltaTransport::name()`.
const AERORSYNC_TRANSPORT_NAME: &str = "aerorsync-proto-31";

/// Chunk size used by `write_atomic_chunked` in production. 64 KiB
/// matches the AeroVault v2 body chunk + keeps syscall count reasonable.
const ATOMIC_WRITE_CHUNK_SIZE: usize = 64 * 1024;

/// Suffix appended to the destination path while the write is in
/// progress. The rename onto the final path is the atomic commit.
const TEMP_SUFFIX: &str = ".aerotmp";

/// Counter used to salt the per-instance temp suffix so two concurrent
/// AeroFTP processes (or two threads in the same app) downloading to the
/// same path do not contend on the same `.aerotmp` filename.
static TEMP_SUFFIX_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// `DeltaTransport` impl driven by the prototype native rsync driver.
///
/// One instance is cheap to construct and safe to share across many
/// transfers; each `upload` / `download` call builds its own SSH session
/// and driver so the trait methods can remain `&self`.
pub struct AerorsyncDeltaTransport {
    ssh_config: SshTransportConfig,
    min_file_size: u64,
}

impl AerorsyncDeltaTransport {
    /// Primary constructor: takes a fully-populated SSH config and the
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
        // Z.4.5 R1 dispatch step (2026-05-14): the previous boundary
        // refusal `Err(PasswordAuthUnsupported)` was a placeholder while
        // the russh transport gained password auth. Now that
        // `RusshSessionTransport::connect` branches on
        // `SshTransportConfig::usable_password()`, the gate moves to
        // `RsyncConfig::validate_auth_material()` which enforces:
        //   - SshKey  → ssh_key_path required (else MissingKey)
        //   - Password → ssh_password required and non-empty (else MissingPassword)
        //   - Neither → HardRejection (integration bug, never silently retry)
        // Callers that want password-based delta sync can now construct
        // an `RsyncConfig { auth_method: Password, ssh_password: Some(_), .. }`
        // and the russh leg picks it up. Subprocess `rsync_over_ssh::build_ssh_e_arg`
        // still refuses Password upfront so the binary path never accidentally
        // shells out without auth material.
        cfg.validate_auth_material()?;

        // Password-only profiles legitimately have no key path. The
        // russh leg ignores `private_key_path` when `usable_password()`
        // is Some, so an empty placeholder is safe; it is never opened
        // or dereferenced. We MUST NOT default to `~/.ssh/id_rsa` or
        // any other concrete path: that would silently load credentials
        // the user did not opt into.
        let key_path = cfg.ssh_key_path.clone().unwrap_or_default();
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
            auth_password: cfg.ssh_password.clone(),
            // An Agent profile carries no key/password; the russh leg
            // resolves SSH_AUTH_SOCK at connect time. `prefers_russh_leg`
            // then routes probe + single-shot through russh (libssh2 is
            // pubkey-file-only).
            auth_agent: matches!(cfg.auth_method, crate::rsync_over_ssh::AuthMethod::Agent),
            // B.1/B.4: probe stock `rsync --version` on the remote. The
            // parser in `parse_probe_protocol` extracts the numeric
            // protocol version from the multi-line banner. A missing
            // `rsync` binary surfaces as exit != 0 and is mapped to
            // `RsyncError::RemoteNotAvailable` (soft classic fallback);
            // only `HostKeyRejected` escalates to `HardRejection`.
            probe_request: RemoteExecRequest {
                program: "rsync".into(),
                args: vec!["--version".into()],
                environment: Vec::new(),
            },
        };
        Ok(Self::new(ssh_config, cfg.min_file_size))
    }
}

#[async_trait]
impl DeltaTransport for AerorsyncDeltaTransport {
    fn name(&self) -> &'static str {
        AERORSYNC_TRANSPORT_NAME
    }

    async fn probe_remote(&self) -> Result<RsyncCapability, RsyncError> {
        // U-04: real exec probe. Opens a one-shot SSH exec channel and
        // runs `aerorsync_serve --probe`. A non-zero exit or a
        // transport failure propagates as `RsyncError::RemoteNotAvailable`
        // so the adapter's probe cache (`PROBE_CACHE`, 5-minute TTL)
        // memoises a typed "unavailable" verdict: without this, every
        // file in a multi-file sync would enter the native path, pay a
        // fresh SSH setup, fail at `open_raw_stream`, and only then
        // fall back to classic.
        let probe_result = if self.ssh_config.prefers_russh_leg() {
            let transport = RusshSessionTransport::connect(self.ssh_config.clone())
                .await
                .map_err(map_native_probe_error_to_rsync)?;
            transport.probe().await
        } else {
            let transport = SshRemoteShellTransport::new(self.ssh_config.clone());
            transport.probe().await
        };
        let probe = match probe_result {
            Ok(p) => p,
            Err(error) => {
                let rsync_error = map_native_probe_error_to_rsync(error);
                if matches!(rsync_error, RsyncError::HardRejection(_)) {
                    return Err(rsync_error);
                }
                tracing::warn!(
                    "native rsync probe failed for {}:{}: {}: marking remote unavailable",
                    self.ssh_config.host,
                    self.ssh_config.port,
                    rsync_error
                );
                return Err(rsync_error);
            }
        };
        Ok(RsyncCapability {
            version: probe.remote_banner,
            protocol: probe.protocol.0,
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

    async fn upload(&self, local_path: &Path, remote_path: &str) -> Result<RsyncStats, RsyncError> {
        self.upload_inner(local_path, remote_path).await
    }

    /// P3-T01 W3.2(b2): open a session-reuse batch backed by russh.
    ///
    /// Performs one SSH handshake here ([`RusshSessionTransport::connect`])
    /// and returns an [`AerorsyncBatch`] that opens a fresh channel-exec
    /// per file over that single SSH session: the per-file cost drops
    /// from full handshake to channel allocation. Failure to connect
    /// degrades to [`crate::delta_transport::NoopBatch`] via the trait
    /// default, so the sync loop falls back to the single-shot path
    /// without losing the file.
    async fn begin_batch(&self) -> Result<Box<dyn DeltaBatch>, RsyncError> {
        match RusshSessionTransport::connect(self.ssh_config.clone()).await {
            Ok(transport) => Ok(Box::new(AerorsyncBatch::new(transport, self.min_file_size))),
            Err(e) => {
                tracing::warn!(
                    "AerorsyncDeltaTransport::begin_batch: russh connect failed ({}); \
                     falling back to NoopBatch: sync loop will use single-shot per-file path",
                    e
                );
                Ok(Box::new(crate::delta_transport::NoopBatch::new()))
            }
        }
    }
}

// --- upload flow ---------------------------------------------------------

impl AerorsyncDeltaTransport {
    /// Single-shot upload. Constructs a fresh `SshRemoteShellTransport`
    /// and an inert `CancelHandle` (the `&self` trait method has no
    /// cancel hook available) and delegates to [`do_upload`]. The same
    /// helper is reused by `AerorsyncBatch::upload` (W3.2b) with a
    /// long-lived transport that keeps an SSH session alive across N
    /// files and with a real `CancelHandle` shared across the batch.
    async fn upload_inner(
        &self,
        local_path: &Path,
        remote_path: &str,
    ) -> Result<RsyncStats, RsyncError> {
        let cancel = CancelHandle::inert();
        let preamble_profile = PreambleProfile::for_host(&self.ssh_config.host);
        if self.ssh_config.prefers_russh_leg() {
            let transport = RusshSessionTransport::connect(self.ssh_config.clone())
                .await
                .map_err(|e| map_native_error_to_rsync(e, false))?;
            do_upload(
                transport,
                cancel,
                local_path,
                remote_path,
                self.min_file_size,
                preamble_profile,
            )
            .await
        } else {
            let transport = SshRemoteShellTransport::new(self.ssh_config.clone());
            do_upload(
                transport,
                cancel,
                local_path,
                remote_path,
                self.min_file_size,
                preamble_profile,
            )
            .await
        }
    }
}

/// Module-private upload core extracted from `upload_inner` in P3-T01 W3.2(a).
///
/// The function takes the remote-shell transport and cancel handle as
/// parameters so the same logic serves both the trait single-shot path
/// (`AerorsyncDeltaTransport::upload`, fresh transport per call) and the
/// session-reuse batch (`AerorsyncBatch::upload` in W3.2b, transport
/// preallocated once per batch). All other behavior: metadata probe,
/// `min_file_size` gate, streaming xxh128, source entry build, driver
/// drive + finish_session, stats build: is byte-identical to the
/// pre-W3.2 single-shot path.
///
/// Pinned by `cargo test --features aerorsync --lib aerorsync::` 453/453;
/// any semantic change must surface as a test diff.
#[allow(clippy::too_many_arguments)]
async fn do_upload<T>(
    transport: T,
    cancel: CancelHandle,
    local_path: &Path,
    remote_path: &str,
    min_file_size: u64,
    preamble_profile: PreambleProfile,
) -> Result<RsyncStats, RsyncError>
where
    T: RawRemoteShellTransport + 'static,
{
    let start = Instant::now();
    let metadata = fs::metadata(local_path).await.map_err(RsyncError::Io)?;
    let file_size = metadata.len();
    if file_size < min_file_size {
        return Err(RsyncError::TooSmall {
            size: file_size,
            threshold: min_file_size,
        });
    }
    // P3-T01 W1.3: upload-side cap removed. Sources of any size now
    // flow through `drive_upload_through_delta_streaming` (W1.2).
    // The driver reads `STREAMING_READ_CHUNK_BYTES`-bounded slabs
    // from the file handle and emits engine literals incrementally,
    // so the upload no longer requests a `Vec<u8>` of `file_size`
    // bytes. The resident memory bound becomes `O(read_chunk +
    // op_vector)`; lifting the op_vector dependency on file_size
    // requires streaming the zstd encoder + wire emission, scoped
    // post-P3-T01 (see `send_delta_phase_streaming` docstring).
    //
    // U-07: preserve the source mtime on the wire. Classic rsync
    // preserves mtime by default and `RsyncConfig::preserve_times`
    // is already on for the SFTP path; hardcoding `mtime: 0` was a
    // silent regression for mtime-aware sync consumers.
    //
    // The xxh128 file checksum advertised in the file-list entry
    // (`-c always-checksum` parity) is computed via a streaming
    // pass over the file before the file_list phase. The OS page
    // cache makes the second read (inside the delta phase) a hot
    // hit on typical workloads, so the practical I/O cost is one
    // disk read.
    let file_checksum = compute_xxh128_file_streaming(local_path)
        .await
        .map_err(RsyncError::Io)?;
    let source_entry = build_source_entry(local_path, file_size, &metadata, file_checksum);

    let source_file = fs::File::open(local_path).await.map_err(RsyncError::Io)?;

    let mut driver =
        AerorsyncDriver::new(transport, cancel).with_preamble_profile(preamble_profile);
    let adapter = CurrentDeltaSyncBridge::new();
    let warnings = new_warnings_sink();
    let mut bridge = build_event_bridge(warnings.clone());

    // B.1: production dispatch now talks to stock `rsync --server`
    // (WrapperParity flavor) instead of the dev helper
    // `aerorsync_serve`. The wrapper command line is byte-pinned
    // against rsync 3.2.7 capture by `upload_remote_command_matches_capture`.
    let spec = RemoteCommandSpec::upload(remote_path);
    let drive_res = driver
        .drive_upload_through_delta_streaming(
            spec,
            source_entry,
            source_file,
            file_size,
            &adapter,
            &mut bridge,
        )
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

// --- download flow -------------------------------------------------------

impl AerorsyncDeltaTransport {
    /// Single-shot download. Constructs a fresh `SshRemoteShellTransport`
    /// and an inert `CancelHandle` and delegates to [`do_download`]. The
    /// same helper is reused by `AerorsyncBatch::download` (W3.2b) with
    /// a long-lived transport that keeps an SSH session alive across N
    /// files and with a real shared cancel handle.
    async fn download_inner(
        &self,
        remote_path: &str,
        local_path: &Path,
    ) -> Result<RsyncStats, RsyncError> {
        let cancel = CancelHandle::inert();
        let preamble_profile = PreambleProfile::for_host(&self.ssh_config.host);
        if self.ssh_config.prefers_russh_leg() {
            let transport = RusshSessionTransport::connect(self.ssh_config.clone())
                .await
                .map_err(|e| map_native_error_to_rsync(e, false))?;
            do_download(transport, cancel, remote_path, local_path, preamble_profile).await
        } else {
            let transport = SshRemoteShellTransport::new(self.ssh_config.clone());
            do_download(transport, cancel, remote_path, local_path, preamble_profile).await
        }
    }
}

/// Discard the streaming writer's in-flight `<target>.aerotmp` before
/// bailing out of [`do_download`] down a fallback-eligible path.
///
/// The delta [`StreamingAtomicWriter`] and the classic SFTP `AtomicFile`
/// fallback both target the SAME deterministic `<target>.aerotmp`, and the
/// classic path opens it with `create_new(true)`. The writer's `Drop`
/// intentionally keeps the temp, so any abandon path that wants the classic
/// fallback to succeed must remove it explicitly: otherwise a transient
/// delta failure degrades into a hard `AlreadyExists` on the retry. The
/// original target file is never touched here (no rename happened on these
/// paths; if a rename had committed, `remove_file` is a harmless no-op).
async fn discard_streaming_temp(writer: StreamingAtomicWriter) {
    let temp = writer.temp_path().to_path_buf();
    drop(writer);
    let _ = fs::remove_file(&temp).await;
}

/// Module-private download core extracted from `download_inner` in P3-T01
/// W3.2(a). Behavior byte-identical to the pre-W3.2 single-shot path:
/// baseline read + FileBaseline/MemoryBaseline pick + StreamingAtomicWriter
/// open + driver drive + finish_session + writer.finalize + stats build.
///
/// Like [`do_upload`], the function accepts the remote-shell transport and
/// cancel handle as parameters so the session-reuse batch (W3.2b) can pass
/// a long-lived transport instead of allocating a fresh SSH session per
/// file. Pinned by `cargo test --features aerorsync --lib aerorsync::`
/// 453/453.
async fn do_download<T>(
    transport: T,
    cancel: CancelHandle,
    remote_path: &str,
    local_path: &Path,
    preamble_profile: PreambleProfile,
) -> Result<RsyncStats, RsyncError>
where
    T: RawRemoteShellTransport + 'static,
{
    let start = Instant::now();
    // P3-T01 W2.5: the bulk read still feeds the signature phase
    // (`adapter.build_signatures` is bulk-only until the post-P3-T01
    // streaming variant lands). Reconstruction, however, no longer
    // materialises a `Vec<u8>`: it streams into a
    // `StreamingAtomicWriter` opened below.
    //
    // U-03: distinguish `NotFound` (legitimate empty baseline) from
    // every other `io::Error`. Before the fix, `unwrap_or_default()`
    // silently masked `PermissionDenied`, `EIO`, symlink loops, etc.
    // into "empty baseline", degrading the delta path to a full
    // download while hiding the underlying error from the user.
    let (destination_data, baseline_mode) = match fs::read(local_path).await {
        Ok(data) => {
            // U-09: capture the pre-existing mode so we can restore it on
            // the temp file before the atomic rename, preserving
            // perms / setuid / readonly across the in-place update.
            let mode = existing_mode_if_any(local_path).await;
            (data, mode)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // Legitimate empty baseline: target file does not exist
            // yet. Classic full-download semantics via the native
            // delta pipeline.
            (Vec::new(), None)
        }
        Err(error) => {
            // Any other read failure must surface, not silently
            // degrade to full-size delta. Pre-commit classification
            // routes this through classic fallback with a visible
            // reason in the stderr string.
            return Err(RsyncError::TransferFailed {
                exit: -1,
                stderr: format!(
                    "native fallback: cannot read local baseline {}: {}",
                    local_path.display(),
                    error
                ),
            });
        }
    };

    // Random-access baseline for `apply_delta_streaming`'s
    // `CopyBlock(idx)` dispatch. When the target does not exist yet
    // we substitute an empty `MemoryBaseline`: the engine never
    // emits CopyBlocks against an empty signature set, so the
    // baseline is unused but the trait object is still required by
    // the streaming entry-point signature.
    let mut baseline: Box<dyn BaselineSource + Send> = if destination_data.is_empty() {
        Box::new(MemoryBaseline::new(Vec::new()))
    } else {
        match FileBaseline::open(local_path).await {
            Ok(fb) => Box::new(fb),
            Err(error) => {
                return Err(RsyncError::TransferFailed {
                    exit: -1,
                    stderr: format!(
                        "native fallback: cannot open streaming baseline {}: {}",
                        local_path.display(),
                        error
                    ),
                });
            }
        }
    };

    // Open the `<target>.aerotmp` sink before the SSH session so a
    // failure here surfaces as a pre-commit error (no wire bytes
    // exchanged, no `local_committed=true` invariant tripped).
    let mut writer =
        StreamingAtomicWriter::new(local_path)
            .await
            .map_err(|e| RsyncError::TransferFailed {
                exit: -1,
                stderr: format!(
                    "native fallback: cannot open streaming temp file for {}: {}",
                    local_path.display(),
                    e
                ),
            })?;

    let mut driver =
        AerorsyncDriver::new(transport, cancel).with_preamble_profile(preamble_profile);
    let adapter = CurrentDeltaSyncBridge::new();
    let warnings = new_warnings_sink();
    let mut bridge = build_event_bridge(warnings.clone());

    // B.1: production dispatch now talks to stock `rsync --server --sender`
    // (WrapperParity flavor). Pinned against rsync 3.2.7 capture by
    // `download_remote_command_matches_capture`.
    let spec = RemoteCommandSpec::download(remote_path);
    let drive_res = driver
        .drive_download_through_delta_streaming(
            spec,
            &destination_data,
            &mut *baseline,
            &mut writer,
            &adapter,
            &mut bridge,
        )
        .await;
    if let Err(e) = drive_res {
        // Abandon path. A delta failure here (transient transport error,
        // early channel close, etc.) maps to `DeltaSyncResult::fallback`
        // and the caller re-runs the classic SFTP download. The
        // `StreamingAtomicWriter` Drop deliberately keeps the orphan
        // `<target>.aerotmp`, but the classic `AtomicFile` fallback opens
        // that SAME deterministic path with `create_new(true)` and would
        // fail with `AlreadyExists`. Discard the temp so the fallback can
        // proceed; the original `local_path` is untouched (no rename).
        let mapped = map_native_error_to_rsync(e, driver.committed());
        discard_streaming_temp(writer).await;
        return Err(mapped);
    }
    if let Err(e) = driver.finish_session(&mut bridge).await {
        // Same abandon-path reasoning as the `drive_res` arm above: clear
        // the orphan temp before the classic fallback re-opens it.
        let mapped = map_native_error_to_rsync(e, driver.committed());
        discard_streaming_temp(writer).await;
        return Err(mapped);
    }

    let file_size = writer.bytes_written();

    let remote_entry = driver.downloaded_entry().cloned();
    let preserve_mode = remote_entry
        .as_ref()
        .map(|entry| entry.mode)
        .or(baseline_mode);
    // `StreamingAtomicWriter::finalize` takes `(i64, u32)` for
    // (mtime_secs, mtime_nsecs); rsync wire entries carry the
    // sub-second part as `Option<i32>` (None = NSEC absent / 0).
    // Cast through `u32` matching the bulk path (`write_atomic_chunked`
    // does the same internally via `mtime_nsec.unwrap_or(0)`).
    let preserve_mtime = remote_entry
        .as_ref()
        .map(|entry| (entry.mtime, entry.mtime_nsec.unwrap_or(0).max(0) as u32));
    if remote_entry.is_none() {
        tracing::warn!(
            "native rsync download completed without remote file metadata; preserving local baseline mode only"
        );
    }

    // Completeness guard. The remote file list carries the authoritative
    // size. Some embedded rsync servers (e.g. WD MyCloud's custom firmware,
    // proto 31) close the SSH channel BEFORE the trailing NDX_DONE marker;
    // `read_trailing_ndx_done` then accepts that early close as a clean EOF
    // and we would otherwise commit a silently truncated reconstruction as a
    // successful delta download. Refuse to commit a short file: discard the
    // temp and return a transfer error so `transfer_with_delta` folds it into
    // `DeltaSyncResult::fallback` and the caller re-runs the classic SFTP
    // download (proven correct on these servers). The temp must be removed
    // here because the classic path opens the same `.aerotmp` with
    // `create_new(true)` and would otherwise fail with EEXIST; the original
    // target file is never touched (no rename happened).
    if let Some(ref entry) = remote_entry {
        // `file_size` is u64 (bytes written); rsync `entry.size` is i64.
        if entry.size < 0 || file_size != entry.size as u64 {
            let stderr = format!(
                "delta reconstruction incomplete: {} of {} bytes for {} \
                 (remote closed before completion); falling back to classic download",
                file_size, entry.size, remote_path
            );
            discard_streaming_temp(writer).await;
            return Err(RsyncError::TransferFailed { exit: -1, stderr });
        }
    }

    // Atomic commit: flush + sync_all + chmod (Unix) + set_mtime + rename.
    // Failures here are post-commit-cutover and surface as
    // `HardRejection` via `map_write_atomic_error`.
    writer
        .finalize(preserve_mode, preserve_mtime)
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

// --- batch (W3.2(b2)) -----------------------------------------------------

/// P3-T01 W3.2(b2): concrete [`DeltaBatch`] impl backed by russh.
///
/// Holds a single [`RusshSessionTransport`] for the lifetime of the
/// batch. `share_session()` per file gives the driver a transport view
/// that points at the same SSH session, so N files cost 1 handshake +
/// N channel-exec opens (vs. N full handshakes on the single-shot
/// path). [`do_upload`] / [`do_download`] are reused unchanged: the
/// batch is the same wire semantics, fewer handshakes.
pub struct AerorsyncBatch {
    transport: RusshSessionTransport,
    /// Cooperative cancel handle exposed via [`DeltaBatch::cancel`].
    /// Cloned per-file into the driver so a cancel mid-file unwinds
    /// via the existing `CancelHandle` paths.
    cancel: CancelHandle,
    min_file_size: u64,
    files_transferred: AtomicU64,
    bytes_on_wire: AtomicU64,
    /// Mirrors the `cancel` flag so [`finalize`] can populate
    /// `BatchStats.partial`.
    ///
    /// [`finalize`]: DeltaBatch::finalize
    cancel_observed: Arc<AtomicBool>,
}

impl AerorsyncBatch {
    fn new(transport: RusshSessionTransport, min_file_size: u64) -> Self {
        let flag = Arc::new(AtomicBool::new(false));
        Self {
            transport,
            cancel: CancelHandle::new(flag.clone(), None),
            min_file_size,
            files_transferred: AtomicU64::new(0),
            bytes_on_wire: AtomicU64::new(0),
            cancel_observed: flag,
        }
    }

    /// Reconnect the shared SSH session, retrying with exponential
    /// backoff so the call survives a brief outage (typically the
    /// 1-10s window an SSH daemon needs to come back after a network
    /// blip, a firewall rule pulse, or a sshd restart).
    ///
    /// The schedule is `0ms, 200ms, 500ms, 1s, 2s, 4s` (cumulative
    /// ~7.7s, 6 attempts). The first attempt fires immediately, which
    /// is the right call when the outage is already over by the time
    /// the classifier sees the channel drop. Each subsequent attempt
    /// waits a bit longer, so the worst case is bounded but the
    /// happy-path latency is unchanged.
    ///
    /// Z.1.2 lane 2026-05-14: the bare single-attempt `reconnect()`
    /// landed inside a 4s iptables REJECT pulse and tripped 63/63
    /// retries with `Connection refused`. With the staircase schedule
    /// every retry attempt completes within the outage window of a
    /// typical pulse, so the batch can finish its file list with a
    /// single SSH session re-handshake.
    ///
    /// Cancellation: each sleep is awaited as a cooperative point,
    /// and after each failed attempt we re-check the cancel flag so a
    /// `Ctrl+C` during a long backoff returns promptly with the most
    /// recent transport error.
    async fn reconnect_with_backoff(&self) -> Result<(), AerorsyncError> {
        const DELAYS_MS: &[u64] = &[0, 200, 500, 1000, 2000, 4000];
        let mut last_err: Option<AerorsyncError> = None;
        for (i, delay_ms) in DELAYS_MS.iter().enumerate() {
            if self.cancel_observed.load(Ordering::SeqCst) {
                return Err(last_err.unwrap_or_else(|| {
                    AerorsyncError::cancelled(
                        "reconnect_with_backoff: cancelled before any attempt",
                    )
                }));
            }
            if *delay_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(*delay_ms)).await;
            }
            match self.transport.reconnect().await {
                Ok(()) => {
                    if i > 0 {
                        tracing::info!(
                            "AerorsyncBatch reconnect succeeded on attempt {}/{}",
                            i + 1,
                            DELAYS_MS.len()
                        );
                    }
                    return Ok(());
                }
                Err(e) => {
                    tracing::debug!(
                        "AerorsyncBatch reconnect attempt {}/{} failed: {}",
                        i + 1,
                        DELAYS_MS.len(),
                        e
                    );
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| {
            AerorsyncError::transport("reconnect_with_backoff: no attempts made (empty schedule)")
        }))
    }
}

#[async_trait]
impl DeltaBatch for AerorsyncBatch {
    async fn upload(
        &mut self,
        local_path: &Path,
        remote_path: &str,
    ) -> Result<RsyncStats, RsyncError> {
        // Z.1.2 — single retry budget on transient SSH channel drops.
        // The first attempt uses the existing shared session; if it
        // fails with a transient I/O error or a `TransferFailed` whose
        // stderr matches the channel-drop allowlist, we re-authenticate
        // via `RusshSessionTransport::reconnect()` and retry the file
        // exactly once. Hard rejections (host-key mismatch, auth
        // refused, cancel) always propagate without retry.
        tracing::debug!(
            "AerorsyncBatch::upload entered for {} (sessions_so_far={})",
            remote_path,
            self.transport.handshake_count()
        );
        let preamble_profile = PreambleProfile::for_host(self.transport.endpoint_host());
        let first = do_upload(
            self.transport.share_session(),
            self.cancel.clone(),
            local_path,
            remote_path,
            self.min_file_size,
            preamble_profile.clone(),
        )
        .await;
        if let Err(ref e) = first {
            tracing::debug!(
                "AerorsyncBatch::upload: first attempt errored on {} → variant={} transient={}",
                remote_path,
                rsync_error_variant(e),
                crate::rsync_over_ssh::is_transient_for_reconnect(e)
            );
        }
        let stats = match first {
            Ok(stats) => stats,
            Err(err) if crate::rsync_over_ssh::is_transient_for_reconnect(&err) => {
                tracing::warn!(
                    "AerorsyncBatch::upload: transient drop on {} ({}); attempting reconnect",
                    remote_path,
                    err
                );
                if let Err(reconnect_err) = self.reconnect_with_backoff().await {
                    tracing::error!(
                        "AerorsyncBatch::upload: reconnect failed for {} after backoff: {}",
                        remote_path,
                        reconnect_err
                    );
                    // Propagate the ORIGINAL transfer error rather than
                    // the reconnect error so journals and callers
                    // diagnose the right symptom.
                    return Err(err);
                }
                do_upload(
                    self.transport.share_session(),
                    self.cancel.clone(),
                    local_path,
                    remote_path,
                    self.min_file_size,
                    preamble_profile,
                )
                .await?
            }
            Err(err) => return Err(err),
        };
        self.files_transferred.fetch_add(1, Ordering::SeqCst);
        let on_wire = stats.bytes_sent.saturating_add(stats.bytes_received);
        self.bytes_on_wire.fetch_add(on_wire, Ordering::SeqCst);
        Ok(stats)
    }

    async fn download(
        &mut self,
        remote_path: &str,
        local_path: &Path,
    ) -> Result<RsyncStats, RsyncError> {
        // Z.1.2 — symmetric retry budget on the download leg. Reconnect
        // semantics match the upload branch above; see the matching
        // comment for the classification rules.
        tracing::debug!(
            "AerorsyncBatch::download entered for {} (sessions_so_far={})",
            remote_path,
            self.transport.handshake_count()
        );
        let preamble_profile = PreambleProfile::for_host(self.transport.endpoint_host());
        let first = do_download(
            self.transport.share_session(),
            self.cancel.clone(),
            remote_path,
            local_path,
            preamble_profile.clone(),
        )
        .await;
        if let Err(ref e) = first {
            tracing::debug!(
                "AerorsyncBatch::download: first attempt errored on {} → variant={} transient={}",
                remote_path,
                rsync_error_variant(e),
                crate::rsync_over_ssh::is_transient_for_reconnect(e)
            );
        }
        let stats = match first {
            Ok(stats) => stats,
            Err(err) if crate::rsync_over_ssh::is_transient_for_reconnect(&err) => {
                tracing::warn!(
                    "AerorsyncBatch::download: transient drop on {} ({}); attempting reconnect",
                    remote_path,
                    err
                );
                if let Err(reconnect_err) = self.reconnect_with_backoff().await {
                    tracing::error!(
                        "AerorsyncBatch::download: reconnect failed for {} after backoff: {}",
                        remote_path,
                        reconnect_err
                    );
                    return Err(err);
                }
                do_download(
                    self.transport.share_session(),
                    self.cancel.clone(),
                    remote_path,
                    local_path,
                    preamble_profile,
                )
                .await?
            }
            Err(err) => return Err(err),
        };
        self.files_transferred.fetch_add(1, Ordering::SeqCst);
        let on_wire = stats.bytes_sent.saturating_add(stats.bytes_received);
        self.bytes_on_wire.fetch_add(on_wire, Ordering::SeqCst);
        Ok(stats)
    }

    fn cancel(&self) {
        self.cancel_observed.store(true, Ordering::SeqCst);
        self.cancel.cancel();
        // Best-effort async teardown of the russh handle. The DeltaBatch
        // contract is sync; we spawn so the cancel returns immediately.
        // The cancel_flag inside the shared transport ensures any
        // in-flight read/write surfaces a typed Cancelled error.
        let transport = self.transport.share_session();
        tokio::spawn(async move {
            transport.cancel().await;
        });
    }

    async fn finalize(self: Box<Self>) -> Result<BatchStats, RsyncError> {
        let session_count = self.transport.handshake_count();
        let _ = self.transport.close().await;
        Ok(BatchStats {
            files_transferred: self.files_transferred.load(Ordering::SeqCst),
            bytes_on_wire: self.bytes_on_wire.load(Ordering::SeqCst),
            session_count,
            partial: self.cancel_observed.load(Ordering::SeqCst),
        })
    }
}

// --- helpers -------------------------------------------------------------

/// Build the single-file `FileListEntry` for the upload path. The flag
/// shape mirrors the frozen oracle's first MSG_DATA (oracle bytes
/// [59..126], decoded in
/// `docs/dev/roadmap/APPENDIX-C-Y-D/APPENDIX-Y/2026-04-25_File_List_Wire_Annotation.md`):
/// the first entry of a list never SAMEs with anything (no previous
/// entry to compare against), and the production CLI invokes
/// `rsync --server -vlogDtprcze...` (preserve owner/group/times,
/// `-c` always-checksum). Therefore `XMIT_USER_NAME_FOLLOWS |
/// XMIT_GROUP_NAME_FOLLOWS | XMIT_MOD_NSEC` is the cumulative shape; the
/// uid/gid varints + name pairs follow inline because `inc_recurse=1`
/// is negotiated via CF_INC_RECURSE in the server compat byte.
fn build_source_entry(
    local_path: &Path,
    size: u64,
    metadata: &std::fs::Metadata,
    file_checksum: Vec<u8>,
) -> FileListEntry {
    // 0x2c00 = USER_NAME_FOLLOWS (1<<10) | GROUP_NAME_FOLLOWS (1<<11) | MOD_NSEC (1<<13).
    const BASELINE_FLAGS: u32 = (1 << 10) | (1 << 11) | (1 << 13);
    let name = local_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("source.bin")
        .to_string();
    let (mtime_secs, mtime_nsec_opt) = file_mtime_components(metadata);
    let (uid_value, gid_value) = file_owner_components(metadata);
    let uid_name = lookup_user_name(uid_value);
    let gid_name = lookup_group_name(gid_value);
    // P3-T01 W1.3: caller computes xxh128 via streaming pass over the
    // file (`compute_xxh128_file_streaming`) so we no longer require a
    // fully-buffered `source_data: &[u8]` argument here. xxh128 over
    // the file bytes mirrors `rsync -c` always-checksum. Server reads
    // 16 bytes (= csum_len_for_type(CSUM_XXH3_128)) regardless of
    // value; using the real digest keeps semantics aligned with
    // classic rsync so the receiver may short-circuit equal files.
    FileListEntry {
        flags: BASELINE_FLAGS,
        path: name,
        size: size as i64,
        mtime: mtime_secs,
        // MOD_NSEC requires a value on the wire even if subsec is zero;
        // emit Some(0) in that case to keep the encoder + decoder paths
        // consistent.
        mtime_nsec: Some(mtime_nsec_opt.unwrap_or(0)),
        mode: file_mode_from_metadata(metadata),
        uid: Some(uid_value as i64),
        uid_name: Some(uid_name),
        gid: Some(gid_value as i64),
        gid_name: Some(gid_name),
        checksum: file_checksum,
        // Symlink source detection is deliberately not wired here: the
        // single-file delta path is only reached with regular-file
        // metadata today, and transferring a symlink also requires
        // bypassing the data/delta phase (a symlink carries no content
        // stream) plus a byte-fixture capture against stock rsync to
        // validate the end-to-end flow. The wire codec
        // (`encode/decode_file_list_entry`) already round-trips symlink
        // entries; wiring the source builder + receiver is the tracked
        // follow-up (see PROTOCOL-RSYNC-COMPARE.md).
        symlink_target: None,
    }
}

/// Extract `(uid, gid)` from filesystem metadata. Falls back to (0, 0)
/// on non-Unix platforms (the native path is `#[cfg(unix)]` at the
/// callsite today, so this branch is unreachable in production).
fn file_owner_components(metadata: &std::fs::Metadata) -> (u32, u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        (metadata.uid(), metadata.gid())
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        (0, 0)
    }
}

/// Look up the user name for `uid` via `getpwuid_r`. On lookup failure
/// or non-Unix, returns the numeric uid as a string so the wire byte
/// `user_name length` is non-zero (avoids a 0-len name that some
/// receivers might mishandle when XMIT_USER_NAME_FOLLOWS is set).
fn lookup_user_name(uid: u32) -> String {
    #[cfg(unix)]
    unsafe {
        let mut pwd: libc::passwd = std::mem::zeroed();
        let mut buf = [0i8; 1024];
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        let rc = libc::getpwuid_r(
            uid as libc::uid_t,
            &mut pwd,
            buf.as_mut_ptr(),
            buf.len(),
            &mut result,
        );
        if rc == 0 && !result.is_null() && !pwd.pw_name.is_null() {
            if let Ok(s) = std::ffi::CStr::from_ptr(pwd.pw_name).to_str() {
                if !s.is_empty() && s.len() <= u8::MAX as usize {
                    return s.to_string();
                }
            }
        }
    }
    uid.to_string()
}

/// Look up the group name for `gid` via `getgrgid_r`. Same fallback
/// strategy as `lookup_user_name`.
fn lookup_group_name(gid: u32) -> String {
    #[cfg(unix)]
    unsafe {
        let mut grp: libc::group = std::mem::zeroed();
        let mut buf = [0i8; 1024];
        let mut result: *mut libc::group = std::ptr::null_mut();
        let rc = libc::getgrgid_r(
            gid as libc::gid_t,
            &mut grp,
            buf.as_mut_ptr(),
            buf.len(),
            &mut result,
        );
        if rc == 0 && !result.is_null() && !grp.gr_name.is_null() {
            if let Ok(s) = std::ffi::CStr::from_ptr(grp.gr_name).to_str() {
                if !s.is_empty() && s.len() <= u8::MAX as usize {
                    return s.to_string();
                }
            }
        }
    }
    gid.to_string()
}

/// Compute the 16-byte xxh128 digest of `data` and return it as the
/// little-endian byte sequence rsync expects on the wire (rsync stores
/// the digest as raw bytes in the order returned by `XXH128_digest`).
fn xxh128_digest_bytes(data: &[u8]) -> Vec<u8> {
    use xxhash_rust::xxh3::xxh3_128;
    let digest = xxh3_128(data);
    digest.to_le_bytes().to_vec()
}

/// P3-T01 W1.3: streaming xxh128 over a file path. Reads the file in
/// `XXH128_STREAM_BUF_BYTES`-bounded slabs and feeds them into a
/// reusable `Xxh3Default` hasher. Output layout matches
/// [`xxh128_digest_bytes`] exactly: `digest.to_le_bytes()`.
///
/// Used by `upload_inner` to populate the `FileListEntry::checksum`
/// field without holding the full source in memory. The OS page cache
/// makes the second read (inside the streaming delta phase) a hot hit
/// on typical workloads, so the practical I/O cost is one disk read.
async fn compute_xxh128_file_streaming(path: &Path) -> std::io::Result<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    use xxhash_rust::xxh3::Xxh3Default;
    /// Buffer size for the streaming xxh128 read. 4 MiB matches the
    /// driver's `STREAMING_READ_CHUNK_BYTES` so the page-cache fill
    /// stride is the same on both passes.
    const XXH128_STREAM_BUF_BYTES: usize = 4 * 1024 * 1024;

    let mut file = fs::File::open(path).await?;
    let mut hasher = Xxh3Default::new();
    let mut buf = vec![0u8; XXH128_STREAM_BUF_BYTES];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.digest128().to_le_bytes().to_vec())
}

/// Extract `(mtime_seconds_since_epoch, optional_nanoseconds)` from a
/// filesystem metadata entry. Falls back to `(0, None)` when `modified`
/// is not exposed (network filesystems, esoteric platforms). The wire
/// format uses an `i64` for seconds, matching the rsync 3.x file list
/// entry layout.
fn file_mtime_components(metadata: &std::fs::Metadata) -> (i64, Option<i32>) {
    match metadata.modified() {
        Ok(system_time) => match system_time.duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => (d.as_secs() as i64, Some(d.subsec_nanos() as i32)),
            Err(before) => (-(before.duration().as_secs() as i64), None),
        },
        Err(_) => (0, None),
    }
}

/// Pull the mode bits (`st_mode` on Unix, synthesised cross-platform
/// equivalent on Windows) out of metadata. `FileListEntry::mode` is a
/// `u32` that mirrors POSIX `st_mode`: high bits encode the file type
/// (`S_IFREG` / `S_IFDIR` / `S_IFLNK`), low bits encode the permission
/// triplet (owner/group/other rwx).
///
/// Z.4.3.f6 (Windows leg): the previous non-unix branch returned a bare
/// `0o644`, missing the `S_IFREG = 0o100000` file-type flag. Stock
/// `rsync --server` checks `S_ISREG(mode)` early in
/// `flist.c::recv_file_entry` and aborts with exit code 22
/// (`RERR_MALLOC`) when the entry advertises an unknown file type,
/// because the next branch tries to size-allocate based on a
/// file-type-specific path. The fix here mirrors what `stat(2)` would
/// have produced on Unix for the same file: file-type bits derived
/// from `FileType` (cross-platform), permission bits synthesised from
/// `readonly` to give either `0o644` (writable) or `0o444`
/// (read-only). Empty `mode == 0` would also fail the receiver's
/// validation, so the synthesis always emits a sensible default.
fn file_mode_from_metadata(metadata: &std::fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode()
    }
    #[cfg(not(unix))]
    {
        // POSIX `S_IF*` constants. Inlined to avoid a libc dependency
        // on the non-unix branch; values are stable on Linux/BSD.
        const S_IFREG: u32 = 0o100000;
        const S_IFDIR: u32 = 0o040000;
        const S_IFLNK: u32 = 0o120000;

        let file_type = metadata.file_type();
        let type_bits = if file_type.is_symlink() {
            S_IFLNK
        } else if file_type.is_dir() {
            S_IFDIR
        } else {
            // Default to regular file. Devices/sockets/fifos do not
            // exist as `FileType` on Windows, so anything else is
            // safely funnelled into the `S_IFREG` bucket here.
            S_IFREG
        };
        let perm_bits = if metadata.permissions().readonly() {
            0o444
        } else {
            0o644
        };
        type_bits | perm_bits
    }
}

/// Read the existing target file's Unix mode if the file is present and
/// readable. Used on download to restore mode + readonly semantics on
/// the temp file *before* the atomic rename, so in-place updates do not
/// silently drop perms (U-09).
async fn existing_mode_if_any(local_path: &Path) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match fs::metadata(local_path).await {
            Ok(meta) => Some(meta.permissions().mode()),
            Err(_) => None,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = local_path;
        None
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
        Err(shared) => shared.lock().map(|guard| guard.clone()).unwrap_or_default(),
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

/// Translate a typed `AerorsyncError` into the production `RsyncError`
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
fn map_native_error_to_rsync(err: AerorsyncError, committed: bool) -> RsyncError {
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

fn map_native_probe_error_to_rsync(err: AerorsyncError) -> RsyncError {
    if err.kind == AerorsyncErrorKind::HostKeyRejected {
        return map_native_error_to_rsync(err, false);
    }
    RsyncError::RemoteNotAvailable
}

/// Diagnostic helper: stable short tag for a [`RsyncError`] variant so
/// `tracing::debug!` calls can log "which arm of the enum did we see"
/// without dragging the full `Debug` impl (which can spill stderr blobs
/// onto a single line and confuse log scrapers). Used by Z.1.2 lane
/// captures to confirm the first attempt's error path matches the
/// classifier's expectation.
fn rsync_error_variant(err: &RsyncError) -> &'static str {
    match err {
        RsyncError::Io(_) => "Io",
        RsyncError::TransferFailed { .. } => "TransferFailed",
        RsyncError::HardRejection(_) => "HardRejection",
        RsyncError::PasswordAuthUnsupported => "PasswordAuthUnsupported",
        RsyncError::MissingPassword => "MissingPassword",
        RsyncError::MissingKey(_) => "MissingKey",
        RsyncError::VersionTooOld { .. } => "VersionTooOld",
        RsyncError::RemoteNotAvailable => "RemoteNotAvailable",
        RsyncError::LocalNotAvailable => "LocalNotAvailable",
        RsyncError::Cancelled => "Cancelled",
        RsyncError::TooSmall { .. } => "TooSmall",
        RsyncError::SpawnFailed(_) => "SpawnFailed",
        RsyncError::ProbeFailed(_) => "ProbeFailed",
    }
}

fn map_write_atomic_error(err: WriteAtomicError) -> RsyncError {
    match err {
        // Pre-open: nothing touched on disk yet → treat as Io, the
        // wrapper degrades to classic fallback for free.
        WriteAtomicError::PreOpen(io) => RsyncError::Io(io),
        // U-13 post-open split:
        //   * write / flush / sync_all / chmod → `local_path` is
        //     guaranteed untouched (rename has not happened yet) and the
        //     classic SFTP path writes to `local_path` directly without
        //     touching `.aerotmp`. Safe to degrade via the classic
        //     fallback envelope.
        //   * rename → the observable commit point; if this fails the
        //     user may see the old contents AND a leftover `.aerotmp`.
        //     Keep as `HardRejection` so classic does not silently
        //     attempt the same overwrite without acknowledgement.
        WriteAtomicError::PostOpen { stage, source } if stage != "rename" => {
            RsyncError::TransferFailed {
                exit: -1,
                stderr: format!(
                    "native fallback: atomic write failed at {} (target untouched): {}",
                    stage, source
                ),
            }
        }
        WriteAtomicError::PostOpen { stage, source } => {
            RsyncError::HardRejection(format!("atomic write failed at {}: {}", stage, source))
        }
    }
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
            let perms = std::fs::Permissions::from_mode(mode & 0o7777);
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

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aerorsync::types::AerorsyncErrorKind;
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
            let err = AerorsyncError::cancelled("user abort");
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
            AerorsyncErrorKind::UnsupportedVersion,
            AerorsyncErrorKind::NegotiationFailed,
            AerorsyncErrorKind::TransportFailure,
            AerorsyncErrorKind::RemoteError,
        ];
        for kind in kinds {
            let err = AerorsyncError::new(kind, "env");
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
        // sees the failure: no silent classic fallback.
        let err = AerorsyncError::host_key_rejected("fingerprint mismatch");
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
    fn map_probe_host_key_rejected_is_hard_rejection() {
        let err = AerorsyncError::host_key_rejected("probe fingerprint mismatch");
        let rs = map_native_probe_error_to_rsync(err);
        match rs {
            RsyncError::HardRejection(msg) => {
                assert!(msg.contains("HostKeyRejected"));
                assert!(msg.contains("probe fingerprint mismatch"));
            }
            other => panic!("probe HostKeyRejected must be hard, got {other:?}"),
        }
    }

    #[test]
    fn map_probe_environmental_error_is_remote_not_available() {
        let err = AerorsyncError::transport("rsync missing");
        let rs = map_native_probe_error_to_rsync(err);
        assert!(matches!(rs, RsyncError::RemoteNotAvailable));
    }

    #[test]
    fn map_post_commit_non_cancel_is_always_hard_rejection() {
        let kinds = [
            AerorsyncErrorKind::UnsupportedVersion,
            AerorsyncErrorKind::InvalidFrame,
            AerorsyncErrorKind::TransportFailure,
            AerorsyncErrorKind::NegotiationFailed,
            AerorsyncErrorKind::PlannerRejected,
            AerorsyncErrorKind::IllegalStateTransition,
            AerorsyncErrorKind::RemoteError,
            AerorsyncErrorKind::UnexpectedMessage,
            AerorsyncErrorKind::HostKeyRejected,
            AerorsyncErrorKind::Internal,
        ];
        for kind in kinds {
            let err = AerorsyncError::new(kind, "post-commit");
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
            AerorsyncErrorKind::InvalidFrame,
            AerorsyncErrorKind::IllegalStateTransition,
            AerorsyncErrorKind::PlannerRejected,
            AerorsyncErrorKind::UnexpectedMessage,
            AerorsyncErrorKind::Internal,
        ];
        for kind in kinds {
            let err = AerorsyncError::new(kind, "proto-bug");
            let rs = map_native_error_to_rsync(err, false);
            match rs {
                RsyncError::HardRejection(_) => {}
                other => panic!("pre-commit {kind:?} → expected HardRejection, got {other:?}"),
            }
        }
    }

    // -- build_source_entry -------------------------------------------------

    /// Helper: produce a real `std::fs::Metadata` by briefly writing an
    /// empty file. Keeps the tests close to production shape (they used
    /// to pass no metadata at all, which masked the mtime regression).
    fn metadata_for(path: &Path) -> std::fs::Metadata {
        if !path.exists() {
            std::fs::File::create(path).expect("create test file");
        }
        std::fs::metadata(path).expect("metadata on freshly created file")
    }

    #[test]
    fn build_source_entry_extracts_basename_and_sets_size() {
        let dir = fresh_tempdir();
        let path = dir.path().join("payload.bin");
        let meta = metadata_for(&path);
        let entry = build_source_entry(&path, 1_234_567, &meta, xxh128_digest_bytes(&[]));
        assert_eq!(entry.path, "payload.bin");
        assert_eq!(entry.size, 1_234_567);
        // U-07 regression pin: mtime MUST be populated from metadata;
        // hardcoding zero was the original bug.
        assert!(
            entry.mtime > 0,
            "mtime must reflect the source file (got {})",
            entry.mtime
        );
        // B.2 baseline: oracle's first-entry shape is
        // USER_NAME_FOLLOWS | GROUP_NAME_FOLLOWS | MOD_NSEC = 0x2c00.
        // uid/gid + names follow inline; xxh128 16-byte checksum trails.
        assert_eq!(entry.flags, (1 << 10) | (1 << 11) | (1 << 13));
        assert!(entry.uid.is_some(), "uid must be populated (preserve_uid)");
        assert!(entry.gid.is_some(), "gid must be populated (preserve_gid)");
        assert!(
            entry.uid_name.as_deref().is_some_and(|s| !s.is_empty()),
            "uid_name must be populated (XMIT_USER_NAME_FOLLOWS)"
        );
        assert!(
            entry.gid_name.as_deref().is_some_and(|s| !s.is_empty()),
            "gid_name must be populated (XMIT_GROUP_NAME_FOLLOWS)"
        );
        assert_eq!(
            entry.checksum.len(),
            16,
            "always_checksum on → 16-byte xxh128 digest required"
        );
        assert!(
            entry.mtime_nsec.is_some(),
            "MOD_NSEC requires mtime_nsec on the wire"
        );
    }

    #[test]
    fn build_source_entry_fallback_name_when_no_file_name() {
        // `/` has no file_name component; use any directory metadata as
        // a source (a directory is fine for the fallback check).
        let dir = fresh_tempdir();
        let meta = std::fs::metadata(dir.path()).unwrap();
        let entry = build_source_entry(Path::new("/"), 0, &meta, xxh128_digest_bytes(&[]));
        assert_eq!(entry.path, "source.bin");
    }

    #[test]
    fn build_source_entry_preserves_unix_mode() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir = fresh_tempdir();
            let path = dir.path().join("perm.bin");
            std::fs::File::create(&path).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
            let meta = std::fs::metadata(&path).unwrap();
            let entry = build_source_entry(&path, 0, &meta, xxh128_digest_bytes(&[]));
            // `mode` is the raw `st_mode` value; the low 12 bits carry
            // the permission bits we just set.
            assert_eq!((entry.mode as u32) & 0o7777, 0o640);
        }
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

        write_atomic_chunked(&target, b"NEW_CONTENTS", 4096, None, None, None)
            .await
            .expect("atomic write must succeed");

        let actual = fs::read(&target).await.unwrap();
        assert_eq!(actual, b"NEW_CONTENTS");
    }

    #[tokio::test]
    async fn write_atomic_creates_missing_target_file() {
        let dir = fresh_tempdir();
        let target = dir.path().join("fresh.bin");
        assert!(!target.exists());
        write_atomic_chunked(&target, b"NEW", 4096, None, None, None)
            .await
            .unwrap();
        assert_eq!(fs::read(&target).await.unwrap(), b"NEW");
    }

    #[tokio::test]
    async fn write_atomic_rejects_zero_chunk_size_pre_open() {
        let dir = fresh_tempdir();
        let target = dir.path().join("x.bin");
        let err = write_atomic_chunked(&target, b"DATA", 0, None, None, None)
            .await
            .expect_err("zero chunk must be rejected");
        assert!(matches!(err, WriteAtomicError::PreOpen(_)));
        // Pre-open rejection must not leave arbitrary temps lying around
        // for this target: with the U-14 unique suffix we cannot assert
        // on a deterministic tmp path (it is per-invocation), but we can
        // assert that no files with the `.aerotmp.` prefix appear in the
        // tempdir: because zero chunk fails before any open attempt.
        let entries = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".aerotmp."))
            .count();
        assert_eq!(entries, 0, "zero-chunk rejection must not open a temp");
    }

    #[tokio::test]
    async fn write_atomic_sparse_roundtrip_is_byte_identical() {
        // head non-zero, large middle hole, tail non-zero, then a fully
        // trailing hole. The file read back MUST equal the input bytes:
        // holes read as zeros. chunk_size 4096 so the middle and trailing
        // zero regions span whole chunks and are punched.
        let dir = fresh_tempdir();
        let target = dir.path().join("sparse.bin");
        let mut data = Vec::new();
        data.extend_from_slice(&[0xAB; 4096]); // dense head
        data.extend_from_slice(&[0u8; 4096 * 8]); // interior hole
        data.extend_from_slice(&[0xCD; 4096]); // dense tail
        data.extend_from_slice(&[0u8; 4096 * 4]); // trailing hole (exercises set_len)

        write_atomic_chunked_sparse(&target, &data, 4096, None, None, None)
            .await
            .expect("sparse write");

        let back = std::fs::read(&target).unwrap();
        assert_eq!(
            back.len(),
            data.len(),
            "size must match (set_len fixed trailing hole)"
        );
        assert_eq!(
            back, data,
            "sparse output must be byte-identical (holes read as zeros)"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn write_atomic_sparse_allocates_fewer_blocks_than_dense() {
        use std::os::unix::fs::MetadataExt;

        // 2 MiB of zeros bracketed by small dense markers. Sparse must
        // allocate strictly fewer 512-byte blocks than the dense write
        // for the same logical content on a hole-capable filesystem.
        let dir = fresh_tempdir();
        let dense = dir.path().join("dense.bin");
        let sparse = dir.path().join("sparse.bin");
        let mut data = vec![0u8; 2 * 1024 * 1024];
        data[..16].fill(0x11);
        let n = data.len();
        data[n - 16..].fill(0x22);

        write_atomic_chunked(&dense, &data, 64 * 1024, None, None, None)
            .await
            .expect("dense write");
        write_atomic_chunked_sparse(&sparse, &data, 64 * 1024, None, None, None)
            .await
            .expect("sparse write");

        // Logical content identical.
        assert_eq!(
            std::fs::read(&dense).unwrap(),
            std::fs::read(&sparse).unwrap()
        );

        let dense_blocks = std::fs::metadata(&dense).unwrap().blocks();
        let sparse_blocks = std::fs::metadata(&sparse).unwrap().blocks();
        assert!(
            sparse_blocks < dense_blocks,
            "sparse must allocate fewer blocks: sparse={sparse_blocks} dense={dense_blocks}"
        );
    }

    #[tokio::test]
    async fn write_atomic_sparse_all_zero_file_keeps_correct_length() {
        // A file that is entirely a hole: every chunk is seeked, nothing
        // is written, and set_len must still produce the exact length.
        let dir = fresh_tempdir();
        let target = dir.path().join("allzero.bin");
        let data = vec![0u8; 4096 * 5];
        write_atomic_chunked_sparse(&target, &data, 4096, None, None, None)
            .await
            .expect("all-zero sparse write");
        let back = std::fs::read(&target).unwrap();
        assert_eq!(back.len(), data.len());
        assert!(back.iter().all(|&b| b == 0));
    }

    #[tokio::test]
    async fn write_atomic_happy_path_cleans_its_own_temp() {
        // Complement to the stale-temp scenario now that the suffix is
        // per-invocation: on success the rename consumes the temp.
        let dir = fresh_tempdir();
        let target = dir.path().join("fresh.bin");
        write_atomic_chunked(&target, b"DATA", 4096, None, None, None)
            .await
            .unwrap();
        let leftovers = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".aerotmp."))
            .count();
        assert_eq!(leftovers, 0, "atomic rename must not leave any .aerotmp.*");
    }

    #[tokio::test]
    async fn write_atomic_preserves_mode_when_requested() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir = fresh_tempdir();
            let target = dir.path().join("mode.bin");
            std::fs::File::create(&target).unwrap();
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640)).unwrap();
            let original_mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o7777;
            assert_eq!(original_mode, 0o640);

            write_atomic_chunked(&target, b"NEW", 4096, None, Some(0o100640), None)
                .await
                .unwrap();

            let after_mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o7777;
            assert_eq!(
                after_mode, 0o640,
                "U-09: mode must be preserved across rename"
            );
        }
    }

    #[tokio::test]
    async fn write_atomic_preserves_mtime_when_requested() {
        let dir = fresh_tempdir();
        let target = dir.path().join("mtime.bin");
        let remote_mtime = (1_700_000_123_i64, Some(987_654_321_i32));

        write_atomic_chunked(&target, b"NEW", 4096, None, None, Some(remote_mtime))
            .await
            .unwrap();

        let meta = std::fs::metadata(&target).unwrap();
        let modified = filetime::FileTime::from_last_modification_time(&meta);
        assert_eq!(modified.unix_seconds(), remote_mtime.0);
        assert_eq!(modified.nanoseconds(), remote_mtime.1.unwrap() as u32);
    }

    // -- write_atomic_chunked mid-write drop invariant pin ----------------

    #[tokio::test]
    async fn write_atomic_preserves_old_on_future_drop_mid_write() {
        // U-12 renamed: this is a `timeout + drop` simulation, not a
        // real SIGKILL. The invariant tested is the rename-last atomicity
        // contract: after a mid-write future drop, `local_path` holds
        // either the OLD contents OR the NEW contents complete: never
        // a torn mix. Real SIGKILL preserves the same invariant because
        // the temp file is always a separate inode until rename.
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

        for interrupt_ms in [5u64, 12, 20, 35, 50] {
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
                    None,
                    None,
                ),
            )
            .await;

            assert!(
                res.is_err(),
                "iteration {interrupt_ms}ms: write completed before timeout: chunking tuning off"
            );

            let after = fs::read(&target).await.unwrap();
            assert_eq!(
                after, old,
                "iteration {interrupt_ms}ms: target MUST hold OLD contents intact after mid-write drop"
            );
        }
    }

    #[tokio::test]
    async fn write_atomic_post_open_pre_rename_is_classic_fallback() {
        // U-13 regression pin: a PostOpen failure at the `write` /
        // `flush` / `sync_all` / `chmod` stage must map to
        // `RsyncError::TransferFailed` (classic-fallback envelope),
        // because the target file is still untouched. Only a
        // `rename`-stage failure may escalate to HardRejection.
        let ioe = std::io::Error::other("simulated");
        let tf = map_write_atomic_error(WriteAtomicError::PostOpen {
            stage: "write",
            source: ioe,
        });
        match tf {
            RsyncError::TransferFailed { exit, stderr } => {
                assert_eq!(exit, -1);
                assert!(stderr.contains("write"));
                assert!(stderr.contains("target untouched"));
            }
            other => panic!("expected TransferFailed, got {other:?}"),
        }
        let ioe2 = std::io::Error::other("rename EXDEV");
        let hr = map_write_atomic_error(WriteAtomicError::PostOpen {
            stage: "rename",
            source: ioe2,
        });
        assert!(matches!(hr, RsyncError::HardRejection(_)));
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

    // -- W3.2(b3) batch session-reuse tests ---------------------------------

    fn write_test_file(dir: &TempDir, name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let path = dir.path().join(name);
        std::fs::File::create(&path)
            .expect("create test file")
            .write_all(bytes)
            .expect("write test file");
        path
    }

    #[tokio::test]
    async fn aerorsync_batch_reuses_ssh_session() {
        let cfg = crate::aerorsync::russh_session_transport::test_dummy_config();
        let transport = RusshSessionTransport::test_with_empty_handle(cfg, 1);
        let mut batch = AerorsyncBatch::new(transport, 1);
        let dir = fresh_tempdir();
        let local = write_test_file(&dir, "batch_reuse.bin", b"1234567890");

        // All operations fail because the test transport has no live handle,
        // but they still exercise the per-file open_raw_stream attempt path.
        for _ in 0..3 {
            let _ = batch.upload(&local, "/remote/reuse.bin").await;
        }

        assert_eq!(batch.transport.handshake_count(), 1);
    }

    #[tokio::test]
    async fn aerorsync_batch_per_file_open_raw_stream_count_equals_file_count() {
        let cfg = crate::aerorsync::russh_session_transport::test_dummy_config();
        let transport = RusshSessionTransport::test_with_empty_handle(cfg, 1);
        let mut batch = AerorsyncBatch::new(transport, 1);
        let dir = fresh_tempdir();
        let a = write_test_file(&dir, "a.bin", b"AAAA");
        let b = write_test_file(&dir, "b.bin", b"BBBB");

        let _ = batch.upload(&a, "/remote/a.bin").await;
        let _ = batch.upload(&b, "/remote/b.bin").await;
        let _ = batch
            .download("/remote/c.bin", &dir.path().join("c.bin"))
            .await;

        assert_eq!(batch.transport.raw_open_count(), 3);
    }

    #[tokio::test]
    async fn aerorsync_batch_finalize_returns_session_count_one_on_perfect_reuse() {
        let cfg = crate::aerorsync::russh_session_transport::test_dummy_config();
        let transport = RusshSessionTransport::test_with_empty_handle(cfg, 1);
        let batch = AerorsyncBatch::new(transport, 1);

        let stats = Box::new(batch)
            .finalize()
            .await
            .expect("finalize should succeed");

        assert_eq!(stats.session_count, 1);
    }

    #[tokio::test]
    async fn aerorsync_batch_session_count_increments_on_transient_reconnect() {
        // Reconnect orchestration is wired in a later step; this test pins the
        // reporting contract by simulating a transient reconnect on the shared
        // transport state before finalize.
        let cfg = crate::aerorsync::russh_session_transport::test_dummy_config();
        let transport = RusshSessionTransport::test_with_empty_handle(cfg, 1);
        let batch = AerorsyncBatch::new(transport, 1);
        batch.transport.test_set_handshake_count(2);

        let stats = Box::new(batch)
            .finalize()
            .await
            .expect("finalize should succeed");

        assert_eq!(stats.session_count, 2);
    }

    // -- from_rsync_config: Z.4.5 R1 wire-up --------------------------------

    /// Z.4.5 R1: when the production [`RsyncConfig`] carries an SSH
    /// password (a password-auth rsync-over-SSH profile), the
    /// constructor MUST
    /// propagate it onto [`SshTransportConfig::auth_password`] so the
    /// russh leg can pick it up. The propagation is independent of the
    /// `auth_method` discriminant: a profile may legitimately carry
    /// both a key and a password (e.g. for paranoid two-factor setups
    /// in the future); the actual selection happens inside
    /// `RusshSessionTransport::connect`.
    #[test]
    fn from_rsync_config_propagates_password_to_transport() {
        use crate::rsync_over_ssh::AuthMethod;
        use secrecy::{ExposeSecret, SecretString};
        use std::path::PathBuf;
        let dir = fresh_tempdir();
        let key_path = dir.path().join("id_dummy");
        std::fs::write(&key_path, b"-----BEGIN OPENSSH PRIVATE KEY-----\nfake\n").unwrap();

        let cfg = RsyncConfig {
            ssh_user: "tester".into(),
            ssh_host: "example.invalid".into(),
            ssh_port: Some(2222),
            ssh_key_path: Some(key_path.clone()),
            ssh_password: Some(SecretString::from("rsync-password".to_string())),
            auth_method: AuthMethod::SshKey,
            ..Default::default()
        };
        let transport =
            AerorsyncDeltaTransport::from_rsync_config(&cfg, SshHostKeyPolicy::AcceptAny)
                .expect("from_rsync_config should accept SshKey method with both materials");
        assert_eq!(transport.ssh_config.host, "example.invalid");
        assert_eq!(transport.ssh_config.port, 2222);
        assert_eq!(transport.ssh_config.private_key_path, key_path);
        let propagated = transport
            .ssh_config
            .auth_password
            .as_ref()
            .expect("ssh_password must be propagated");
        assert_eq!(propagated.expose_secret(), "rsync-password");
        // And the helper agrees:
        assert!(transport.ssh_config.usable_password().is_some());
        let _ = PathBuf::from("placeholder"); // silence unused import warning on some CI configs
    }

    /// Z.4.5 R1 dispatch step (2026-05-14): the boundary refusal of
    /// `auth_method=Password` is gone. A password-only `RsyncConfig`
    /// now produces a transport whose `auth_password` is set and whose
    /// `private_key_path` is the empty placeholder (the russh leg
    /// ignores the key path when `usable_password()` is Some).
    #[test]
    fn from_rsync_config_accepts_password_only_method() {
        use crate::rsync_over_ssh::AuthMethod;
        use secrecy::{ExposeSecret, SecretString};

        let cfg = RsyncConfig {
            ssh_user: "tester".into(),
            ssh_host: "example.invalid".into(),
            ssh_password: Some(SecretString::from("rsync-password".to_string())),
            auth_method: AuthMethod::Password,
            ..Default::default()
        };
        let transport =
            AerorsyncDeltaTransport::from_rsync_config(&cfg, SshHostKeyPolicy::AcceptAny)
                .expect("password-only RsyncConfig should now produce a transport");
        // Empty placeholder (NOT a default ~/.ssh path): russh leg ignores it.
        assert_eq!(
            transport.ssh_config.private_key_path,
            std::path::PathBuf::new(),
            "password-only profile must not silently inject a default key path"
        );
        let propagated = transport
            .ssh_config
            .auth_password
            .as_ref()
            .expect("ssh_password must be propagated");
        assert_eq!(propagated.expose_secret(), "rsync-password");
        assert!(transport.ssh_config.usable_password().is_some());
    }

    /// SSH agent auth: a `RsyncConfig { auth_method: Agent }` with no key
    /// and no password must produce a transport whose `auth_agent` flag
    /// is set, no password propagated, and an empty key placeholder. The
    /// russh leg resolves SSH_AUTH_SOCK at connect time; nothing static
    /// is validated or injected here.
    #[test]
    fn from_rsync_config_agent_method_sets_auth_agent_flag() {
        use crate::rsync_over_ssh::AuthMethod;

        let cfg = RsyncConfig {
            ssh_user: "tester".into(),
            ssh_host: "example.invalid".into(),
            ssh_key_path: None,
            ssh_password: None,
            auth_method: AuthMethod::Agent,
            ..Default::default()
        };
        let transport =
            AerorsyncDeltaTransport::from_rsync_config(&cfg, SshHostKeyPolicy::AcceptAny)
                .expect("agent RsyncConfig should produce a transport");
        assert!(
            transport.ssh_config.auth_agent,
            "auth_agent must be set for AuthMethod::Agent"
        );
        assert!(
            transport.ssh_config.auth_password.is_none(),
            "agent profile must not carry a password"
        );
        assert_eq!(
            transport.ssh_config.private_key_path,
            std::path::PathBuf::new(),
            "agent profile must not inject a default key path"
        );
        assert!(
            transport.ssh_config.prefers_russh_leg(),
            "agent profile must route through the russh leg"
        );
    }

    /// Z.4.5 R1 dispatch step: `validate_auth_material()` now gates the
    /// boundary instead of the old hard refusal. A `Password` method
    /// without a non-empty password surfaces `MissingPassword`, NOT
    /// `PasswordAuthUnsupported` (which has been removed from the
    /// boundary as of this step).
    #[test]
    fn from_rsync_config_password_method_without_password_returns_missing_password() {
        use crate::rsync_over_ssh::AuthMethod;

        let cfg = RsyncConfig {
            ssh_user: "tester".into(),
            ssh_host: "example.invalid".into(),
            ssh_password: None,
            ssh_key_path: Some(std::path::PathBuf::from("/tmp/key")),
            auth_method: AuthMethod::Password,
            ..Default::default()
        };
        match AerorsyncDeltaTransport::from_rsync_config(&cfg, SshHostKeyPolicy::AcceptAny) {
            Err(RsyncError::MissingPassword) => {}
            Err(other) => {
                panic!("expected MissingPassword via validate_auth_material, got Err({other:?})")
            }
            Ok(_) => panic!("expected MissingPassword, got Ok(_)"),
        }
    }

    /// Z.4.5 R1 dispatch step: empty SecretString must be rejected by
    /// `validate_auth_material()` so a misconfigured profile cannot
    /// reach the russh leg with a zero-length password.
    #[test]
    fn from_rsync_config_password_method_with_empty_password_returns_missing_password() {
        use crate::rsync_over_ssh::AuthMethod;
        use secrecy::SecretString;

        let cfg = RsyncConfig {
            ssh_user: "tester".into(),
            ssh_host: "example.invalid".into(),
            ssh_password: Some(SecretString::from(String::new())),
            auth_method: AuthMethod::Password,
            ..Default::default()
        };
        match AerorsyncDeltaTransport::from_rsync_config(&cfg, SshHostKeyPolicy::AcceptAny) {
            Err(RsyncError::MissingPassword) => {}
            Err(other) => panic!("expected MissingPassword, got Err({other:?})"),
            Ok(_) => panic!("expected MissingPassword, got Ok(_)"),
        }
    }

    /// Z.4.5 R1 dispatch step: a config that carries neither key nor
    /// password is still rejected as `HardRejection`. This is the
    /// "integration bug" guard from `validate_auth_material()`: it is
    /// not a credential failure (which the user can fix with input) but
    /// a wiring bug (the call site forgot to attach material). The
    /// dispatch must not silently fall back to another transport.
    #[test]
    fn from_rsync_config_with_no_auth_material_is_hard_rejection() {
        use crate::rsync_over_ssh::AuthMethod;

        let cfg = RsyncConfig {
            ssh_user: "tester".into(),
            ssh_host: "example.invalid".into(),
            ssh_password: None,
            ssh_key_path: None,
            auth_method: AuthMethod::SshKey,
            ..Default::default()
        };
        match AerorsyncDeltaTransport::from_rsync_config(&cfg, SshHostKeyPolicy::AcceptAny) {
            Err(RsyncError::HardRejection(message)) => {
                assert!(message.contains("neither ssh_key_path nor ssh_password"));
            }
            Err(other) => panic!("expected HardRejection, got Err({other:?})"),
            Ok(_) => panic!("expected HardRejection, got Ok(_)"),
        }
    }

    /// Regression: WD MyCloud early-close and transient delta failures.
    /// The delta `StreamingAtomicWriter` and the classic SFTP `AtomicFile`
    /// fallback share the SAME deterministic `<target>.aerotmp`, which the
    /// classic path opens with `create_new(true)`. If `do_download` bails
    /// out of a fallback-eligible path without clearing that temp, the
    /// fallback dies with `AlreadyExists` and a transient blip becomes a
    /// hard error instead of a transparent classic retry. `discard_streaming_temp`
    /// must remove the orphan while leaving the original target untouched.
    #[tokio::test]
    async fn abandon_path_discards_orphan_temp_so_classic_fallback_can_open() {
        let dir = fresh_tempdir();
        let target = dir.path().join("payload.md");
        // Pre-existing target content must survive: no rename happened on
        // the abandon path.
        std::fs::File::create(&target)
            .unwrap()
            .write_all(b"original-content")
            .unwrap();

        // Open the streaming writer the way `do_download` does. `new`
        // creates `<target>.aerotmp` and never touches the target.
        let writer = StreamingAtomicWriter::new(&target).await.unwrap();
        let temp = writer.temp_path().to_path_buf();
        assert!(temp.exists(), "writer must create the deterministic .aerotmp");

        // Without the fix the orphan blocks the classic fallback's open.
        let blocked = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)
            .await;
        assert_eq!(
            blocked.unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists,
            "an orphan temp blocks the classic AtomicFile create_new(true) open"
        );

        // The abandon path clears the orphan.
        discard_streaming_temp(writer).await;
        assert!(!temp.exists(), "orphan .aerotmp must be removed on abandon");

        // The classic fallback's exact open mode now succeeds.
        let reopened = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)
            .await;
        assert!(
            reopened.is_ok(),
            "classic create_new(true) must succeed once the orphan is gone"
        );
        drop(reopened);

        // The original target was never touched (no rename on the abandon path).
        assert_eq!(std::fs::read(&target).unwrap(), b"original-content");
    }
}
