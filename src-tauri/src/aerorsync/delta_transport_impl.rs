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
//!   Y-RSC.5: the signature phase also streams from `FileBaseline` via
//!   `send_signature_phase_from_baseline` (single `read_block` pass for
//!   rolling + wire strong). There is no bulk `tokio::fs::read` of the
//!   baseline on the production download path. Peak RSS is
//!   `O(block_size + writer_buffer)`, independent of baseline size.

#![cfg(feature = "aerorsync")]

use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::fs::{self, OpenOptions};
use tokio::io::{AsyncSeekExt, AsyncWrite, AsyncWriteExt};
use xxhash_rust::xxh3::Xxh3Default;

use crate::aerorsync::engine_adapter::{
    BaselineSource, CurrentDeltaSyncBridge, FileBaseline, MemoryBaseline,
};
use crate::aerorsync::fallback_policy::{classify_fallback, FallbackVerdict};
use crate::aerorsync::native_driver::{
    xxh128_wire_bytes, AerorsyncDriver, PreambleProfile, MD4_ALGO_NAME, MD5_ALGO_NAME,
    SHA1_ALGO_NAME, XXH128_ALGO_NAME, XXH3_ALGO_NAME, XXH64_ALGO_NAME,
};
use crate::aerorsync::real_wire::{is_symlink_mode, FileListEntry};
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
#[allow(dead_code)] // retained for production atomic-write path callers
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
    /// When true, negotiate `-X` and carry `user.*` xattrs (B3).
    /// Default off so frozen wire oracles and non-xattr paths stay
    /// byte-identical until a caller opts in via [`Self::with_xattrs`].
    preserve_xattrs: bool,
    /// X.5: turn ENOTSUP / metadata-loss warnings into hard errors.
    fail_on_metadata_loss: bool,
}

impl AerorsyncDeltaTransport {
    /// Primary constructor: takes a fully-populated SSH config and the
    /// size threshold below which delta is declined.
    pub fn new(ssh_config: SshTransportConfig, min_file_size: u64) -> Self {
        Self {
            ssh_config,
            min_file_size,
            preserve_xattrs: false,
            fail_on_metadata_loss: false,
        }
    }

    /// Opt this transport into extended attributes (`-X` + local
    /// read/apply of `user.*`). Off by default: see field docs.
    pub fn with_xattrs(mut self, preserve_xattrs: bool) -> Self {
        self.preserve_xattrs = preserve_xattrs;
        self
    }

    /// X.5: when the destination cannot store xattrs, fail the transfer
    /// instead of continuing with a typed warning.
    pub fn with_fail_on_metadata_loss(mut self, fail: bool) -> Self {
        self.fail_on_metadata_loss = fail;
        self
    }

    /// The xattr negotiation a batch must inherit, as one value.
    ///
    /// R3: the batch path calls `do_upload` / `do_download` itself, so it
    /// needs these flags explicitly. Reading them through a single accessor
    /// keeps `begin_batch` from drifting away from the single-file path,
    /// which is how the two ended up disagreeing in the first place.
    fn xattr_policy(&self) -> (bool, bool) {
        (self.preserve_xattrs, self.fail_on_metadata_loss)
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
        self.download_inner(remote_path, local_path, None).await
    }

    async fn download_with_progress(
        &self,
        remote_path: &str,
        local_path: &Path,
        progress: Option<crate::delta_transport::DeltaProgressSink>,
    ) -> Result<RsyncStats, RsyncError> {
        self.download_inner(remote_path, local_path, progress).await
    }

    async fn upload(&self, local_path: &Path, remote_path: &str) -> Result<RsyncStats, RsyncError> {
        self.upload_inner(local_path, remote_path, None).await
    }

    async fn upload_with_progress(
        &self,
        local_path: &Path,
        remote_path: &str,
        progress: Option<crate::delta_transport::DeltaProgressSink>,
    ) -> Result<RsyncStats, RsyncError> {
        self.upload_inner(local_path, remote_path, progress).await
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
            Ok(transport) => {
                let (preserve_xattrs, fail_on_metadata_loss) = self.xattr_policy();
                Ok(Box::new(AerorsyncBatch::new(
                    transport,
                    self.min_file_size,
                    preserve_xattrs,
                    fail_on_metadata_loss,
                )))
            }
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
        progress: Option<crate::delta_transport::DeltaProgressSink>,
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
                progress,
                self.preserve_xattrs,
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
                progress,
                self.preserve_xattrs,
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
    progress: Option<crate::delta_transport::DeltaProgressSink>,
    preserve_xattrs: bool,
) -> Result<RsyncStats, RsyncError>
where
    T: RawRemoteShellTransport + 'static,
{
    let start = Instant::now();
    // Y-RSC.4: lstat semantics. `fs::metadata` follows symlinks, which
    // silently uploaded the link TARGET's content as a regular file;
    // stock rsync with `-l` (already in the pinned server flag string)
    // preserves the link itself. For non-symlink paths the two calls
    // return identical metadata.
    let metadata = fs::symlink_metadata(local_path)
        .await
        .map_err(RsyncError::Io)?;
    if metadata.file_type().is_symlink() {
        // Symlinks bypass the `min_file_size` gate on purpose: the gate
        // exists to skip delta overhead on small FILE payloads, but a
        // symlink has no data phase at all, and a `TooSmall` refusal
        // would reroute it to the classic SFTP path, which materialises
        // the target's content instead of the link.
        return do_upload_symlink(
            transport,
            cancel,
            local_path,
            remote_path,
            &metadata,
            preamble_profile,
            progress,
            start,
            preserve_xattrs,
        )
        .await;
    }
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
    // The driver fills the file-list checksum only after the preamble
    // identifies the negotiated algorithm. The placeholder stays empty
    // here so upload cannot accidentally advertise xxh128 bytes to an
    // xxh64/xxh3/md5 receiver.
    let source_entry = build_source_entry(
        local_path,
        file_size,
        &metadata,
        Vec::new(),
        None,
        preserve_xattrs,
    );

    let source_file = fs::File::open(local_path).await.map_err(RsyncError::Io)?;

    let mut driver = AerorsyncDriver::new(transport, cancel)
        .with_preamble_profile(preamble_profile)
        .with_progress_sink(progress);
    let adapter = CurrentDeltaSyncBridge::new();
    let warnings = new_warnings_sink();
    let mut bridge = build_event_bridge(warnings.clone());

    // B.1: production dispatch now talks to stock `rsync --server`
    // (WrapperParity flavor) instead of the dev helper
    // `aerorsync_serve`. The wrapper command line is byte-pinned
    // against rsync 3.2.7 capture by `upload_remote_command_matches_capture`.
    // B3: `-X` rides on the same switch as local xattr read/apply.
    let spec = RemoteCommandSpec::upload(remote_path).with_xattrs(preserve_xattrs);
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

/// Y-RSC.4: upload a local symlink as a symlink (never its target's
/// content). The link travels entirely inside the file-list entry
/// (`S_IFLNK` mode + target string): the driver's
/// [`AerorsyncDriver::drive_upload_symlink`] skips the signature /
/// delta / data phases and the stock remote generator creates the link
/// from the flist.
///
/// Unix-only by construction: on non-Unix targets the function fails
/// closed with [`RsyncError::HardRejection`]. A soft error here would
/// re-route the transfer to the classic SFTP path, which follows the
/// link and silently materialises the target's content as a regular
/// file: the exact corruption this guard exists to prevent (mirror of
/// the module's `S_IFREG` non-Unix lesson, Z.4.3.f6).
#[allow(clippy::too_many_arguments)]
async fn do_upload_symlink<T>(
    transport: T,
    cancel: CancelHandle,
    local_path: &Path,
    remote_path: &str,
    metadata: &std::fs::Metadata,
    preamble_profile: PreambleProfile,
    progress: Option<crate::delta_transport::DeltaProgressSink>,
    start: Instant,
    preserve_xattrs: bool,
) -> Result<RsyncStats, RsyncError>
where
    T: RawRemoteShellTransport + 'static,
{
    #[cfg(not(unix))]
    {
        let _ = (
            transport,
            cancel,
            remote_path,
            metadata,
            preamble_profile,
            progress,
            start,
            preserve_xattrs,
        );
        return Err(RsyncError::HardRejection(format!(
            "symlink upload is not supported on this platform: {} is a symbolic link and \
             uploading it as a regular file would silently materialise its target's content",
            local_path.display()
        )));
    }
    #[cfg(unix)]
    {
        let target_os = fs::read_link(local_path).await.map_err(RsyncError::Io)?;
        let Some(target) = target_os.to_str().map(str::to_owned) else {
            // The proto-31 flist codec transports the target as UTF-8;
            // a non-UTF-8 target cannot round-trip, and degrading to the
            // classic path would materialise the target content instead.
            return Err(RsyncError::HardRejection(format!(
                "symlink target of {} is not valid UTF-8; cannot transport it on the wire",
                local_path.display()
            )));
        };
        // rsync F_LENGTH convention for links: st_size == strlen(target).
        let target_len = target.len() as u64;
        let source_entry = build_source_entry(
            local_path,
            target_len,
            metadata,
            Vec::new(),
            Some(target),
            preserve_xattrs,
        );

        let mut driver = AerorsyncDriver::new(transport, cancel)
            .with_preamble_profile(preamble_profile)
            .with_progress_sink(progress);
        let warnings = new_warnings_sink();
        let mut bridge = build_event_bridge(warnings.clone());

        let spec = RemoteCommandSpec::upload(remote_path).with_xattrs(preserve_xattrs);
        if let Err(e) = driver
            .drive_upload_symlink(spec, source_entry, &mut bridge)
            .await
        {
            return Err(map_native_error_to_rsync(e, driver.committed()));
        }
        if let Err(e) = driver.finish_session(&mut bridge).await {
            return Err(map_native_error_to_rsync(e, driver.committed()));
        }

        let duration_ms = start.elapsed().as_millis() as u64;
        let warnings = drain_warnings(warnings);
        Ok(build_stats(
            driver.session_stats(),
            target_len,
            duration_ms,
            warnings,
        ))
    }
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
        progress: Option<crate::delta_transport::DeltaProgressSink>,
    ) -> Result<RsyncStats, RsyncError> {
        let cancel = CancelHandle::inert();
        let preamble_profile = PreambleProfile::for_host(&self.ssh_config.host);
        if self.ssh_config.prefers_russh_leg() {
            let transport = RusshSessionTransport::connect(self.ssh_config.clone())
                .await
                .map_err(|e| map_native_error_to_rsync(e, false))?;
            do_download(
                transport,
                cancel,
                remote_path,
                local_path,
                preamble_profile,
                progress,
                self.preserve_xattrs,
                self.fail_on_metadata_loss,
            )
            .await
        } else {
            let transport = SshRemoteShellTransport::new(self.ssh_config.clone());
            do_download(
                transport,
                cancel,
                remote_path,
                local_path,
                preamble_profile,
                progress,
                self.preserve_xattrs,
                self.fail_on_metadata_loss,
            )
            .await
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

/// Audit S1: is a peer-supplied symlink target safe to materialise on a
/// download? Safe means it stays within the link's own directory: an
/// absolute target, or a relative target whose lexical resolution rises
/// above the directory the link is created in, is refused. This is the
/// `--safe-links` policy applied by default, resolved lexically without
/// touching the filesystem (no TOCTOU, no following the link).
#[cfg(unix)]
fn symlink_target_is_safe(target: &str) -> bool {
    use std::path::{Component, Path};
    let path = Path::new(target);
    if path.is_absolute() {
        return false;
    }
    let mut depth: i64 = 0;
    for component in path.components() {
        match component {
            Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    // Rose above the link's own directory.
                    return false;
                }
            }
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            // Absolute markers are already rejected above; treat any
            // residual root/prefix as unsafe rather than guessing.
            Component::RootDir | Component::Prefix(_) => return false,
        }
    }
    true
}

/// Y-RSC.4: materialise a downloaded symlink entry at `local_path` with
/// the create-at-temp-name + atomic-rename discipline of the regular
/// download path (`<target>.aerotmp` → rename; the rename is the
/// commit, so a kill-9 mid-create leaves the original path untouched).
///
/// Every failure maps to [`RsyncError::HardRejection`] on purpose: a
/// soft error would send `transfer_with_delta` down the classic SFTP
/// fallback, which follows the remote link and silently materialises
/// the TARGET's content as a regular file: exactly the corruption this
/// path must never produce (the module's non-Unix `S_IFREG` lesson,
/// Z.4.3.f6). That is also why the non-Unix arm fails closed instead of
/// writing anything.
///
/// The symlink's own mtime is restored best-effort via `utimensat(...,
/// AT_SYMLINK_NOFOLLOW)`: mirror of rsync's `CAN_SET_SYMLINK_TIMES`
/// behaviour, where a failed time-set on a link is a warning, never a
/// transfer failure.
async fn create_symlink_atomic(
    entry: &FileListEntry,
    local_path: &Path,
    remote_path: &str,
) -> Result<(), RsyncError> {
    let Some(target) = entry.symlink_target.as_deref().filter(|t| !t.is_empty()) else {
        return Err(RsyncError::HardRejection(format!(
            "symlink entry for {} carries no target string; refusing to guess",
            remote_path
        )));
    };
    #[cfg(unix)]
    {
        // Audit S1: the target is peer-controlled. A hostile server can
        // otherwise make a download materialise a link to an absolute
        // path (/etc/passwd) or a parent traversal; creating the link
        // does not follow it, but a later write-through would. Refuse an
        // unsafe target fail-closed (mirror of rsync `--safe-links`), the
        // secure default for a client pulling from an untrusted peer.
        if !symlink_target_is_safe(target) {
            return Err(RsyncError::HardRejection(format!(
                "symlink entry for {} has an unsafe target {} (absolute, or escaping the \
                 download directory); refusing (safe-links)",
                remote_path, target
            )));
        }
        // Same deterministic temp name as `StreamingAtomicWriter` (the
        // writer's temp was discarded just above, so the slot is free);
        // clear any stale leftover before `symlink`, which has
        // create-new semantics and would otherwise fail with EEXIST.
        let mut temp_os = local_path.as_os_str().to_owned();
        temp_os.push(TEMP_SUFFIX);
        let temp_path = PathBuf::from(temp_os);
        let _ = fs::remove_file(&temp_path).await;
        fs::symlink(target, &temp_path).await.map_err(|e| {
            RsyncError::HardRejection(format!(
                "cannot create symlink temp {} -> {}: {}",
                temp_path.display(),
                target,
                e
            ))
        })?;
        set_symlink_times_best_effort(&temp_path, entry.mtime, entry.mtime_nsec.unwrap_or(0));
        if let Err(e) = fs::rename(&temp_path, local_path).await {
            let _ = fs::remove_file(&temp_path).await;
            return Err(RsyncError::HardRejection(format!(
                "cannot commit symlink {} -> {}: {}",
                local_path.display(),
                target,
                e
            )));
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = local_path;
        Err(RsyncError::HardRejection(format!(
            "remote {} is a symbolic link (target {}); symlink creation is not supported on \
             this platform and downloading the target's content in its place would be silent \
             corruption",
            remote_path, target
        )))
    }
}

/// Best-effort `lutimes` equivalent: set the mtime of the link ITSELF
/// (never the target) via `utimensat(AT_FDCWD, path, times,
/// AT_SYMLINK_NOFOLLOW)`. Failure is logged at debug level and ignored,
/// mirroring rsync's warning-only handling when symlink times cannot be
/// set.
#[cfg(unix)]
fn set_symlink_times_best_effort(path: &Path, mtime_secs: i64, mtime_nsec: i32) {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return;
    };
    let times = [
        // atime: leave untouched.
        libc::timespec {
            tv_sec: 0,
            tv_nsec: libc::UTIME_OMIT,
        },
        libc::timespec {
            tv_sec: mtime_secs as libc::time_t,
            tv_nsec: mtime_nsec.max(0) as _,
        },
    ];
    // SAFETY: `c_path` is a valid NUL-terminated path and `times` is a
    // valid 2-element timespec array for the whole call.
    let rc = unsafe {
        libc::utimensat(
            libc::AT_FDCWD,
            c_path.as_ptr(),
            times.as_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc != 0 {
        tracing::debug!(
            "set_symlink_times_best_effort: utimensat({}) failed: {}",
            path.display(),
            std::io::Error::last_os_error()
        );
    }
}

/// CLAUDE-AV-B3-12: `AsyncWrite` shim that xxh3-128s every byte on its
/// way to the inner writer, so [`do_download`] can check the
/// reconstruction against the sender's whole-file trailer without a
/// second read pass over the temp file.
///
/// Wrapping the sink (rather than threading a hasher down through
/// `apply_delta_streaming`) keeps the driver and the engine adapter
/// untouched: the reconstruction is exactly the byte sequence the writer
/// accepts, in order, so hashing here is equivalent by construction and
/// stays O(1) in memory.
struct HashingWriter<'a, W> {
    inner: &'a mut W,
    hasher: Xxh3Default,
}

impl<'a, W> HashingWriter<'a, W> {
    fn new(inner: &'a mut W) -> Self {
        Self {
            inner,
            hasher: Xxh3Default::new(),
        }
    }

    fn digest128(&self) -> u128 {
        self.hasher.digest128()
    }
}

impl<W> AsyncWrite for HashingWriter<'_, W>
where
    W: AsyncWrite + Unpin + Send,
{
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let me = self.get_mut();
        let result = Pin::new(&mut *me.inner).poll_write(cx, buf);
        // Hash exactly the prefix the inner writer accepted. A short
        // write that hashed the whole `buf` would silently desynchronise
        // the digest from the bytes actually on disk, turning the guard
        // into a source of false corruption reports.
        if let Poll::Ready(Ok(n)) = &result {
            me.hasher.update(&buf[..*n]);
        }
        result
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let me = self.get_mut();
        Pin::new(&mut *me.inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let me = self.get_mut();
        Pin::new(&mut *me.inner).poll_shutdown(cx)
    }
}

/// Render a wire checksum for an operator-facing error string.
fn hex_checksum(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
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
#[allow(clippy::too_many_arguments)]
async fn do_download<T>(
    transport: T,
    cancel: CancelHandle,
    remote_path: &str,
    local_path: &Path,
    preamble_profile: PreambleProfile,
    progress: Option<crate::delta_transport::DeltaProgressSink>,
    preserve_xattrs: bool,
    fail_on_metadata_loss: bool,
) -> Result<RsyncStats, RsyncError>
where
    T: RawRemoteShellTransport + 'static,
{
    let start = Instant::now();
    // Y-RSC.5: open a streaming baseline only (no bulk `fs::read`).
    // Signatures and CopyBlock reconstruction both use
    // `BaselineSource::read_block`, so peak RAM is O(block_size) plus
    // the writer buffer. Reconstruction streams into a
    // `StreamingAtomicWriter` opened below.
    //
    // U-03: distinguish `NotFound` (legitimate empty baseline) from
    // every other `io::Error`. Before the fix, `unwrap_or_default()`
    // silently masked `PermissionDenied`, `EIO`, symlink loops, etc.
    // into "empty baseline", degrading the delta path to a full
    // download while hiding the underlying error from the user.
    //
    // U-09: capture the pre-existing mode so we can restore it on
    // the temp file before the atomic rename, preserving
    // perms / setuid / readonly across the in-place update.
    let baseline_mode = existing_mode_if_any(local_path).await;
    let mut baseline: Box<dyn BaselineSource + Send> = match fs::metadata(local_path).await {
        Ok(meta) if meta.is_file() => match FileBaseline::open(local_path).await {
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
        },
        Ok(_) => {
            // Directory / special file: same class as a non-NotFound
            // read failure under the old bulk path (fs::read would have
            // rejected these). Surface rather than silently full-download.
            return Err(RsyncError::TransferFailed {
                exit: -1,
                stderr: format!(
                    "native fallback: local baseline {} is not a regular file",
                    local_path.display()
                ),
            });
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // Legitimate empty baseline: target file does not exist
            // yet. Classic full-download semantics via the native
            // delta pipeline. Empty MemoryBaseline: no CopyBlocks
            // against an empty signature set, but the trait object is
            // still required by the streaming entry-point signature.
            Box::new(MemoryBaseline::new(Vec::new()))
        }
        Err(error) => {
            // Any other metadata failure must surface, not silently
            // degrade to full-size delta. Pre-commit classification
            // routes this through classic fallback with a visible
            // reason in the stderr string.
            return Err(RsyncError::TransferFailed {
                exit: -1,
                stderr: format!(
                    "native fallback: cannot inspect local baseline {}: {}",
                    local_path.display(),
                    error
                ),
            });
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

    let mut driver = AerorsyncDriver::new(transport, cancel)
        .with_preamble_profile(preamble_profile)
        .with_progress_sink(progress);
    let adapter = CurrentDeltaSyncBridge::new();
    let warnings = new_warnings_sink();
    let mut bridge = build_event_bridge(warnings.clone());

    // B.1: production dispatch now talks to stock `rsync --server --sender`
    // (WrapperParity flavor). Pinned against rsync 3.2.7 capture by
    // `download_remote_command_matches_capture`.
    // B3: `-X` rides on the same switch as local xattr apply.
    let spec = RemoteCommandSpec::download(remote_path).with_xattrs(preserve_xattrs);
    // CLAUDE-AV-B3-12: hash the reconstruction as it streams to disk so
    // the whole-file trailer can be checked below. The shim borrows
    // `writer` only for the drive; the digest outlives the scope so
    // `writer` is free again for the guards and the commit.
    let (drive_res, reconstructed_digest) = {
        let mut hashing_writer = HashingWriter::new(&mut writer);
        let res = driver
            .drive_download_through_delta_streaming(
                spec,
                &mut *baseline,
                &mut hashing_writer,
                &adapter,
                &mut bridge,
            )
            .await;
        (res, hashing_writer.digest128())
    };
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
    // Y-RSC.4: a symlink entry has no signature/delta/data phases: the
    // driver returned right after the flist and nothing reached the
    // streaming writer. Discard the (empty) temp and materialise the
    // link itself; every guard below (size, checksum, finalize) is
    // file-content machinery that does not apply to links.
    if let Some(ref entry) = remote_entry {
        if is_symlink_mode(entry.mode) {
            let stats_snapshot = driver.session_stats().clone();
            discard_streaming_temp(writer).await;
            create_symlink_atomic(entry, local_path, remote_path).await?;
            let duration_ms = start.elapsed().as_millis() as u64;
            let warnings = drain_warnings(warnings);
            return Ok(build_stats(
                &stats_snapshot,
                entry.size.max(0) as u64,
                duration_ms,
                warnings,
            ));
        }
    }
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

    // Whole-file checksum guard (CLAUDE-AV-B3-12 / CLAUDE-AV-B3-14). The
    // size guard above only catches truncation; it cannot see corruption.
    // rsync closes the delta stream with the sender's strong checksum over
    // the WHOLE reconstructed file (`match.c::match_sums` →
    // `sum_init(xfer_sum_nni, checksum_seed)` … `sum_end(sender_file_sum)`
    // → `write_buf(f, sender_file_sum, xfer_sum_len)`). We already
    // received and stored that trailer and then never looked at it, so a
    // weak-hash false match in `engine_adapter::find_match` reconstructed
    // wrong bytes and reported success. Verifying it also catches the
    // sender's deliberate bad-checksum signal: on a source read error
    // `match.c` intentionally transmits an all-zero sum so the receiver
    // refuses the file.
    //
    // INTEROP: run ONLY for algorithms we can recompute in-tree (xxh128
    // via the streaming `HashingWriter`; xxh3/xxh64/md5/md4/sha1 via a
    // page-cache re-read of the temp). Against a peer that negotiated
    // anything else (sha256, sha512, none: reachable only through the
    // env override) the check is a deliberate no-op so a verify that
    // assumed the wrong digest cannot silently disable delta forever.
    //
    // HASHER DESIGN (CLAUDE-AV-B3-14): `HashingWriter` is constructed
    // BEFORE the drive, but the negotiated algo is only known AFTER it
    // (the preamble is exchanged inside the drive). We keep the streaming
    // xxh3 shim always-on for the xxh128 fast path (zero extra I/O), and
    // only when md5 wins do we re-hash the just-written temp from disk.
    // That second read is a page-cache hit on typical workloads (see
    // `compute_xxh128_file_streaming`); dual-hashing every download would
    // throttle the xxh128 path at md5 disk speed for no gain.
    //
    // SEED: rsync's FILE checksum is UNSEEDED for every negotiable
    // algorithm, md4 and sha1 included. `checksum.c::sum_init` calls
    // `XXH3_128bits_reset` / `md5_begin` / `mdfour_begin` (or
    // `EVP_DigestInit_ex(..., NULL)` under OpenSSL) and ignores its
    // `seed` argument; the only seeded trailers are the legacy
    // `CSUM_MD4_OLD/BUSTED/ARCHAIC` variants, which name negotiation can
    // never select (the negotiated "md4" is the modern `CSUM_MD4`). The
    // per-BLOCK checksum does seed via `get_checksum2`. Our sender
    // mirrors that asymmetry already, so `checksum_seed` must NOT enter
    // here: feeding it in would break against real rsync AND against our
    // own server.
    match driver.negotiated_checksum_algo() {
        Some(XXH128_ALGO_NAME) => {
            if let Some(expected) = driver.received_file_checksum() {
                let actual = xxh128_wire_bytes(reconstructed_digest);
                if expected != actual.as_slice() {
                    let stderr = format!(
                        "delta reconstruction checksum mismatch for {}: sender sent xxh128 {}, \
                         reconstruction hashes to {} ({} bytes); falling back to classic download",
                        remote_path,
                        hex_checksum(expected),
                        hex_checksum(&actual),
                        file_size
                    );
                    discard_streaming_temp(writer).await;
                    return Err(RsyncError::TransferFailed { exit: -1, stderr });
                }
            }
        }
        // Non-xxh128 peers. Re-read the temp (flushed) rather than
        // dual-hash during the drive; see HASHER DESIGN above.
        Some(
            algo @ (MD5_ALGO_NAME | XXH3_ALGO_NAME | XXH64_ALGO_NAME | MD4_ALGO_NAME
            | SHA1_ALGO_NAME),
        ) => {
            if let Some(expected) = driver.received_file_checksum() {
                if let Err(e) = writer.flush().await {
                    let stderr = format!(
                        "delta reconstruction checksum flush failed for {}: {}; \
                         falling back to classic download",
                        remote_path, e
                    );
                    discard_streaming_temp(writer).await;
                    return Err(RsyncError::TransferFailed { exit: -1, stderr });
                }
                let actual = match algo {
                    MD5_ALGO_NAME => compute_md5_file_streaming(writer.temp_path()).await,
                    XXH3_ALGO_NAME => compute_xxh3_file_streaming(writer.temp_path()).await,
                    XXH64_ALGO_NAME => compute_xxh64_file_streaming(writer.temp_path()).await,
                    MD4_ALGO_NAME => compute_md4_file_streaming(writer.temp_path()).await,
                    SHA1_ALGO_NAME => compute_sha1_file_streaming(writer.temp_path()).await,
                    _ => unreachable!("match arm restricts the negotiated checksum algorithm"),
                };
                let actual = match actual {
                    Ok(v) => v,
                    Err(e) => {
                        let stderr = format!(
                            "delta reconstruction checksum re-read failed for {}: {}; \
                             falling back to classic download",
                            remote_path, e
                        );
                        discard_streaming_temp(writer).await;
                        return Err(RsyncError::TransferFailed { exit: -1, stderr });
                    }
                };
                if expected != actual.as_slice() {
                    let stderr = format!(
                        "delta reconstruction checksum mismatch for {}: sender sent {} {}, \
                         reconstruction hashes to {} ({} bytes); falling back to classic download",
                        remote_path,
                        algo,
                        hex_checksum(expected),
                        hex_checksum(&actual),
                        file_size
                    );
                    discard_streaming_temp(writer).await;
                    return Err(RsyncError::TransferFailed { exit: -1, stderr });
                }
            }
        }
        _ => {
            // Unimplemented algo (sha256 / sha512 / none, or an absent
            // negotiation): leave the delta path alone. The no-op is what
            // keeps this shippable without live fixtures for every peer
            // flavour. Y-RSC.3 moved md4 and sha1 out of this arm.
        }
    }

    // B3 / X.4: xattrs must land on the temp file before rename so a
    // kill-9 never leaves a visible target without its metadata.
    let apply_xattrs = remote_entry
        .as_ref()
        .and_then(|e| e.xattrs.as_ref())
        .filter(|_| preserve_xattrs)
        .cloned();

    // Atomic commit: flush + sync_all + chmod (Unix) + set_mtime +
    // xattrs + rename. Failures here are post-commit-cutover and surface
    // as `HardRejection` via `map_write_atomic_error`.
    let xattr_warnings = writer
        .finalize(
            preserve_mode,
            preserve_mtime,
            apply_xattrs,
            fail_on_metadata_loss,
        )
        .await
        .map_err(map_write_atomic_error)?;

    let duration_ms = start.elapsed().as_millis() as u64;
    let mut warnings = drain_warnings(warnings);
    warnings.extend(xattr_warnings);
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
    /// R3: the xattr negotiation of the transport that opened this batch.
    /// Held here because the batch path calls `do_upload`/`do_download`
    /// directly and would otherwise hard-code the flags off, dropping the
    /// attributes of a caller that asked for them without a warning.
    preserve_xattrs: bool,
    /// Companion of `preserve_xattrs` (X.5): turns an ENOTSUP warning into
    /// a hard error. Carried for the same reason.
    fail_on_metadata_loss: bool,
}

impl AerorsyncBatch {
    fn new(
        transport: RusshSessionTransport,
        min_file_size: u64,
        preserve_xattrs: bool,
        fail_on_metadata_loss: bool,
    ) -> Self {
        let flag = Arc::new(AtomicBool::new(false));
        Self {
            transport,
            cancel: CancelHandle::new(flag.clone(), None),
            min_file_size,
            files_transferred: AtomicU64::new(0),
            bytes_on_wire: AtomicU64::new(0),
            cancel_observed: flag,
            preserve_xattrs,
            fail_on_metadata_loss,
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
            None,
            self.preserve_xattrs,
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
                    None,
                    self.preserve_xattrs,
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
            None,
            self.preserve_xattrs,
            self.fail_on_metadata_loss,
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
                    None,
                    self.preserve_xattrs,
                    self.fail_on_metadata_loss,
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
/// `symlink_target` is `Some` only on the Y-RSC.4 symlink-upload path:
/// the caller resolved it with `read_link` after an lstat-style
/// `symlink_metadata` probe, and `metadata` then carries `S_IFLNK` mode
/// bits plus `st_size == strlen(target)`. Regular uploads pass `None`
/// and are byte-identical to before the parameter existed. The entry is
/// still a FIRST-list-entry shape either way: explicit mtime/mode, no
/// `XMIT_SAME_*` compression (audit 2026-07-21 §4.1: SAME flags are
/// legal only from the second entry on).
fn build_source_entry(
    local_path: &Path,
    size: u64,
    metadata: &std::fs::Metadata,
    file_checksum: Vec<u8>,
    symlink_target: Option<String>,
    preserve_xattrs: bool,
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
    // B3 / X.3: when the session negotiates `-X`, read `user.*` xattrs
    // via libc. `None` when xattrs are off keeps the encoder emitting
    // zero xattr bytes (frozen oracles stay byte-identical). `Some(vec)`
    // — empty or not — when on matches the codec contract for negotiated
    // sessions.
    let xattrs = if preserve_xattrs {
        Some(crate::aerorsync::xattr_fs::read_user_xattrs(local_path).unwrap_or_default())
    } else {
        None
    };
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
        symlink_target,
        xattrs,
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
#[allow(dead_code)] // helper retained for checksum parity paths
fn xxh128_digest_bytes(data: &[u8]) -> Vec<u8> {
    use xxhash_rust::xxh3::xxh3_128;
    let digest = xxh3_128(data);
    digest.to_le_bytes().to_vec()
}

/// CLAUDE-AV-B3-14: streaming md5 over a file path. Twin of
/// the xxhash readers: same slab size and page-cache argument, used by
/// the download-side whole-file verify when the peer
/// negotiated md5. Output is the raw 16-byte digest rsync puts on the
/// wire (`sum_end` for `CSUM_MD5`); the trailer is unseeded, so
/// `checksum_seed` must NOT enter here.
async fn compute_md5_file_streaming(path: &Path) -> std::io::Result<Vec<u8>> {
    use md5::{Digest, Md5};
    use tokio::io::AsyncReadExt;
    /// Same 4 MiB stride as the xxh128 twin so the page-cache fill
    /// matches the just-written reconstruction.
    const MD5_STREAM_BUF_BYTES: usize = 4 * 1024 * 1024;

    let mut file = fs::File::open(path).await?;
    let mut hasher = Md5::new();
    let mut buf = vec![0u8; MD5_STREAM_BUF_BYTES];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().to_vec())
}

/// Y-RSC.3: streaming md4 over a file path, twin of
/// [`compute_md5_file_streaming`] for peers that negotiated the legacy
/// md4. Output is the raw 16-byte digest rsync puts on the wire
/// (`sum_end` for the modern `CSUM_MD4`); the trailer is UNSEEDED (only
/// the pre-negotiation `CSUM_MD4_OLD/BUSTED/ARCHAIC` variants seed
/// `sum_init`, and name negotiation can never select those), so
/// `checksum_seed` must NOT enter here.
async fn compute_md4_file_streaming(path: &Path) -> std::io::Result<Vec<u8>> {
    use md4::{Digest, Md4};
    use tokio::io::AsyncReadExt;
    const STREAM_BUF_BYTES: usize = 4 * 1024 * 1024;

    let mut file = fs::File::open(path).await?;
    let mut hasher = Md4::new();
    let mut buf = vec![0u8; STREAM_BUF_BYTES];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().to_vec())
}

/// Y-RSC.3: streaming sha1 over a file path for peers that negotiated
/// sha1 (reachable via the `AEROFTP_RSYNC_CSUM_ALGOS` override). Output
/// is the raw 20-byte digest; rsync's sha1 whole-file sum runs through
/// `EVP_DigestInit_ex(..., NULL)` and is unseeded like every other
/// negotiable trailer.
async fn compute_sha1_file_streaming(path: &Path) -> std::io::Result<Vec<u8>> {
    use sha1::{Digest, Sha1};
    use tokio::io::AsyncReadExt;
    const STREAM_BUF_BYTES: usize = 4 * 1024 * 1024;

    let mut file = fs::File::open(path).await?;
    let mut hasher = Sha1::new();
    let mut buf = vec![0u8; STREAM_BUF_BYTES];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().to_vec())
}

async fn compute_xxh3_file_streaming(path: &Path) -> std::io::Result<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    use xxhash_rust::xxh3::Xxh3Default;
    const STREAM_BUF_BYTES: usize = 4 * 1024 * 1024;

    let mut file = fs::File::open(path).await?;
    let mut hasher = Xxh3Default::new();
    let mut buf = vec![0u8; STREAM_BUF_BYTES];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.digest().to_le_bytes().to_vec())
}

async fn compute_xxh64_file_streaming(path: &Path) -> std::io::Result<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    use xxhash_rust::xxh64::Xxh64;
    const STREAM_BUF_BYTES: usize = 4 * 1024 * 1024;

    let mut file = fs::File::open(path).await?;
    let mut hasher = Xxh64::new(0);
    let mut buf = vec![0u8; STREAM_BUF_BYTES];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.digest().to_le_bytes().to_vec())
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
        copy_blocks: stats.copy_blocks,
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

    // -- CLAUDE-AV-B3-12: whole-file checksum verify on native delta -------

    /// Script a complete single-file download session for the mock
    /// transport: server preamble (advertising `peer_algos`) + file list
    /// entry + terminator + the sender's signature-phase echo + a delta
    /// stream that is one whole-file Literal + the closing summary frame.
    ///
    /// One Literal and an absent local baseline keep the real
    /// `CurrentDeltaSyncBridge` out of the picture: with no baseline
    /// there are no signatures, so the sender cannot emit a CopyBlock and
    /// the reconstruction is exactly `content`. That isolates the
    /// trailer check, which is what these tests are about.
    fn download_session_inbound(content: &[u8], peer_algos: &str, trailer: Vec<u8>) -> Vec<u8> {
        use crate::aerorsync::real_wire::{
            compress_zstd_literal_stream, encode_delta_stream, encode_file_list_entry,
            encode_file_list_terminator, encode_item_flags, encode_ndx, encode_server_preamble,
            encode_sum_head, encode_summary_frame, DeltaOp, DeltaStreamReport,
            FileListDecodeOptions, FileListEntry, MuxHeader, MuxTag, NdxState, ServerPreamble,
            SumHead, SummaryFrame,
        };

        fn mux(payload: &[u8]) -> Vec<u8> {
            let header = MuxHeader {
                tag: MuxTag::Data,
                length: payload.len() as u32,
            };
            let mut out = header.encode().to_vec();
            out.extend_from_slice(payload);
            out
        }

        // Mirrors `native_driver::tests::sample_file_list_entry`, but the
        // size must be the real content length or the pre-existing
        // completeness guard rejects the file before ours ever runs.
        // XMIT_SAME_MODE is deliberately NOT set: the mode has to travel
        // so `finalize` chmods the temp to something it can still stamp
        // an mtime onto (a transmitted mode of 0 means chmod 0o000, and
        // the commit then fails with EPERM at the mtime step).
        const XMIT_SAME_UID: u32 = 0x0008;
        const XMIT_SAME_GID: u32 = 0x0010;
        const XMIT_LONG_NAME: u32 = 0x0040;
        const XMIT_SAME_TIME: u32 = 0x0080;
        // CLAUDE-AV-B3-18: file-list digest and delta trailer use the same
        // negotiated checksum width. The fixture's trailer therefore
        // supplies the authoritative width instead of assuming 16.
        let checksum_len = trailer.len();
        let entry = FileListEntry {
            flags: XMIT_LONG_NAME | XMIT_SAME_UID | XMIT_SAME_GID | XMIT_SAME_TIME,
            path: "target.bin".to_string(),
            size: content.len() as i64,
            mtime: 0,
            mtime_nsec: None,
            mode: 0o100_644,
            uid: None,
            uid_name: None,
            gid: None,
            gid_name: None,
            checksum: vec![0xAA; checksum_len],
            symlink_target: None,
            xattrs: None,
        };
        let opts = FileListDecodeOptions {
            protocol: 31,
            xfer_flags_as_varint: true,
            always_checksum: true,
            csum_len: checksum_len,
            preserve_uid: true,
            preserve_gid: true,
            previous_name: None,
            preserve_xattrs: false,
        };

        // Sender's signature-phase echo: ndx + iflags + an empty sum_head
        // (no baseline → no blocks), matching `download_sender_prefix()`.
        let mut ndx_state = NdxState::default();
        let mut prefix = encode_ndx(1, &mut ndx_state);
        prefix.extend_from_slice(&encode_item_flags(0x8002));
        prefix.extend_from_slice(&encode_sum_head(&SumHead {
            count: 0,
            block_length: 512,
            checksum_length: 2,
            remainder_length: 0,
        }));

        let compressed =
            compress_zstd_literal_stream(&[content]).expect("zstd compress fixture literal");
        let delta_bytes = encode_delta_stream(&DeltaStreamReport {
            ops: vec![DeltaOp::Literal {
                compressed_payload: compressed[0].clone(),
            }],
            file_checksum: trailer,
        });

        // The real download tail: `PRE_SUMMARY_NDX_DONE_COUNT_DOWNLOAD`
        // (3) NDX_DONE markers ahead of the summary, which is what
        // `drain_leading_ndx_done_download` expects from rsync.
        let mut summary = vec![0x00; 3];
        summary.extend_from_slice(&encode_summary_frame(
            &SummaryFrame {
                total_read: 12_345,
                total_written: content.len() as i64,
                total_size: content.len() as i64,
                flist_buildtime: Some(1),
                flist_xfertime: Some(0),
            },
            31,
        ));

        let mut inbound = encode_server_preamble(&ServerPreamble {
            protocol_version: 31,
            compat_flags: 0x07,
            checksum_algos: peer_algos.to_string(),
            compression_algos: "none zstd".to_string(),
            checksum_seed: 0xDEAD_BEEF,
            consumed: 0,
        });
        inbound.extend_from_slice(&mux(&encode_file_list_entry(&entry, &opts)));
        inbound.extend_from_slice(&mux(&encode_file_list_terminator(&opts)));
        inbound.extend_from_slice(&mux(&prefix));
        inbound.extend_from_slice(&mux(&delta_bytes));
        inbound.extend_from_slice(&mux(&summary));
        inbound
    }

    async fn run_download_fixture(
        dir: &TempDir,
        content: &[u8],
        peer_algos: &str,
        trailer: Vec<u8>,
    ) -> (Result<RsyncStats, RsyncError>, PathBuf) {
        run_download_fixture_with_profile(
            dir,
            content,
            PreambleProfile::default(),
            peer_algos,
            trailer,
        )
        .await
    }

    /// Y-RSC.3: like [`run_download_fixture`] but with an explicit client
    /// advertisement. Needed for winners outside the byte-pinned default
    /// list (sha1, sha256, ...), which in production become reachable
    /// only through the `AEROFTP_RSYNC_CSUM_ALGOS` override; the custom
    /// profile mirrors that override without touching process env (unit
    /// tests run in parallel).
    async fn run_download_fixture_with_profile(
        dir: &TempDir,
        content: &[u8],
        profile: PreambleProfile,
        peer_algos: &str,
        trailer: Vec<u8>,
    ) -> (Result<RsyncStats, RsyncError>, PathBuf) {
        use crate::aerorsync::mock::{MockRemoteShellTransport, MockTransportConfig};

        let local_path = dir.path().join("target.bin");
        let transport = MockRemoteShellTransport::new(
            MockTransportConfig::healthy_upload()
                .with_raw_inbound(download_session_inbound(content, peer_algos, trailer)),
        );
        let result = do_download(
            transport,
            CancelHandle::inert(),
            "/remote/target.bin",
            &local_path,
            profile,
            None,
            false,
            false,
        )
        .await;
        (result, local_path)
    }

    /// The guard's reason for existing: a delta whose reconstruction does
    /// not match the sender's whole-file checksum (the observable symptom
    /// of a weak-hash false match in `find_match`) must be refused before
    /// the atomic rename, not committed and reported as success.
    #[tokio::test]
    async fn download_refuses_reconstruction_that_fails_the_whole_file_checksum() {
        let dir = fresh_tempdir();
        let content = b"the bytes that actually arrive on the wire".to_vec();
        let (result, local_path) = run_download_fixture(
            &dir,
            &content,
            "xxh128 xxh3 xxh64 md5 md4",
            vec![0xCC; 16], // sender's trailer for some OTHER content
        )
        .await;

        match result {
            Err(RsyncError::TransferFailed { exit, stderr }) => {
                assert_eq!(exit, -1, "must be fallback-eligible, not a hard rejection");
                assert!(
                    stderr.contains("checksum mismatch") && stderr.contains("/remote/target.bin"),
                    "stderr must name the failure and the file: {stderr}"
                );
            }
            other => panic!("expected TransferFailed on checksum mismatch, got {other:?}"),
        }
        assert!(
            !local_path.exists(),
            "target must be untouched: the temp is never renamed onto it"
        );
        // StreamingAtomicWriter uses `<target>.aerotmp` (no salt); the
        // salted `temp_path_for` helper is for the bulk write path only.
        let streaming_temp = {
            let mut os = local_path.as_os_str().to_os_string();
            os.push(".aerotmp");
            PathBuf::from(os)
        };
        assert!(
            !streaming_temp.exists(),
            "temp must be discarded so the classic fallback can re-open it with create_new(true)"
        );
    }

    /// The interop guard for algorithms we still do not recompute.
    /// xxh128, xxh3, xxh64, md5, md4, and sha1 are verified (Y-RSC.3
    /// moved md4/sha1 out of this pin), so the skip case now points at
    /// sha256: reachable only when an `AEROFTP_RSYNC_CSUM_ALGOS`-shaped
    /// override advertises it, mirrored here via a custom profile. The
    /// same mismatching trailer that is fatal for the implemented algos
    /// has to commit here. Getting this wrong would silently disable
    /// delta for every peer that negotiated an unimplemented algo.
    #[tokio::test]
    async fn download_skips_the_verify_when_the_peer_negotiated_an_unimplemented_algo() {
        let dir = fresh_tempdir();
        let content = b"the bytes that actually arrive on the wire".to_vec();
        // "sha256" wins negotiation (32-byte trailer) and remains
        // unimplemented, so the verify must no-op.
        let (result, local_path) = run_download_fixture_with_profile(
            &dir,
            &content,
            PreambleProfile {
                checksum_algos: "sha256".to_string(),
                compression_algos: "zstd".to_string(),
            },
            "sha256",
            vec![0xCC; 32],
        )
        .await;

        assert!(
            result.is_ok(),
            "an unimplemented-algo peer must keep the pre-verify path, got {result:?}"
        );
        assert_eq!(
            tokio::fs::read(&local_path).await.expect("target written"),
            content,
            "reconstruction must still commit unchanged"
        );
    }

    /// The happy path: a trailer that matches commits normally. Without
    /// this, a guard that rejected everything would still pass the test
    /// above.
    #[tokio::test]
    async fn download_commits_when_the_whole_file_checksum_matches() {
        let dir = fresh_tempdir();
        let content = b"the bytes that actually arrive on the wire".to_vec();
        // Unseeded, matching rsync's `sum_init` for CSUM_XXH3_128 (it
        // ignores checksum_seed; only the legacy MD4 variants mix it in)
        // even though this fixture's preamble carries a nonzero seed.
        let trailer = xxh128_wire_bytes(xxhash_rust::xxh3::xxh3_128(&content));
        let (result, local_path) =
            run_download_fixture(&dir, &content, "xxh128 xxh3 xxh64 md5 md4", trailer).await;

        assert!(
            result.is_ok(),
            "a matching trailer must commit, got {result:?}"
        );
        assert_eq!(
            tokio::fs::read(&local_path).await.expect("target written"),
            content
        );
    }

    /// CLAUDE-AV-B3-14: md5 peer + wrong trailer must fail the same way
    /// as the xxh128 mismatch case. Pass peer_algos `"md5"` alone so
    /// md5 wins (our list is `xxh128 xxh3 xxh64 md5 md4`; the fixture
    /// preamble's default `"md5 xxh64"` would negotiate xxh64 and disarm
    /// the verify).
    #[tokio::test]
    async fn download_refuses_md5_reconstruction_that_fails_the_whole_file_checksum() {
        let dir = fresh_tempdir();
        let content = b"the bytes that actually arrive on the wire".to_vec();
        let (result, local_path) =
            run_download_fixture(&dir, &content, "md5", vec![0xCC; 16]).await;

        match result {
            Err(RsyncError::TransferFailed { exit, stderr }) => {
                assert_eq!(exit, -1, "must be fallback-eligible, not a hard rejection");
                assert!(
                    stderr.contains("checksum mismatch")
                        && stderr.contains("md5")
                        && stderr.contains("/remote/target.bin"),
                    "stderr must name the failure, the algo, and the file: {stderr}"
                );
            }
            other => panic!("expected TransferFailed on md5 checksum mismatch, got {other:?}"),
        }
        assert!(
            !local_path.exists(),
            "target must be untouched: the temp is never renamed onto it"
        );
        let streaming_temp = {
            let mut os = local_path.as_os_str().to_os_string();
            os.push(".aerotmp");
            PathBuf::from(os)
        };
        assert!(
            !streaming_temp.exists(),
            "temp must be discarded so the classic fallback can re-open it with create_new(true)"
        );
    }

    /// CLAUDE-AV-B3-14: md5 peer + correct unseeded trailer commits.
    /// Pins both the happy path and the seed decision (fixture preamble
    /// carries a nonzero `checksum_seed`; the matching digest is plain
    /// md5 over the file bytes, no seed mixed in).
    #[tokio::test]
    async fn download_commits_when_the_md5_whole_file_checksum_matches() {
        use md5::{Digest, Md5};
        let dir = fresh_tempdir();
        let content = b"the bytes that actually arrive on the wire".to_vec();
        let trailer = Md5::digest(&content).to_vec();
        let (result, local_path) = run_download_fixture(&dir, &content, "md5", trailer).await;

        assert!(
            result.is_ok(),
            "a matching md5 trailer must commit, got {result:?}"
        );
        assert_eq!(
            tokio::fs::read(&local_path).await.expect("target written"),
            content
        );
    }

    /// Y-RSC.3: an md4 peer with a wrong trailer must be refused, the
    /// same way as the md5 mismatch case. Before Y-RSC.3 this exact
    /// fixture committed (the verify was a deliberate no-op for md4);
    /// this test is the proof the no-op branch is gone for md4.
    #[tokio::test]
    async fn download_refuses_md4_reconstruction_that_fails_the_whole_file_checksum() {
        let dir = fresh_tempdir();
        let content = b"the bytes that actually arrive on the wire".to_vec();
        let (result, local_path) =
            run_download_fixture(&dir, &content, "md4", vec![0xCC; 16]).await;

        match result {
            Err(RsyncError::TransferFailed { exit, stderr }) => {
                assert_eq!(exit, -1, "must be fallback-eligible, not a hard rejection");
                assert!(
                    stderr.contains("checksum mismatch")
                        && stderr.contains("md4")
                        && stderr.contains("/remote/target.bin"),
                    "stderr must name the failure, the algo, and the file: {stderr}"
                );
            }
            other => panic!("expected TransferFailed on md4 checksum mismatch, got {other:?}"),
        }
        assert!(
            !local_path.exists(),
            "target must be untouched: the temp is never renamed onto it"
        );
    }

    /// Y-RSC.3: md4 peer + correct unseeded trailer commits. The
    /// fixture preamble carries a nonzero `checksum_seed` (0xDEAD_BEEF)
    /// while the matching digest is plain MD4 over the file bytes: the
    /// pin that the negotiated `CSUM_MD4` trailer never mixes the seed
    /// (only the pre-negotiation MD4_OLD/BUSTED/ARCHAIC variants do).
    /// Expected bytes from the independent python RFC 1320 oracle.
    #[tokio::test]
    async fn download_commits_when_the_md4_whole_file_checksum_matches() {
        let dir = fresh_tempdir();
        let content = b"the bytes that actually arrive on the wire".to_vec();
        let trailer = vec![
            0x7f, 0x58, 0x69, 0x36, 0x72, 0xf4, 0x02, 0xea, 0xcc, 0xcf, 0xec, 0xf4, 0xe2, 0x9d,
            0x50, 0x9a,
        ];
        let (result, local_path) = run_download_fixture(&dir, &content, "md4", trailer).await;

        assert!(
            result.is_ok(),
            "a matching md4 trailer must commit, got {result:?}"
        );
        assert_eq!(
            tokio::fs::read(&local_path).await.expect("target written"),
            content
        );
    }

    /// Y-RSC.3: sha1 peer + wrong 20-byte trailer must be refused. sha1
    /// is outside the byte-pinned default advertisement, so the fixture
    /// mirrors the `AEROFTP_RSYNC_CSUM_ALGOS=sha1` override with a
    /// custom profile; the peer list is the stock rsync 3.2.7 full
    /// advertisement, which includes sha1 on OpenSSL builds.
    #[tokio::test]
    async fn download_refuses_sha1_reconstruction_that_fails_the_whole_file_checksum() {
        let dir = fresh_tempdir();
        let content = b"the bytes that actually arrive on the wire".to_vec();
        let (result, local_path) = run_download_fixture_with_profile(
            &dir,
            &content,
            PreambleProfile {
                checksum_algos: "sha1".to_string(),
                compression_algos: "zstd".to_string(),
            },
            "xxh128 xxh3 xxh64 md5 md4 sha1 none",
            vec![0xCC; 20],
        )
        .await;

        match result {
            Err(RsyncError::TransferFailed { exit, stderr }) => {
                assert_eq!(exit, -1, "must be fallback-eligible, not a hard rejection");
                assert!(
                    stderr.contains("checksum mismatch")
                        && stderr.contains("sha1")
                        && stderr.contains("/remote/target.bin"),
                    "stderr must name the failure, the algo, and the file: {stderr}"
                );
            }
            other => panic!("expected TransferFailed on sha1 checksum mismatch, got {other:?}"),
        }
        assert!(
            !local_path.exists(),
            "target must be untouched: the temp is never renamed onto it"
        );
    }

    /// Y-RSC.3: sha1 peer + correct unseeded 20-byte trailer commits.
    /// Nonzero fixture seed + plain SHA1(content) trailer pins the
    /// unseeded whole-file decision for sha1 too. Expected bytes from
    /// python3 hashlib.
    #[tokio::test]
    async fn download_commits_when_the_sha1_whole_file_checksum_matches() {
        let dir = fresh_tempdir();
        let content = b"the bytes that actually arrive on the wire".to_vec();
        let trailer = vec![
            0x18, 0x39, 0x5a, 0xd3, 0x7c, 0x06, 0xe3, 0xab, 0xd7, 0xe9, 0x0a, 0x89, 0x83, 0x11,
            0x8b, 0x4c, 0xdf, 0x56, 0x0e, 0xe8,
        ];
        let (result, local_path) = run_download_fixture_with_profile(
            &dir,
            &content,
            PreambleProfile {
                checksum_algos: "sha1".to_string(),
                compression_algos: "zstd".to_string(),
            },
            "xxh128 xxh3 xxh64 md5 md4 sha1 none",
            trailer,
        )
        .await;

        assert!(
            result.is_ok(),
            "a matching sha1 trailer must commit, got {result:?}"
        );
        assert_eq!(
            tokio::fs::read(&local_path).await.expect("target written"),
            content
        );
    }

    #[tokio::test]
    async fn download_refuses_xxh64_and_xxh3_checksum_mismatches_and_preserves_target() {
        for algorithm in [XXH64_ALGO_NAME, XXH3_ALGO_NAME] {
            let dir = fresh_tempdir();
            let content = b"the bytes that actually arrive on the wire".to_vec();
            let original = b"the existing target must survive".to_vec();
            let target = dir.path().join("target.bin");
            tokio::fs::write(&target, &original)
                .await
                .expect("seed existing target");

            let (result, local_path) =
                run_download_fixture(&dir, &content, algorithm, vec![0xCC; 8]).await;
            match result {
                Err(RsyncError::TransferFailed { exit, stderr }) => {
                    assert_eq!(exit, -1);
                    assert!(
                        stderr.contains("checksum mismatch")
                            && stderr.contains(algorithm)
                            && stderr.contains("/remote/target.bin"),
                        "stderr must identify the algorithm and target: {stderr}"
                    );
                }
                other => {
                    panic!("expected {algorithm} checksum mismatch fallback, got {other:?}")
                }
            }
            assert_eq!(
                tokio::fs::read(&local_path)
                    .await
                    .expect("original target remains"),
                original,
                "{algorithm} mismatch must leave the target untouched"
            );
            let mut temp_name = local_path.as_os_str().to_os_string();
            temp_name.push(".aerotmp");
            assert!(
                !PathBuf::from(temp_name).exists(),
                "{algorithm} mismatch must discard the streaming temp"
            );
        }
    }

    #[tokio::test]
    async fn download_commits_when_xxh64_and_xxh3_whole_file_checksums_match() {
        let content = b"the bytes that actually arrive on the wire".to_vec();
        let cases = [
            (
                XXH64_ALGO_NAME,
                xxhash_rust::xxh64::xxh64(&content, 0)
                    .to_le_bytes()
                    .to_vec(),
            ),
            (
                XXH3_ALGO_NAME,
                xxhash_rust::xxh3::xxh3_64(&content).to_le_bytes().to_vec(),
            ),
        ];
        for (algorithm, trailer) in cases {
            let dir = fresh_tempdir();
            let (result, local_path) =
                run_download_fixture(&dir, &content, algorithm, trailer).await;
            assert!(
                result.is_ok(),
                "a matching {algorithm} trailer must commit, got {result:?}"
            );
            assert_eq!(
                tokio::fs::read(&local_path).await.expect("target written"),
                content
            );
        }
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
        let entry = build_source_entry(
            &path,
            1_234_567,
            &meta,
            xxh128_digest_bytes(&[]),
            None,
            false,
        );
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
        let entry = build_source_entry(
            Path::new("/"),
            0,
            &meta,
            xxh128_digest_bytes(&[]),
            None,
            false,
        );
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
            let entry = build_source_entry(&path, 0, &meta, xxh128_digest_bytes(&[]), None, false);
            // `mode` is the raw `st_mode` value; the low 12 bits carry
            // the permission bits we just set.
            assert_eq!((entry.mode as u32) & 0o7777, 0o640);
        }
    }

    // -- Y-RSC.4: symlink source entry + atomic link creation ---------------

    #[cfg(unix)]
    #[test]
    fn build_source_entry_symlink_carries_iflnk_mode_target_and_no_checksum() {
        let dir = fresh_tempdir();
        let target = "payload-target.bin";
        std::fs::write(dir.path().join(target), b"tgt").unwrap();
        let link = dir.path().join("entry.lnk");
        std::os::unix::fs::symlink(target, &link).unwrap();

        // lstat semantics: `symlink_metadata` describes the LINK, not
        // its target (mode carries S_IFLNK, len is strlen(target)).
        let meta = std::fs::symlink_metadata(&link).unwrap();
        assert!(meta.file_type().is_symlink());
        let entry = build_source_entry(
            &link,
            target.len() as u64,
            &meta,
            Vec::new(),
            Some(target.to_string()),
            false,
        );

        assert_eq!(entry.mode & 0o170000, 0o120000, "S_IFLNK mode bits");
        assert_eq!(entry.symlink_target.as_deref(), Some(target));
        assert_eq!(entry.size, target.len() as i64, "rsync F_LENGTH for links");
        assert!(
            entry.checksum.is_empty(),
            "symlink entries carry no flist checksum (proto >= 28)"
        );
        // First-entry shape (audit 2026-07-21 §4.1): explicit mtime and
        // mode, never XMIT_SAME_* compression.
        assert_eq!(entry.flags, (1 << 10) | (1 << 11) | (1 << 13));
        assert!(entry.mtime > 0, "explicit mtime required on first entry");
        assert!(entry.mtime_nsec.is_some(), "MOD_NSEC requires a value");
    }

    #[tokio::test]
    async fn create_symlink_atomic_rejects_entry_without_target() {
        let dir = fresh_tempdir();
        let dest = dir.path().join("no-target.lnk");
        let mut entry = crate::aerorsync::real_wire::FileListEntry {
            flags: 1 << 13,
            path: "no-target.lnk".to_string(),
            size: 0,
            mtime: 0,
            mtime_nsec: Some(0),
            mode: 0o120_777,
            uid: None,
            uid_name: None,
            gid: None,
            gid_name: None,
            checksum: vec![],
            symlink_target: None,
            xattrs: None,
        };
        let err = create_symlink_atomic(&entry, &dest, "/remote/no-target.lnk")
            .await
            .unwrap_err();
        assert!(matches!(err, RsyncError::HardRejection(_)));
        // Empty target string is equally refused: readlink can never
        // produce it, so it only appears from a malformed peer.
        entry.symlink_target = Some(String::new());
        let err = create_symlink_atomic(&entry, &dest, "/remote/no-target.lnk")
            .await
            .unwrap_err();
        assert!(matches!(err, RsyncError::HardRejection(_)));
        assert!(!dest.exists(), "nothing may be materialised on refusal");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_target_is_safe_rejects_absolute_and_traversal() {
        // Safe: the target stays within the link's own directory.
        assert!(symlink_target_is_safe("sibling.bin"));
        assert!(symlink_target_is_safe("sub/dir/file.bin"));
        assert!(symlink_target_is_safe("sub/../also-here.bin"));
        assert!(symlink_target_is_safe("./here.bin"));
        // Unsafe: absolute target.
        assert!(!symlink_target_is_safe("/etc/passwd"));
        // Unsafe: rises above the link's own directory.
        assert!(!symlink_target_is_safe("../secret"));
        assert!(!symlink_target_is_safe("sub/../../escape"));
        assert!(!symlink_target_is_safe(
            "../../../../home/user/.ssh/authorized_keys"
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn create_symlink_atomic_refuses_unsafe_target_fail_closed() {
        let dir = fresh_tempdir();
        let dest = dir.path().join("evil.lnk");
        let entry = crate::aerorsync::real_wire::FileListEntry {
            flags: 1 << 13,
            path: "evil.lnk".to_string(),
            size: 0,
            mtime: 0,
            mtime_nsec: Some(0),
            mode: 0o120_777,
            uid: None,
            uid_name: None,
            gid: None,
            gid_name: None,
            checksum: vec![],
            symlink_target: Some("../../../../etc/passwd".to_string()),
            xattrs: None,
        };
        let err = create_symlink_atomic(&entry, &dest, "/remote/evil.lnk")
            .await
            .unwrap_err();
        assert!(matches!(err, RsyncError::HardRejection(_)));
        assert!(!dest.exists(), "unsafe symlink must not be materialised");
        let mut temp_os = dest.as_os_str().to_owned();
        temp_os.push(TEMP_SUFFIX);
        assert!(
            !std::path::Path::new(&temp_os).exists(),
            "no temp may be left behind on refusal"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn create_symlink_atomic_replaces_existing_file_and_leaves_no_temp() {
        let dir = fresh_tempdir();
        let dest = dir.path().join("replace-me.lnk");
        std::fs::write(&dest, b"old regular content").unwrap();
        let entry = crate::aerorsync::real_wire::FileListEntry {
            flags: 1 << 13,
            path: "replace-me.lnk".to_string(),
            size: 11,
            mtime: 1_700_000_000,
            mtime_nsec: Some(0),
            mode: 0o120_777,
            uid: None,
            uid_name: None,
            gid: None,
            gid_name: None,
            checksum: vec![],
            symlink_target: Some("rel/tgt.bin".to_string()),
            xattrs: None,
        };
        create_symlink_atomic(&entry, &dest, "/remote/replace-me.lnk")
            .await
            .expect("atomic symlink replace");

        let meta = std::fs::symlink_metadata(&dest).unwrap();
        assert!(meta.file_type().is_symlink(), "dest must now be a symlink");
        assert_eq!(
            std::fs::read_link(&dest).unwrap(),
            PathBuf::from("rel/tgt.bin")
        );
        let temp = {
            let mut os = dest.as_os_str().to_os_string();
            os.push(TEMP_SUFFIX);
            PathBuf::from(os)
        };
        assert!(!temp.exists(), "temp must be renamed away, never leaked");
    }

    #[cfg(not(unix))]
    #[tokio::test]
    async fn create_symlink_atomic_fails_closed_on_non_unix() {
        // Compile-surface + behaviour guard for the Windows lane: a
        // symlink entry must surface a typed HardRejection, never a
        // silently materialised regular file (module S_IFREG lesson).
        let dir = fresh_tempdir();
        let dest = dir.path().join("refused.lnk");
        let entry = crate::aerorsync::real_wire::FileListEntry {
            flags: 1 << 13,
            path: "refused.lnk".to_string(),
            size: 7,
            mtime: 0,
            mtime_nsec: Some(0),
            mode: 0o120_777,
            uid: None,
            uid_name: None,
            gid: None,
            gid_name: None,
            checksum: vec![],
            symlink_target: Some("tgt.bin".to_string()),
            xattrs: None,
        };
        let err = create_symlink_atomic(&entry, &dest, "/remote/refused.lnk")
            .await
            .unwrap_err();
        assert!(matches!(err, RsyncError::HardRejection(_)));
        assert!(!dest.exists(), "fail closed: nothing on disk");
    }

    /// Scripted no-op upload session: server preamble + the 5 NDX_DONE
    /// markers the generator emits when nothing needs transferring: the
    /// exact inbound shape of a symlink upload (flist-only, no
    /// signature/delta phases; see
    /// `native_driver::tests::driver_upload_symlink_emits_flist_only_and_finishes_noop`).
    fn symlink_upload_session_inbound() -> Vec<u8> {
        use crate::aerorsync::real_wire::{
            encode_server_preamble, MuxHeader, MuxTag, ServerPreamble,
        };
        let mut inbound = encode_server_preamble(&ServerPreamble {
            protocol_version: 31,
            compat_flags: 0x07,
            checksum_algos: "md5".to_string(),
            compression_algos: "none zstd".to_string(),
            checksum_seed: 0xDEAD_BEEF,
            consumed: 0,
        });
        let payload = [0x00u8; 5];
        let header = MuxHeader {
            tag: MuxTag::Data,
            length: payload.len() as u32,
        };
        inbound.extend_from_slice(&header.encode());
        inbound.extend_from_slice(&payload);
        inbound
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn do_upload_detects_symlink_and_bypasses_min_file_size() {
        use crate::aerorsync::mock::{MockRemoteShellTransport, MockTransportConfig};

        let dir = fresh_tempdir();
        std::fs::write(dir.path().join("payload.bin"), b"content").unwrap();
        let link = dir.path().join("up.lnk");
        std::os::unix::fs::symlink("payload.bin", &link).unwrap();

        let transport = MockRemoteShellTransport::new(
            MockTransportConfig::healthy_upload()
                .with_raw_inbound(symlink_upload_session_inbound()),
        );
        // A prohibitive threshold proves the symlink path bypasses the
        // TooSmall gate: rerouting a link to the classic SFTP path would
        // materialise the TARGET content instead of the link.
        let result = do_upload(
            transport,
            CancelHandle::inert(),
            &link,
            "/remote/up.lnk",
            10_000_000,
            PreambleProfile::default(),
            None,
            false,
        )
        .await
        .expect("symlink upload must succeed despite the min_file_size gate");
        assert_eq!(
            result.total_size,
            "payload.bin".len() as u64,
            "stats total is the target string length (rsync F_LENGTH)"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn do_upload_rejects_non_utf8_symlink_target_hard() {
        use crate::aerorsync::mock::{MockRemoteShellTransport, MockTransportConfig};
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let dir = fresh_tempdir();
        let link = dir.path().join("bad.lnk");
        std::os::unix::fs::symlink(OsStr::from_bytes(b"\xff\xfe-tgt"), &link).unwrap();

        let transport = MockRemoteShellTransport::new(MockTransportConfig::healthy_upload());
        let err = do_upload(
            transport,
            CancelHandle::inert(),
            &link,
            "/remote/bad.lnk",
            0,
            PreambleProfile::default(),
            None,
            false,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, RsyncError::HardRejection(_)),
            "non-UTF-8 target must fail closed, got {err:?}"
        );
    }

    /// Scripted symlink download session: preamble + flist carrying one
    /// S_IFLNK entry (target string, no flist checksum) + terminator,
    /// then directly the sender finish tail (3 NDX_DONE + SummaryFrame +
    /// trailing marker). No signature echo and no delta stream: the
    /// generator never requests symlinks.
    fn symlink_download_session_inbound(link_name: &str, target: &str) -> Vec<u8> {
        use crate::aerorsync::real_wire::{
            encode_file_list_entry, encode_file_list_terminator, encode_server_preamble,
            encode_summary_frame, FileListDecodeOptions, FileListEntry, MuxHeader, MuxTag,
            ServerPreamble, SummaryFrame,
        };

        fn mux(payload: &[u8]) -> Vec<u8> {
            let header = MuxHeader {
                tag: MuxTag::Data,
                length: payload.len() as u32,
            };
            let mut out = header.encode().to_vec();
            out.extend_from_slice(payload);
            out
        }

        let entry = FileListEntry {
            flags: 1 << 13, // XMIT_MOD_NSEC: explicit first-entry shape
            path: link_name.to_string(),
            size: target.len() as i64,
            mtime: 1_750_000_000,
            mtime_nsec: Some(0),
            mode: 0o120_777,
            uid: Some(1000),
            uid_name: None,
            gid: Some(1000),
            gid_name: None,
            checksum: vec![],
            symlink_target: Some(target.to_string()),
            xattrs: None,
        };
        let opts = FileListDecodeOptions {
            protocol: 31,
            xfer_flags_as_varint: true,
            always_checksum: true,
            csum_len: 16,
            preserve_uid: true,
            preserve_gid: true,
            previous_name: None,
            preserve_xattrs: false,
        };
        let mut finish_tail = vec![0x00; 3];
        finish_tail.extend_from_slice(&encode_summary_frame(
            &SummaryFrame {
                total_read: 100,
                total_written: target.len() as i64,
                total_size: target.len() as i64,
                flist_buildtime: Some(1),
                flist_xfertime: Some(0),
            },
            31,
        ));

        let mut inbound = encode_server_preamble(&ServerPreamble {
            protocol_version: 31,
            compat_flags: 0x07,
            checksum_algos: "md5".to_string(),
            compression_algos: "none zstd".to_string(),
            checksum_seed: 0xDEAD_BEEF,
            consumed: 0,
        });
        inbound.extend_from_slice(&mux(&encode_file_list_entry(&entry, &opts)));
        inbound.extend_from_slice(&mux(&encode_file_list_terminator(&opts)));
        inbound.extend_from_slice(&mux(&finish_tail));
        inbound.extend_from_slice(&mux(&[0x00]));
        inbound
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn do_download_creates_symlink_end_to_end() {
        use crate::aerorsync::mock::{MockRemoteShellTransport, MockTransportConfig};

        let dir = fresh_tempdir();
        let local = dir.path().join("down.lnk");
        // Safe-links (audit S1): a representative in-directory target. An
        // escaping target (`../...`) is covered by
        // `create_symlink_atomic_refuses_unsafe_target_fail_closed`.
        let target = "rel/tgt.bin";
        let transport = MockRemoteShellTransport::new(
            MockTransportConfig::healthy_upload()
                .with_raw_inbound(symlink_download_session_inbound("down.lnk", target)),
        );
        let stats = do_download(
            transport,
            CancelHandle::inert(),
            "/remote/down.lnk",
            &local,
            PreambleProfile::default(),
            None,
            false,
            false,
        )
        .await
        .expect("symlink download must create the link");
        assert_eq!(stats.total_size, target.len() as u64);

        let meta = std::fs::symlink_metadata(&local).unwrap();
        assert!(meta.file_type().is_symlink(), "local must be a symlink");
        assert_eq!(std::fs::read_link(&local).unwrap(), PathBuf::from(target));
        let temp = {
            let mut os = local.as_os_str().to_os_string();
            os.push(TEMP_SUFFIX);
            PathBuf::from(os)
        };
        assert!(!temp.exists(), "streaming + symlink temps must be gone");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn do_download_symlink_replaces_existing_regular_file() {
        use crate::aerorsync::mock::{MockRemoteShellTransport, MockTransportConfig};

        let dir = fresh_tempdir();
        let local = dir.path().join("was-file.lnk");
        std::fs::write(&local, b"stale regular baseline").unwrap();
        let target = "tgt-after.bin";
        let transport = MockRemoteShellTransport::new(
            MockTransportConfig::healthy_upload()
                .with_raw_inbound(symlink_download_session_inbound("was-file.lnk", target)),
        );
        do_download(
            transport,
            CancelHandle::inert(),
            "/remote/was-file.lnk",
            &local,
            PreambleProfile::default(),
            None,
            false,
            false,
        )
        .await
        .expect("symlink download over an existing regular file");

        let meta = std::fs::symlink_metadata(&local).unwrap();
        assert!(
            meta.file_type().is_symlink(),
            "regular file must be atomically replaced by the link"
        );
        assert_eq!(std::fs::read_link(&local).unwrap(), PathBuf::from(target));
    }

    // -- Y-RSC.4 live lane 3 (stock rsync 3.2.7 over SSH, port 2224) --------
    //
    // Gated like the native_driver live tests: compiled only under
    // `RUSTFLAGS='--cfg ci_lane3'`, skip-graceful when the Docker
    // harness is not reachable. Server-side setup/verification runs over
    // SSH as root with the same capture key (readlink / ln -s).

    #[cfg(all(ci_lane3, unix))]
    fn lane3_key_path() -> Option<PathBuf> {
        let key_path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/aerorsync/capture/keys/id_ed25519");
        if key_path.exists() {
            Some(key_path)
        } else {
            eprintln!("[lane3-symlink] ssh key not found at {key_path:?}: skipping");
            None
        }
    }

    #[cfg(all(ci_lane3, unix))]
    fn lane3_ssh_config(key_path: PathBuf) -> crate::aerorsync::ssh_transport::SshTransportConfig {
        use crate::aerorsync::transport::RemoteExecRequest;
        crate::aerorsync::ssh_transport::SshTransportConfig {
            host: "127.0.0.1".into(),
            port: 2224,
            username: "testuser".into(),
            private_key_path: key_path,
            connect_timeout_ms: 10_000,
            io_timeout_ms: 30_000,
            worker_idle_poll_ms: 250,
            max_frame_size: 1 << 20,
            host_key_policy: SshHostKeyPolicy::AcceptAny,
            probe_request: RemoteExecRequest {
                program: "rsync".into(),
                args: vec!["--version".into()],
                environment: Vec::new(),
            },
            auth_password: None,
            auth_agent: false,
        }
    }

    /// Run a shell command on the harness as root over SSH and return
    /// trimmed stdout. Panics on a non-zero exit so a broken harness
    /// surfaces loudly instead of producing vacuous assertions.
    #[cfg(all(ci_lane3, unix))]
    fn lane3_ssh_testuser(key_path: &Path, command: &str) -> String {
        let out = std::process::Command::new("ssh")
            .args([
                "-p",
                "2224",
                "-i",
                key_path.to_str().expect("key path is UTF-8"),
                "-o",
                "StrictHostKeyChecking=no",
                "-o",
                "UserKnownHostsFile=/dev/null",
                "-o",
                "BatchMode=yes",
                "testuser@127.0.0.1",
                command,
            ])
            .output()
            .expect("spawn ssh for harness verification");
        assert!(
            out.status.success(),
            "harness ssh command failed ({}): {}",
            command,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// (a) Upload a local symlink with a relative target through the
    /// production `do_upload` path (symlink_metadata detection included)
    /// and verify server-side over SSH that `readlink` returns the
    /// identical target and the file type is a symlink.
    #[cfg(all(ci_lane3, unix))]
    #[tokio::test]
    async fn delta_upload_symlink_live_lane_3_readlink_identical() {
        use crate::aerorsync::ssh_transport::SshRemoteShellTransport;

        if tokio::net::TcpStream::connect("127.0.0.1:2224")
            .await
            .is_err()
        {
            eprintln!("[lane3-symlink] harness not reachable on 127.0.0.1:2224: skipping");
            return;
        }
        let Some(key_path) = lane3_key_path() else {
            return;
        };

        let dir = fresh_tempdir();
        let relative_target = "lane3-up-target.bin";
        std::fs::write(dir.path().join(relative_target), b"lane3 payload").unwrap();
        let link = dir.path().join("lane3-up.lnk");
        std::os::unix::fs::symlink(relative_target, &link).unwrap();

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let remote_path = format!("/workspace/lane3-symlink-up-{nanos}.lnk");

        let transport = SshRemoteShellTransport::new(lane3_ssh_config(key_path.clone()));
        let stats = do_upload(
            transport,
            CancelHandle::inert(),
            &link,
            &remote_path,
            1_000_000, // prohibitive threshold: symlinks must bypass TooSmall
            PreambleProfile::for_host("127.0.0.1"),
            None,
            false,
        )
        .await
        .expect("live symlink upload against stock rsync");
        assert_eq!(stats.total_size, relative_target.len() as u64);

        let observed = lane3_ssh_testuser(&key_path, &format!("readlink '{remote_path}'"));
        eprintln!("[lane3-symlink] server readlink: {observed}");
        assert_eq!(
            observed, relative_target,
            "server-side readlink must be identical to the uploaded target"
        );
        let ftype = lane3_ssh_testuser(
            &key_path,
            &format!("test -L '{remote_path}' && echo symlink || echo other"),
        );
        assert_eq!(ftype, "symlink", "server-side file type must be a symlink");
    }

    /// (b) Create a symlink server-side over SSH, download it through
    /// the production `do_download` path, and verify the local link is
    /// readlink-identical.
    #[cfg(all(ci_lane3, unix))]
    #[tokio::test]
    async fn delta_download_symlink_live_lane_3_readlink_identical() {
        use crate::aerorsync::ssh_transport::SshRemoteShellTransport;

        if tokio::net::TcpStream::connect("127.0.0.1:2224")
            .await
            .is_err()
        {
            eprintln!("[lane3-symlink] harness not reachable on 127.0.0.1:2224: skipping");
            return;
        }
        let Some(key_path) = lane3_key_path() else {
            return;
        };

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let relative_target = format!("lane3-dl-target-{nanos}.bin");
        let remote_link = format!("/workspace/lane3-symlink-dl-{nanos}.lnk");
        lane3_ssh_testuser(
            &key_path,
            &format!("ln -sfn '{relative_target}' '{remote_link}' && readlink '{remote_link}'"),
        );

        let dir = fresh_tempdir();
        let local = dir.path().join("lane3-dl.lnk");
        let transport = SshRemoteShellTransport::new(lane3_ssh_config(key_path.clone()));
        let stats = do_download(
            transport,
            CancelHandle::inert(),
            &remote_link,
            &local,
            PreambleProfile::for_host("127.0.0.1"),
            None,
            false,
            false,
        )
        .await
        .expect("live symlink download against stock rsync");
        assert_eq!(stats.total_size, relative_target.len() as u64);

        let meta = std::fs::symlink_metadata(&local).expect("local link created");
        assert!(meta.file_type().is_symlink(), "local must be a symlink");
        let observed = std::fs::read_link(&local).expect("read_link");
        eprintln!(
            "[lane3-symlink] local readlink: {}",
            observed.to_string_lossy()
        );
        assert_eq!(observed, PathBuf::from(&relative_target));
        let temp = {
            let mut os = local.as_os_str().to_os_string();
            os.push(TEMP_SUFFIX);
            PathBuf::from(os)
        };
        assert!(!temp.exists(), "no temp leftovers after live download");
    }

    // -- B4 / X.6 live lane 3 xattr acceptance (stock rsync 3.2.7, -X) ------
    //
    // Production still defaults preserve_xattrs=false; these tests opt in
    // via AerorsyncDeltaTransport::with_xattrs(true) only. Remote verify
    // uses python3+ctypes (attr-utils CLI is absent on host and container).

    /// Apply a single `user.*` xattr on a local path via the public apply
    /// path (same libc setxattr the production download uses).
    #[cfg(all(ci_lane3, unix))]
    fn lane3_local_set_user_xattr(path: &Path, name: &str, value: &[u8]) {
        use crate::aerorsync::real_wire::XattrPair;
        use crate::aerorsync::xattr_fs::{apply_xattrs, XattrApplyOutcome};
        match apply_xattrs(path, &[XattrPair::inline(name, value.to_vec())], true) {
            XattrApplyOutcome::Applied { count } => {
                assert!(count >= 1, "setxattr wrote zero pairs for {name}");
            }
            other => panic!("local setxattr for {name} failed: {other:?}"),
        }
    }

    /// Read one remote xattr as raw bytes (None if missing / ENODATA).
    /// Paths and names are under test control (no shell metacharacters).
    #[cfg(all(ci_lane3, unix))]
    fn lane3_remote_get_xattr(key_path: &Path, remote_path: &str, name: &str) -> Option<Vec<u8>> {
        lane3_remote_get_xattr_maybe_nofollow(key_path, remote_path, name, false)
    }

    /// Same probe, but reading the **link itself** (`lgetxattr`) instead of
    /// its target. R2 needs this: asking `getxattr` about an uploaded symlink
    /// resolves the target, and when that target does not exist remotely the
    /// call returns ENOENT — which would make "the link carries no attribute"
    /// pass for the wrong reason. See
    /// `delta_upload_symlink_xattr_not_inherited_live_lane_3`, which asserts
    /// both probes to keep that distinction observable.
    #[cfg(all(ci_lane3, unix))]
    fn lane3_remote_lget_xattr(key_path: &Path, remote_path: &str, name: &str) -> Option<Vec<u8>> {
        lane3_remote_get_xattr_maybe_nofollow(key_path, remote_path, name, true)
    }

    #[cfg(all(ci_lane3, unix))]
    fn lane3_remote_get_xattr_maybe_nofollow(
        key_path: &Path,
        remote_path: &str,
        name: &str,
        nofollow: bool,
    ) -> Option<Vec<u8>> {
        let func = if nofollow { "lgetxattr" } else { "getxattr" };
        let cmd = format!(
            r#"python3 - <<'PY'
import ctypes, base64, sys
L = ctypes.CDLL(None, use_errno=True)
F = L.{func}
F.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_void_p, ctypes.c_size_t]
F.restype = ctypes.c_ssize_t
p = b"{remote_path}"
n = b"{name}"
s = F(p, n, None, 0)
if s < 0:
    print("MISSING")
    sys.exit(0)
b = ctypes.create_string_buffer(s)
g = F(p, n, b, s)
print(base64.b64encode(b.raw[:g]).decode())
PY"#
        );
        let out = lane3_ssh_testuser(key_path, &cmd);
        if out == "MISSING" || out.is_empty() {
            None
        } else {
            use base64::Engine as _;
            Some(
                base64::engine::general_purpose::STANDARD
                    .decode(out.trim())
                    .expect("remote getxattr base64"),
            )
        }
    }

    /// Seed a remote regular file with a `user.*` xattr (value as base64).
    #[cfg(all(ci_lane3, unix))]
    fn lane3_remote_set_xattr(key_path: &Path, remote_path: &str, name: &str, value: &[u8]) {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(value);
        let cmd = format!(
            r#"python3 - <<'PY'
import ctypes, base64, sys
L = ctypes.CDLL(None, use_errno=True)
L.setxattr.argtypes = [
    ctypes.c_char_p, ctypes.c_char_p, ctypes.c_void_p, ctypes.c_size_t, ctypes.c_int
]
L.setxattr.restype = ctypes.c_int
p = b"{remote_path}"
n = b"{name}"
v = base64.b64decode("{b64}")
rc = L.setxattr(p, n, v, len(v), 0)
if rc != 0:
    raise SystemExit("setxattr failed rc=%s errno=%s" % (rc, ctypes.get_errno()))
print("ok")
PY"#
        );
        let out = lane3_ssh_testuser(key_path, &cmd);
        assert_eq!(out, "ok", "remote setxattr for {name}");
    }

    #[cfg(all(ci_lane3, unix))]
    fn lane3_remote_sha256(key_path: &Path, remote_path: &str) -> String {
        lane3_ssh_testuser(
            key_path,
            &format!("sha256sum '{remote_path}' | awk '{{print $1}}'"),
        )
    }

    #[cfg(all(ci_lane3, unix))]
    fn lane3_local_sha256(path: &Path) -> String {
        use sha2::{Digest, Sha256};
        use std::io::Read;
        let mut f = std::fs::File::open(path).expect("open local for sha256");
        let mut hasher = Sha256::new();
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = f.read(&mut buf).expect("read");
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        format!("{:x}", hasher.finalize())
    }

    /// Shared cold-upload path: local file (+ optional xattrs) through
    /// `AerorsyncDeltaTransport::with_xattrs(true)` against stock rsync.
    #[cfg(all(ci_lane3, unix))]
    async fn lane3_xattr_upload_and_verify(
        label: &str,
        payload: &[u8],
        attrs: &[(&str, &[u8])],
        expect_absent: &[&str],
    ) {
        if tokio::net::TcpStream::connect("127.0.0.1:2224")
            .await
            .is_err()
        {
            eprintln!("[lane3-xattr] harness not reachable on 127.0.0.1:2224: skipping");
            return;
        }
        let Some(key_path) = lane3_key_path() else {
            return;
        };

        let dir = fresh_tempdir();
        let local = dir.path().join(format!("{label}.bin"));
        std::fs::write(&local, payload).expect("write local payload");
        for (name, value) in attrs {
            lane3_local_set_user_xattr(&local, name, value);
        }

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let remote_path = format!("/workspace/lane3-xattr-{label}-{nanos}.bin");

        let transport =
            AerorsyncDeltaTransport::new(lane3_ssh_config(key_path.clone()), 0).with_xattrs(true);
        let stats = transport
            .upload(&local, &remote_path)
            .await
            .unwrap_or_else(|e| panic!("[{label}] live xattr upload failed: {e:?}"));
        assert_eq!(
            stats.total_size,
            payload.len() as u64,
            "[{label}] transferred size"
        );

        let local_hash = lane3_local_sha256(&local);
        let remote_hash = lane3_remote_sha256(&key_path, &remote_path);
        assert_eq!(
            remote_hash, local_hash,
            "[{label}] content sha256 must match after xattr session"
        );

        for (name, value) in attrs {
            let got = lane3_remote_get_xattr(&key_path, &remote_path, name);
            assert_eq!(
                got.as_deref(),
                Some(*value),
                "[{label}] remote xattr {name} must match local value"
            );
            eprintln!(
                "[lane3-xattr] {label}: remote {name} ok ({} bytes)",
                value.len()
            );
        }
        for name in expect_absent {
            let got = lane3_remote_get_xattr(&key_path, &remote_path, name);
            assert!(
                got.is_none(),
                "[{label}] remote must not invent xattr {name}: got {got:?}"
            );
        }

        // Clean remote artifact so reruns do not fill /workspace.
        let _ = lane3_ssh_testuser(&key_path, &format!("rm -f '{remote_path}'"));
    }

    /// (1) Inline: value ≤ MAX_FULL_DATUM (32 B) rides in the file-list blob.
    #[cfg(all(ci_lane3, unix))]
    #[tokio::test]
    async fn delta_upload_xattr_inline_live_lane_3() {
        let value = b"hello-inline-xattr"; // 18 B ≤ 32
        assert!(value.len() <= crate::aerorsync::real_wire::MAX_FULL_DATUM);
        lane3_xattr_upload_and_verify(
            "inline",
            b"lane3-xattr-inline-payload-v1",
            &[("user.aeroftp.test", value.as_slice())],
            &[],
        )
        .await;
    }

    /// (2) OOB: value > MAX_FULL_DATUM travels as digest + out-of-band section.
    #[cfg(all(ci_lane3, unix))]
    #[tokio::test]
    async fn delta_upload_xattr_oob_live_lane_3() {
        let value = vec![b'O'; crate::aerorsync::real_wire::MAX_FULL_DATUM + 64];
        assert!(value.len() > crate::aerorsync::real_wire::MAX_FULL_DATUM);
        // Small payload (same class as inline). Larger bodies are covered
        // by content-parity lane3 tests; here we pin the OOB xattr path.
        let payload = b"lane3-xattr-oob-payload-small";
        lane3_xattr_upload_and_verify(
            "oob",
            payload,
            &[("user.aeroftp.oob", value.as_slice())],
            &[],
        )
        .await;
    }

    /// (3) Binary value with an interior NUL (must not truncate at C string).
    #[cfg(all(ci_lane3, unix))]
    #[tokio::test]
    async fn delta_upload_xattr_binary_nul_live_lane_3() {
        let value: &[u8] = b"pre\x00mid\xffpost";
        lane3_xattr_upload_and_verify(
            "binul",
            b"lane3-xattr-binary-nul-payload",
            &[("user.aeroftp.binul", value)],
            &[],
        )
        .await;
    }

    /// (4) Session negotiates `-X` but the file carries no user xattrs:
    /// content stays identical and the peer must not desync.
    #[cfg(all(ci_lane3, unix))]
    #[tokio::test]
    async fn delta_upload_xattr_none_with_x_flag_live_lane_3() {
        lane3_xattr_upload_and_verify(
            "noxattr",
            b"lane3-xattr-session-on-but-empty-attrs",
            &[],
            &[
                "user.aeroftp.test",
                "user.aeroftp.oob",
                "user.aeroftp.binul",
            ],
        )
        .await;
    }

    /// (5) Download twin: seed remote xattr over SSH, pull with
    /// `.with_xattrs(true)`, assert local `read_user_xattrs` matches.
    #[cfg(all(ci_lane3, unix))]
    #[tokio::test]
    async fn delta_download_xattr_live_lane_3() {
        use crate::aerorsync::xattr_fs::read_user_xattrs;

        if tokio::net::TcpStream::connect("127.0.0.1:2224")
            .await
            .is_err()
        {
            eprintln!("[lane3-xattr] harness not reachable on 127.0.0.1:2224: skipping");
            return;
        }
        let Some(key_path) = lane3_key_path() else {
            return;
        };

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let remote_path = format!("/workspace/lane3-xattr-dl-{nanos}.bin");
        let payload = b"lane3-xattr-download-payload-v1";
        let attr_name = "user.aeroftp.dl";
        let attr_value: &[u8] = b"from-remote\x00ok";

        // Seed remote file + xattr (content via python, xattr via ctypes).
        use base64::Engine as _;
        let payload_b64 = base64::engine::general_purpose::STANDARD.encode(payload);
        lane3_ssh_testuser(
            &key_path,
            &format!(
                r#"python3 - <<'PY'
import base64
open("{remote_path}", "wb").write(base64.b64decode("{payload_b64}"))
print("seeded")
PY"#
            ),
        );
        lane3_remote_set_xattr(&key_path, &remote_path, attr_name, attr_value);

        let dir = fresh_tempdir();
        let local = dir.path().join("lane3-xattr-dl.bin");
        let transport =
            AerorsyncDeltaTransport::new(lane3_ssh_config(key_path.clone()), 0).with_xattrs(true);
        let stats = transport
            .download(&remote_path, &local)
            .await
            .expect("live xattr download against stock rsync");
        assert_eq!(stats.total_size, payload.len() as u64);

        let got_bytes = std::fs::read(&local).expect("read downloaded file");
        assert_eq!(got_bytes.as_slice(), payload, "download content must match");

        let pairs = read_user_xattrs(&local).expect("read local xattrs after download");
        let found = pairs.iter().find(|p| p.name == attr_name);
        assert!(
            found.is_some(),
            "expected {attr_name} after download, got {pairs:?}"
        );
        assert_eq!(
            found.and_then(|p| p.datum.bytes()),
            Some(attr_value),
            "downloaded xattr value must match remote seed"
        );
        eprintln!(
            "[lane3-xattr] download: local {attr_name} ok ({} bytes)",
            attr_value.len()
        );

        let _ = lane3_ssh_testuser(&key_path, &format!("rm -f '{remote_path}'"));
    }

    /// (6) R3 acceptance: the **batch** path preserves xattrs against a real
    /// rsync, observed rather than argued.
    ///
    /// #484 pinned that `begin_batch` inherits the transport's flags, but its
    /// test stops at the flag: the four call sites want a live peer and
    /// `AerorsyncBatch` holds a concrete `RusshSessionTransport`, so no mock
    /// reaches them. Until this test existed, "the batch preserves xattrs" was
    /// true by construction only.
    ///
    /// Two files on one session, because reuse is the whole point of a batch:
    /// `session_count == 1` is asserted, so a silent degradation to
    /// one-handshake-per-file would fail here instead of passing quietly. The
    /// first file carries an inline value and the second an out-of-band one,
    /// so both xattr encodings cross the batch leg.
    ///
    /// Note this is also the first live exercise of the russh leg on lane 3:
    /// the single-file tests above run on libssh2 (`prefers_russh_leg()` is
    /// false for a pubkey-file profile) while `begin_batch` always connects
    /// through russh.
    #[cfg(all(ci_lane3, unix))]
    #[tokio::test]
    async fn delta_upload_xattr_batch_two_files_live_lane_3() {
        if tokio::net::TcpStream::connect("127.0.0.1:2224")
            .await
            .is_err()
        {
            eprintln!("[lane3-xattr-batch] harness not reachable on 127.0.0.1:2224: skipping");
            return;
        }
        let Some(key_path) = lane3_key_path() else {
            return;
        };

        let inline_value: &[u8] = b"batch-inline-xattr"; // 18 B ≤ MAX_FULL_DATUM
        assert!(inline_value.len() <= crate::aerorsync::real_wire::MAX_FULL_DATUM);
        let oob_value = vec![b'B'; crate::aerorsync::real_wire::MAX_FULL_DATUM + 64];
        assert!(oob_value.len() > crate::aerorsync::real_wire::MAX_FULL_DATUM);
        const INLINE_NAME: &str = "user.aeroftp.batch_inline";
        const OOB_NAME: &str = "user.aeroftp.batch_oob";
        const NEVER_SET: &str = "user.aeroftp.batch_never_set";

        let dir = fresh_tempdir();
        let local_a = dir.path().join("batch-a.bin");
        let local_b = dir.path().join("batch-b.bin");
        std::fs::write(&local_a, b"lane3-batch-payload-a-v1").expect("write payload a");
        std::fs::write(&local_b, b"lane3-batch-payload-b-v1-longer").expect("write payload b");
        lane3_local_set_user_xattr(&local_a, INLINE_NAME, inline_value);
        lane3_local_set_user_xattr(&local_b, OOB_NAME, &oob_value);

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let remote_a = format!("/workspace/lane3-xattr-batch-a-{nanos}.bin");
        let remote_b = format!("/workspace/lane3-xattr-batch-b-{nanos}.bin");

        let transport =
            AerorsyncDeltaTransport::new(lane3_ssh_config(key_path.clone()), 0).with_xattrs(true);
        let mut batch = transport
            .begin_batch()
            .await
            .expect("begin_batch must not error on a reachable harness");
        // begin_batch degrades to NoopBatch when the russh connect fails, and
        // NoopBatch::upload would then fail with a message about session reuse
        // that says nothing about xattrs. Name the real cause here instead.
        assert!(
            !batch.is_noop(),
            "batch degraded to NoopBatch: russh could not connect to the lane 3 harness, \
             so the batch xattr path was never exercised"
        );

        let stats_a = batch
            .upload(&local_a, &remote_a)
            .await
            .expect("batch upload of file a");
        assert_eq!(
            stats_a.total_size,
            std::fs::metadata(&local_a).unwrap().len(),
            "[batch-a] transferred size"
        );
        let stats_b = batch
            .upload(&local_b, &remote_b)
            .await
            .expect("batch upload of file b");
        assert_eq!(
            stats_b.total_size,
            std::fs::metadata(&local_b).unwrap().len(),
            "[batch-b] transferred size"
        );

        let batch_stats = batch.finalize().await.expect("batch finalize");
        assert_eq!(
            batch_stats.files_transferred, 2,
            "both files must be counted by the batch"
        );
        assert_eq!(
            batch_stats.session_count, 1,
            "two files must ride one SSH handshake: that is what a batch is for"
        );
        assert!(
            !batch_stats.partial,
            "no cancel was issued, the batch must not report partial"
        );

        for (label, local, remote, name, value) in [
            ("batch-a", &local_a, &remote_a, INLINE_NAME, inline_value),
            (
                "batch-b",
                &local_b,
                &remote_b,
                OOB_NAME,
                oob_value.as_slice(),
            ),
        ] {
            let local_hash = lane3_local_sha256(local);
            let remote_hash = lane3_remote_sha256(&key_path, remote);
            assert_eq!(
                remote_hash, local_hash,
                "[{label}] content sha256 must match after a batch xattr session"
            );
            let got = lane3_remote_get_xattr(&key_path, remote, name);
            assert_eq!(
                got.as_deref(),
                Some(value),
                "[{label}] remote xattr {name} must match the local value"
            );
            let absent = lane3_remote_get_xattr(&key_path, remote, NEVER_SET);
            assert!(
                absent.is_none(),
                "[{label}] remote must not invent {NEVER_SET}: got {absent:?}"
            );
            eprintln!(
                "[lane3-xattr-batch] {label}: remote {name} ok ({} bytes), sha256 parity ok",
                value.len()
            );
        }

        // Cross-check: the batch must not smear one file's attribute onto the
        // other. A single shared session is exactly where that could happen.
        assert!(
            lane3_remote_get_xattr(&key_path, &remote_a, OOB_NAME).is_none(),
            "file a must not carry file b's attribute"
        );
        assert!(
            lane3_remote_get_xattr(&key_path, &remote_b, INLINE_NAME).is_none(),
            "file b must not carry file a's attribute"
        );

        let _ = lane3_ssh_testuser(&key_path, &format!("rm -f '{remote_a}' '{remote_b}'"));
    }

    /// (7) R2 acceptance: uploading a symlink must not ship the **target's**
    /// attributes labelled as the link's, against a real rsync.
    ///
    /// #482 fixed the read side (`llistxattr`/`lgetxattr`) and pinned it with a
    /// unit test on the local wrappers. What was never observed is the wire
    /// consequence: that stock rsync ends up with a bare link. On Linux the
    /// kernel forbids `user.*` on a symlink, so the pre-fix behaviour could
    /// also make the receiver answer EPERM — this test therefore asserts the
    /// upload *succeeds* as well as what it leaves behind.
    ///
    /// Two remote probes, deliberately: `lgetxattr` on the link must find
    /// nothing (the pin), and `getxattr` through the link must find the
    /// target's attribute (the control). Without the control, a dangling
    /// remote link would make the pin pass for the wrong reason.
    #[cfg(all(ci_lane3, unix))]
    #[tokio::test]
    async fn delta_upload_symlink_xattr_not_inherited_live_lane_3() {
        use crate::aerorsync::xattr_fs::read_user_xattrs;

        if tokio::net::TcpStream::connect("127.0.0.1:2224")
            .await
            .is_err()
        {
            eprintln!("[lane3-xattr-symlink] harness not reachable on 127.0.0.1:2224: skipping");
            return;
        }
        let Some(key_path) = lane3_key_path() else {
            return;
        };

        const ATTR_NAME: &str = "user.aeroftp.symlink_target_attr";
        // Out-of-band, deliberately: above `MAX_FULL_DATUM` the value travels
        // in its own per-file section, and that is what makes this a pin rather
        // than decoration. With an inline value the test cannot fail on Linux
        // whatever the code does, because the kernel forbids `user.*` on a
        // symlink, so the remote link is bare either way. Above the ceiling the
        // pre-R2 read emits a section for an entry the peer is not expecting and
        // the stream desynchronises: measured against the reverted wrappers,
        // this test fails with `expected NDX_DONE (0x00), got 0x01`.
        let local_attr: &[u8] = &[b'L'; crate::aerorsync::real_wire::MAX_FULL_DATUM + 64];
        let remote_attr: &[u8] = b"REMOTE_TARGET_ATTR";

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let relative_target = format!("lane3-xattr-lnk-target-{nanos}.bin");
        let remote_link = format!("/workspace/lane3-xattr-lnk-{nanos}.lnk");
        let remote_target = format!("/workspace/{relative_target}");

        let dir = fresh_tempdir();
        let target = dir.path().join(&relative_target);
        std::fs::write(&target, b"lane3 symlink target payload").expect("write target");
        lane3_local_set_user_xattr(&target, ATTR_NAME, local_attr);
        let link = dir.path().join("lane3-xattr-up.lnk");
        std::os::unix::fs::symlink(&relative_target, &link).expect("create symlink");

        // Control on the source side: if this setxattr had silently been lost,
        // "the link carries nothing" would be true for a reason that has
        // nothing to do with R2.
        let target_pairs = read_user_xattrs(&target).expect("read target xattrs");
        assert!(
            target_pairs
                .iter()
                .any(|p| p.name == ATTR_NAME && p.datum.bytes() == Some(local_attr)),
            "local target must actually carry {ATTR_NAME}, got {target_pairs:?}"
        );

        let transport = AerorsyncDeltaTransport::new(
            lane3_ssh_config(key_path.clone()),
            1_000_000, // prohibitive threshold: a symlink must bypass TooSmall
        )
        .with_xattrs(true);
        let stats = transport
            .upload(&link, &remote_link)
            .await
            .expect("live symlink upload with -X negotiated must not fail (pre-R2 risk: EPERM)");
        assert_eq!(stats.total_size, relative_target.len() as u64);

        let ftype = lane3_ssh_testuser(
            &key_path,
            &format!("test -L '{remote_link}' && echo symlink || echo other"),
        );
        assert_eq!(ftype, "symlink", "server-side file type must be a symlink");
        let observed = lane3_ssh_testuser(&key_path, &format!("readlink '{remote_link}'"));
        assert_eq!(
            observed, relative_target,
            "server-side readlink must be identical to the uploaded target"
        );

        // The pin: the link itself carries no user attribute.
        let on_link = lane3_remote_lget_xattr(&key_path, &remote_link, ATTR_NAME);
        assert!(
            on_link.is_none(),
            "remote link must not carry the target's attribute: got {on_link:?}"
        );

        // The control: materialise the target remotely with a distinguishable
        // value, then read *through* the link. Finding it proves the probe
        // works and that the assertion above measured the link, not ENOENT.
        lane3_ssh_testuser(&key_path, &format!("printf 'x' > '{remote_target}'"));
        lane3_remote_set_xattr(&key_path, &remote_target, ATTR_NAME, remote_attr);
        let through_link = lane3_remote_get_xattr(&key_path, &remote_link, ATTR_NAME);
        assert_eq!(
            through_link.as_deref(),
            Some(remote_attr),
            "control probe: reading through the link must reach the remote target"
        );
        let still_absent = lane3_remote_lget_xattr(&key_path, &remote_link, ATTR_NAME);
        assert!(
            still_absent.is_none(),
            "the link must still carry nothing of its own: got {still_absent:?}"
        );
        eprintln!("[lane3-xattr-symlink] link bare, target reachable through it: R2 holds on wire");

        let _ = lane3_ssh_testuser(
            &key_path,
            &format!("rm -f '{remote_link}' '{remote_target}'"),
        );
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
        // 100ns-aligned nanoseconds: NTFS stores time as 100ns FILETIME ticks, so a value
        // that is not a multiple of 100 (e.g. 987_654_321) is floored on Windows and the
        // exact round-trip assertion below would be platform-dependent. A 100ns-aligned
        // value round-trips exactly on NTFS (100ns) and ext4 (1ns) alike, so this still
        // verifies mtime preservation without asserting unportable sub-100ns precision.
        let remote_mtime = (1_700_000_123_i64, Some(987_654_300_i32));

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

    /// R3: a batch must carry the transport's xattr negotiation. Before this
    /// the four `do_upload` / `do_download` call sites inside
    /// `impl DeltaBatch for AerorsyncBatch` passed `false` literals, so a
    /// caller that built the transport with `.with_xattrs(true)` and then
    /// used the **batch** path lost the attributes with no warning, which is
    /// exactly the silent metadata loss `fail_on_metadata_loss` exists to
    /// make audible.
    ///
    /// This pins the inheritance, which is all a unit test can reach here:
    /// the call sites themselves need a live peer. They now read `self`
    /// with no boolean literal left between the transport and the wire.
    #[test]
    fn batch_inherits_the_transport_xattr_policy() {
        let cfg = crate::aerorsync::russh_session_transport::test_dummy_config();
        let base = AerorsyncDeltaTransport::new(cfg.clone(), 1);
        assert_eq!(
            base.xattr_policy(),
            (false, false),
            "default must stay off: frozen wire oracles depend on it"
        );

        let opted_in = AerorsyncDeltaTransport::new(cfg.clone(), 1).with_xattrs(true);
        assert_eq!(opted_in.xattr_policy(), (true, false));

        let hard = AerorsyncDeltaTransport::new(cfg, 1)
            .with_xattrs(true)
            .with_fail_on_metadata_loss(true);
        assert_eq!(hard.xattr_policy(), (true, true));

        let (preserve_xattrs, fail_on_metadata_loss) = hard.xattr_policy();
        let transport = RusshSessionTransport::test_with_empty_handle(
            crate::aerorsync::russh_session_transport::test_dummy_config(),
            1,
        );
        let batch = AerorsyncBatch::new(transport, 1, preserve_xattrs, fail_on_metadata_loss);
        assert!(batch.preserve_xattrs, "batch dropped -X on the floor");
        assert!(
            batch.fail_on_metadata_loss,
            "batch dropped fail_on_metadata_loss on the floor"
        );
    }

    #[tokio::test]
    async fn aerorsync_batch_reuses_ssh_session() {
        let cfg = crate::aerorsync::russh_session_transport::test_dummy_config();
        let transport = RusshSessionTransport::test_with_empty_handle(cfg, 1);
        let mut batch = AerorsyncBatch::new(transport, 1, false, false);
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
        let mut batch = AerorsyncBatch::new(transport, 1, false, false);
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
        let batch = AerorsyncBatch::new(transport, 1, false, false);

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
        let batch = AerorsyncBatch::new(transport, 1, false, false);
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
        assert!(
            temp.exists(),
            "writer must create the deterministic .aerotmp"
        );

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
