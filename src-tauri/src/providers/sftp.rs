//! SFTP Provider Implementation
//!
//! This module provides SFTP (SSH File Transfer Protocol) support using the russh crate.
//! Supports both password and SSH key-based authentication.
//!
//! Status: v1.3.0

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use super::types::is_session_closed_error_message;
use super::{
    ProviderError, ProviderTransferExecutorKind, ProviderType, RemoteEntry, SftpConfig,
    StorageProvider,
};
use crate::ssh_exec::ssh_exec_collect;
use async_trait::async_trait;
use russh::client::AuthResult;
use russh::client::{self, Config, Handle, Handler};
use russh::keys::{self, known_hosts, Algorithm, HashAlg, PrivateKeyWithHashAlg, PublicKey};
use russh::{compression, Preferred};
use russh_sftp::client::SftpSession;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio::sync::Mutex as TokioMutex;
use tokio_util::sync::CancellationToken;

use super::multi_thread::{
    aerotmp_path_for, run_concurrent_range_download, ConcurrentRangeConfig, ConcurrentRangeOutcome,
};

/// Hard cap on intra-file SFTP range streams (PD-SFTP-2), mirroring the S3
/// `MULTI_THREAD_MAX_STREAMS`. Each stream is a full independent SSH
/// connection from the pool, so the cap stays conservative; the live
/// benchmark in master 9.6.2 says where it pays.
const SFTP_MULTI_THREAD_MAX_STREAMS: usize = 16;

/// Default intra-file cutoff: below this a single SFTP stream is faster
/// than paying N SSH handshakes. Matches the S3 default (250 MiB) so the
/// `--multi-thread-cutoff` CLI flag behaves identically across backends.
const SFTP_MULTI_THREAD_CUTOFF_DEFAULT: u64 = 250 * 1024 * 1024;

/// Map a russh / russh-sftp / io error onto a [`ProviderError`].
///
/// The russh family does not type-tag transport-level failures (broken
/// pipe, channel torn down, EOF after server idle reaper); they all
/// surface as opaque `Display` strings nested inside the operation
/// error. We string-match those patterns and route them into
/// [`ProviderError::ConnectionLost`] so the command layer can attempt
/// a silent reconnect+replay. Anything else falls through to the
/// caller-supplied fallback variant (NotFound / TransferFailed /
/// ServerError / ...) preserving the previous behavior.
fn classify_russh_err(
    e: impl std::fmt::Display,
    fallback: impl FnOnce(String) -> ProviderError,
) -> ProviderError {
    let s = e.to_string();
    if is_session_closed_error_message(&s) {
        ProviderError::ConnectionLost(s)
    } else {
        fallback(s)
    }
}

/// Shared, lock-protected handle to the underlying russh SSH session.
/// Used by sibling modules (e.g. rsync-over-SSH) to open additional channels
/// (exec, direct-tcpip) without re-authenticating.
pub type SharedSshHandle = Arc<TokioMutex<Handle<SshHandler>>>;

/// POSIX single-quote a string for safe interpolation into a remote shell
/// command. The whole value is wrapped in `'...'` (everything literal inside
/// single quotes) and every embedded `'` is emitted as `'\''` (close quote,
/// escaped literal quote, reopen quote). This neutralises `$()`, backticks,
/// `;`, `&&`, newlines and spaces: there is no shell metacharacter that
/// survives single-quoting. Used by [`SftpProvider::checksum`] before passing
/// a listing-derived path to `sha256sum` over an exec channel.
fn shell_single_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// SSH Client Handler for server key verification.
///
/// Exposed as `pub` because [`SharedSshHandle`] (a public type alias in the same
/// module) names it, and clippy's `exported_private_dependencies` lint requires the
/// visibility levels to match. Callers outside this module don't construct or
/// manipulate it: they only hold the handle and pass it back through APIs that
/// expect `SharedSshHandle`.
pub struct SshHandler {
    /// The host being connected to (for known_hosts lookup)
    host: String,
    /// The port being connected to
    port: u16,
    /// CLI mode: auto-accept unknown hosts and save to known_hosts
    trust_unknown_hosts: bool,
    /// Shared slot populated on successful verification with the
    /// SHA-256 hex fingerprint (lowercase, colon-free) of the server
    /// host key's SSH-wire-encoded bytes. The native rsync path
    /// (`providers::sftp::delta_transport`) consumes this to pin its
    /// second SSH connection: U-02 closes the MITM hole that
    /// `SshHostKeyPolicy::AcceptAny` left open on the native leg.
    host_key_sha256_hex: Arc<std::sync::OnceLock<String>>,
}

impl SshHandler {
    fn with_trust_and_slot(
        host: &str,
        port: u16,
        trust: bool,
        slot: Arc<std::sync::OnceLock<String>>,
    ) -> Self {
        Self {
            host: host.to_string(),
            port,
            trust_unknown_hosts: trust,
            host_key_sha256_hex: slot,
        }
    }

    /// Compute the SHA-256 hex digest of the SSH-wire-encoded public
    /// key bytes, matching the layout that libssh2's
    /// `session.host_key()` returns on the other side of the native
    /// rsync connection. Returns `None` if the russh key encoding fails
    ///: in that case the native path will refuse to enable because
    /// the slot stays empty (secure default).
    fn compute_host_key_fingerprint_hex(key: &PublicKey) -> Option<String> {
        use sha2::{Digest, Sha256};
        let wire = key.to_bytes().ok()?;
        let digest = Sha256::digest(&wire);
        let mut hex = String::with_capacity(digest.len() * 2);
        for byte in digest {
            use std::fmt::Write as _;
            let _ = write!(hex, "{byte:02x}");
        }
        Some(hex)
    }
}

impl Handler for SshHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKey,
    ) -> Result<bool, Self::Error> {
        // Use russh's built-in known_hosts verification
        match known_hosts::check_known_hosts(&self.host, self.port, server_public_key) {
            Ok(true) => {
                tracing::info!("SFTP: Host key verified for {}", self.host);
                // U-02 slot populate: native rsync path pins against
                // this fingerprint.
                if let Some(hex) = Self::compute_host_key_fingerprint_hex(server_public_key) {
                    let _ = self.host_key_sha256_hex.set(hex);
                }
                Ok(true)
            }
            Ok(false) => {
                if self.trust_unknown_hosts {
                    // CLI --trust-host-key mode: accept and learn
                    tracing::info!(
                        "SFTP: Auto-accepting host key for {} (--trust-host-key)",
                        self.host
                    );
                    if let Err(e) =
                        known_hosts::learn_known_hosts(&self.host, self.port, server_public_key)
                    {
                        tracing::warn!("SFTP: Failed to save host key to known_hosts: {}", e);
                    }
                    if let Some(hex) = Self::compute_host_key_fingerprint_hex(server_public_key) {
                        let _ = self.host_key_sha256_hex.set(hex);
                    }
                    Ok(true)
                } else {
                    // SEC-P1-06: Host not in known_hosts: reject here.
                    // Frontend must call sftp_check_host_key + sftp_accept_host_key first.
                    tracing::warn!(
                        "SFTP: Host key for {} not pre-approved via TOFU dialog: rejecting",
                        self.host
                    );
                    Ok(false)
                }
            }
            Err(keys::Error::KeyChanged { line }) => {
                tracing::error!(
                    "SFTP: REJECTING connection to {} - host key changed at known_hosts line {} (possible MITM attack)",
                    self.host,
                    line
                );
                Ok(false)
            }
            Err(e) => {
                // SEC: Reject on unknown errors: do not silently accept.
                // Only TOFU (Ok(false)) should auto-accept; other errors may indicate
                // corrupted known_hosts or key format issues.
                tracing::error!(
                    "SFTP: REJECTING connection to {} - known_hosts verification error: {}",
                    self.host,
                    e
                );
                Ok(false)
            }
        }
    }
}

/// Secure connection spec retained after `connect()` so the shared
/// transfer engine can re-dial N **independent** SSH+SFTP connections for
/// file-level parallelism (PD-SFTP-1).
///
/// This mirrors `FtpConnectionSpec` / `FtpManager::connection_spec()`:
/// `provider_connect` zeroizes the outer config password after the first
/// connect, so the provider must retain its own `SecretString` copy.
/// Holding credentials for the provider's lifetime is the exact security
/// posture FTP already ships. Secrets are only exposed (`ExposeSecret`)
/// at dial time, never on IPC or in logs.
#[derive(Clone)]
pub struct SftpConnectionSpec {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: Option<secrecy::SecretString>,
    pub private_key_path: Option<String>,
    pub key_passphrase: Option<secrecy::SecretString>,
    pub initial_path: Option<String>,
    pub timeout_secs: u64,
    /// SHA-256 hex of the host key accepted by the first connect. Pool
    /// re-dials verify the new connection's key against this (defense in
    /// depth on top of `known_hosts`), same posture as the U-02 rsync pin.
    pub pinned_host_key_sha256: Option<String>,
}

impl SftpConnectionSpec {
    /// Rebuild an `SftpConfig` for an independent worker. `trust_unknown_hosts`
    /// is forced to `false`: a pooled re-dial must never TOFU, the host key
    /// is already in `known_hosts` from the first connect.
    fn to_config(&self) -> SftpConfig {
        SftpConfig {
            host: self.host.clone(),
            port: self.port,
            username: self.username.clone(),
            password: self.password.clone(),
            private_key_path: self.private_key_path.clone(),
            key_passphrase: self.key_passphrase.clone(),
            initial_path: self.initial_path.clone(),
            timeout_secs: self.timeout_secs,
            trust_unknown_hosts: false,
        }
    }
}

/// SFTP Provider
///
/// Provides secure file transfer over SSH using the SFTP protocol.
pub struct SftpProvider {
    config: SftpConfig,
    /// SSH connection handle (shared so rsync-over-SSH can open exec channels on the same session).
    ssh_handle: Option<SharedSshHandle>,
    /// SFTP session for file operations
    sftp: Option<SftpSession>,
    /// Current working directory
    current_dir: String,
    /// Home directory (resolved on connect)
    home_dir: String,
    /// Download speed limit in bytes/sec (0 = unlimited)
    download_limit_bps: u64,
    /// Upload speed limit in bytes/sec (0 = unlimited)
    upload_limit_bps: u64,
    /// SSH compression enabled (zlib@openssh.com)
    compression_enabled: bool,
    /// Buffer size for download/upload (default: 32 KB)
    buffer_size: usize,
    /// Shared slot populated by [`SshHandler`] during `check_server_key`
    /// with the SHA-256 hex fingerprint of the accepted host key. The
    /// native rsync transport reuses this fingerprint to pin its own
    /// SSH connection (U-02) so the fresh TCP socket it opens for
    /// `aerorsync_serve` does not skip host-key verification.
    host_key_sha256_hex: Arc<std::sync::OnceLock<String>>,
    /// Secure connection spec for re-dialling independent pool sessions.
    /// `Some` once captured: either at the end of a successful `connect()`
    /// (before `provider_connect` zeroizes the outer config) or when this
    /// provider was produced by `clone_for_transfer()` as a not-yet-
    /// connected pool worker.
    connection_spec: Option<SftpConnectionSpec>,
    /// Intra-file parallel streams (PD-SFTP-2). `1` (default) disables it:
    /// the single-stream path stays the only behaviour. `>= 2` enables a
    /// chunked range download over N independent SSH connections for files
    /// at/above `multi_thread_cutoff`. Set via `set_multi_thread_download`
    /// (CLI `--multi-thread-streams`).
    multi_thread_streams: usize,
    /// File size at/above which intra-file parallelism engages.
    multi_thread_cutoff: u64,
}

impl SftpProvider {
    pub fn new(config: SftpConfig) -> Self {
        Self {
            config,
            ssh_handle: None,
            sftp: None,
            current_dir: "/".to_string(),
            home_dir: "/".to_string(),
            download_limit_bps: 0,
            upload_limit_bps: 0,
            compression_enabled: false,
            // 256 KiB is the sweet spot for SFTP throughput on modern links:
            // 32 KiB caps loopback at ~35 MB/s, 256 KiB reaches ~65 MB/s,
            // and 1 MiB only adds another ~5 MB/s while wasting RAM. OpenSSH
            // (>=8) and russh-sftp both negotiate packet sizes well above 32K
            // in practice. Override per-call with --chunk-size / --buffer-size.
            buffer_size: 256 * 1024,
            host_key_sha256_hex: Arc::new(std::sync::OnceLock::new()),
            connection_spec: None,
            multi_thread_streams: 1,
            multi_thread_cutoff: SFTP_MULTI_THREAD_CUTOFF_DEFAULT,
        }
    }

    /// Return the SHA-256 hex fingerprint of the host key that
    /// [`SshHandler::check_server_key`] accepted during the current
    /// SFTP session, or `None` before a successful handshake.
    ///
    /// Used by [`SftpProvider::delta_transport`] (U-02) to pin the
    /// native rsync path's independent SSH connection against the same
    /// fingerprint the classic SFTP verification already cleared.
    pub fn accepted_host_key_sha256_hex(&self) -> Option<String> {
        self.host_key_sha256_hex.get().cloned()
    }

    /// Secure connection spec retained after a successful `connect()`.
    /// Mirrors `FtpManager::connection_spec()`. `None` until connected (or
    /// until set by `clone_for_transfer()` on a pool worker).
    pub fn connection_spec(&self) -> Option<SftpConnectionSpec> {
        self.connection_spec.clone()
    }

    /// Ensure this provider has its own independent, authenticated SSH+SFTP
    /// connection (PD-SFTP-1). A `clone_for_transfer()` worker starts
    /// unconnected and carries only the secure spec; the first transfer
    /// dials a **separate** SSH connection (separate TCP socket, separate
    /// auth) so N files run truly in parallel, exactly like the FTP pool.
    ///
    /// Host-key safety on the re-dial: `connect()` still goes through
    /// `SshHandler` -> `known_hosts` with `trust_unknown_hosts = false`
    /// (never TOFU on a pooled dial; the key is already known from the
    /// first connect, `KeyChanged` is rejected). Defense in depth: the
    /// freshly accepted fingerprint is compared against the pin captured
    /// at the first connect and a mismatch aborts the worker.
    async fn ensure_connected(&mut self) -> Result<(), ProviderError> {
        if self.sftp.is_some() {
            return Ok(());
        }
        let spec = self
            .connection_spec
            .clone()
            .ok_or(ProviderError::NotConnected)?;
        self.config = spec.to_config();
        // Fresh per-connection slot so the comparison reflects this dial.
        self.host_key_sha256_hex = Arc::new(std::sync::OnceLock::new());
        self.connect().await?;
        if let Some(pinned) = spec.pinned_host_key_sha256.as_deref() {
            match self.accepted_host_key_sha256_hex().as_deref() {
                Some(seen) if seen == pinned => {}
                other => {
                    let _ = self.disconnect().await;
                    return Err(ProviderError::ConnectionFailed(format!(
                        "SFTP pool re-dial host key mismatch (expected {}, got {:?}): aborting worker",
                        pinned, other
                    )));
                }
            }
        }
        Ok(())
    }

    /// PD-SFTP-2 intra-file download: split a large file into N gap-free
    /// windows, each streamed over its **own independent SSH connection**
    /// (the exact connection model of the file-level pool: spec re-dial with
    /// host-key pin, no shared SSH handle/channel), assembled into a
    /// pre-allocated `.aerotmp` and atomically renamed. Reuses the shared
    /// [`run_concurrent_range_download`] orchestrator (plan / temp / RAII
    /// cleanup / bounded concurrency / progress / cancel) so HTTP and SFTP
    /// share one engine, not a fifth implementation.
    ///
    /// Strict gate (the SFTP equivalent of HTTP `206` + `Content-Range`):
    /// every window must yield exactly `end - start + 1` bytes; a premature
    /// EOF is a hard error, never a silent short read. SFTP has no
    /// `ServerIgnoredRange` analogue (`seek`+`read` cannot ignore a range),
    /// so that orchestrator arm is unreachable here and fails loud if hit.
    ///
    /// Single-session READ pipelining (rclone's `--sftp-concurrency`) is
    /// deliberately **not** implemented: per the rev-3 honesty rule it is an
    /// optional, separately-measured tier, never a closure promise. N
    /// independent connections is the mechanism, exactly like PD-SFTP-1.
    async fn download_intra_file_pooled(
        &self,
        remote_path: &str,
        local_path: &str,
        total_size: u64,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let spec = self
            .connection_spec
            .clone()
            .ok_or(ProviderError::NotConnected)?;
        let streams = self
            .multi_thread_streams
            .clamp(2, SFTP_MULTI_THREAD_MAX_STREAMS);
        let buffer_size = self.buffer_size.max(4096);
        // Split any bandwidth cap across the N connections so the aggregate
        // stays near the user's limit (same intent as the single-stream
        // throttle; no new semantics vs the PD-SFTP-1 file-level pool, which
        // also runs N connections).
        let per_stream_limit_bps = if self.download_limit_bps > 0 {
            (self.download_limit_bps / streams as u64).max(1)
        } else {
            0
        };
        let remote_path_owned = remote_path.to_string();
        let current_dir = self.current_dir.clone();
        let home_dir = self.home_dir.clone();
        let compression_enabled = self.compression_enabled;

        let cfg = ConcurrentRangeConfig {
            final_path: PathBuf::from(local_path),
            provider_type: ProviderType::Sftp,
            total_size,
            streams,
            max_streams: SFTP_MULTI_THREAD_MAX_STREAMS,
            max_parallel: streams,
        };

        tracing::info!(
            "SFTP: intra-file download {} ({} bytes) over {} independent connections",
            remote_path_owned,
            total_size,
            streams
        );

        let write_one_range = move |start: u64,
                                    end: u64,
                                    temp_path: PathBuf,
                                    aggregate: Arc<AtomicU64>,
                                    cancel: CancellationToken| {
            let spec = spec.clone();
            let remote_path = remote_path_owned.clone();
            let current_dir = current_dir.clone();
            let home_dir = home_dir.clone();
            async move {
                sftp_download_one_range(
                    spec,
                    remote_path,
                    current_dir,
                    home_dir,
                    buffer_size,
                    per_stream_limit_bps,
                    compression_enabled,
                    start,
                    end,
                    temp_path,
                    aggregate,
                    cancel,
                )
                .await
            }
        };

        match run_concurrent_range_download(
            cfg,
            write_one_range,
            CancellationToken::new(),
            on_progress,
        )
        .await?
        {
            ConcurrentRangeOutcome::Completed => {
                let temp = aerotmp_path_for(Path::new(local_path));
                tokio::fs::rename(&temp, local_path)
                    .await
                    .map_err(ProviderError::IoError)?;
                tracing::info!("SFTP: intra-file download complete: {}", remote_path);
                Ok(())
            }
            ConcurrentRangeOutcome::ServerIgnoredRange => {
                // Unreachable for SFTP: seek+read cannot "ignore" a range.
                // Never silently re-download (it would double the bytes).
                let _ = tokio::fs::remove_file(aerotmp_path_for(Path::new(local_path))).await;
                Err(ProviderError::TransferFailed(
                    "SFTP intra-file: unexpected range-ignored outcome".to_string(),
                ))
            }
        }
    }

    /// Return a cloneable handle to the underlying SSH session, if connected.
    ///
    /// Exposed to let sibling modules (rsync-over-SSH, port forwarding, ...)
    /// open additional channels on the same authenticated session. The handle
    /// is protected by a Tokio [`Mutex`](TokioMutex): callers should hold the
    /// guard for the minimal time required to send a message, since concurrent
    /// SFTP operations go through the same inner mpsc sender.
    pub fn handle_shared(&self) -> Option<SharedSshHandle> {
        self.ssh_handle.clone()
    }

    /// Build a [`DeltaTransport`](crate::delta_transport::DeltaTransport) ready to
    /// run against this provider's SSH session, or `None` if this provider is not
    /// currently eligible for delta sync.
    ///
    /// Eligibility conditions (all must hold):
    /// - Provider is connected (shared handle present)
    /// - SSH authentication has either a private key path on disk or a
    ///   non-empty password saved in the profile.
    ///
    /// This method is the single choke point where an `SftpProvider` becomes a
    /// `dyn DeltaTransport`. The adapter layer (`delta_sync_rsync`) never reaches
    /// into provider internals, preserving the forward compatibility promise for
    /// the strada C native transport.
    ///
    /// ## Cross-OS (PR-T11)
    ///
    /// - **Unix + any build**: returns `RsyncBinaryTransport` as the classic
    ///   fallback when the native feature is off or refuses.
    /// - **Unix + `aerorsync`**: attempts `AerorsyncDeltaTransport`
    ///   first (if the runtime toggle and host-key pinning allow), otherwise
    ///   falls back to `RsyncBinaryTransport`.
    /// - **Windows + `aerorsync`**: uses the native transport only.
    ///   Without the feature compiled in, this method returns `None` so the
    ///   consumer transparently drops to classic SFTP (same shape the adapter
    ///   already accepts for non-SFTP providers).
    pub fn delta_transport(&self) -> Option<Box<dyn crate::delta_transport::DeltaTransport>> {
        let handle = self.ssh_handle.clone()?;
        let known_hosts_path = dirs::home_dir().map(|h| h.join(".ssh").join("known_hosts"));
        let rsync_config = self.rsync_config_for_delta(known_hosts_path)?;

        #[cfg(feature = "aerorsync")]
        {
            // Runtime toggle - read from settings. When on, attempt
            // AerorsyncDeltaTransport and fall through to classic
            // binary on any construction error.
            //
            // U-02 security gate: the native path opens its own SSH
            // connection (separate TCP socket, separate libssh2 session)
            // and must not weaken the host-key posture of the parent
            // SFTP session. We only enable the native leg when the
            // classic SFTP flow has already captured the accepted host
            // key's SHA-256 fingerprint. Without a fingerprint we refuse
            // to enable native: the fresh SSH connection would otherwise
            // ride `AcceptAny`, which is a MITM window on a second
            // independent socket.
            let native_mode = crate::settings::load_native_rsync_mode();
            if !matches!(native_mode, crate::settings::NativeRsyncMode::Classic) {
                use crate::aerorsync::delta_transport_impl::AerorsyncDeltaTransport;
                use crate::aerorsync::ssh_transport::SshHostKeyPolicy;

                let host_key_policy = match self.accepted_host_key_sha256_hex() {
                    Some(hex) => SshHostKeyPolicy::pinned_hex(hex),
                    None => {
                        tracing::warn!(
                            "providers::sftp: native rsync disabled for this session: parent \
                             SFTP handshake did not capture a host key fingerprint (possible \
                             password-only auth or early error); falling back to classic"
                        );
                        if matches!(native_mode, crate::settings::NativeRsyncMode::Native) {
                            tracing::warn!(
                                "providers::sftp: native-only rsync mode selected; skipping classic binary fallback"
                            );
                            return None;
                        }
                        return classic_binary_fallback(rsync_config, handle);
                    }
                };

                match AerorsyncDeltaTransport::from_rsync_config(&rsync_config, host_key_policy) {
                    Ok(transport) => {
                        tracing::info!(
                            "providers::sftp: using native rsync delta transport (host key pinned)"
                        );
                        return Some(Box::new(transport));
                    }
                    Err(error) => {
                        tracing::warn!(
                            "providers::sftp: native rsync transport construction failed ({error}); falling back to classic"
                        );
                        if matches!(native_mode, crate::settings::NativeRsyncMode::Native) {
                            tracing::warn!(
                                "providers::sftp: native-only rsync mode selected; skipping classic binary fallback"
                            );
                            return None;
                        }
                    }
                }
            }
        }

        classic_binary_fallback(rsync_config, handle)
    }

    fn expand_home_path(path: &str) -> String {
        if let Some(stripped) = path.strip_prefix("~/") {
            if let Some(home) = dirs::home_dir() {
                return home.join(stripped).to_string_lossy().to_string();
            }
        }

        path.to_string()
    }

    fn rsync_config_for_delta(
        &self,
        known_hosts_path: Option<std::path::PathBuf>,
    ) -> Option<crate::rsync_over_ssh::RsyncConfig> {
        use crate::rsync_over_ssh::AuthMethod;
        use secrecy::ExposeSecret;

        let (ssh_key_path, ssh_password, auth_method) =
            if let Some(key_path_str) = self.config.private_key_path.as_ref() {
                (
                    Some(std::path::PathBuf::from(Self::expand_home_path(
                        key_path_str,
                    ))),
                    None,
                    AuthMethod::SshKey,
                )
            } else {
                let password = self
                    .config
                    .password
                    .as_ref()
                    .filter(|secret| !secret.expose_secret().is_empty())?;
                (None, Some(password.clone()), AuthMethod::Password)
            };

        Some(crate::rsync_over_ssh::RsyncConfig {
            compress: true,
            preserve_times: true,
            progress: true,
            min_file_size: crate::rsync_over_ssh::DEFAULT_MIN_FILE_SIZE,
            ssh_key_path,
            ssh_password,
            auth_method,
            ssh_port: Some(self.config.port),
            ssh_user: self.config.username.clone(),
            ssh_host: self.config.host.clone(),
            // Classic SFTP flow already verified the host key via
            // `SshHandler::check_server_key`; rsync's SSH transport can
            // trust that verification for the same session.
            strict_host_key_check: "accept-new".to_string(),
            known_hosts_path,
        })
    }
}

/// PR-T11 cross-OS helper. On Unix this constructs the classic
/// `RsyncBinaryTransport` that drives the system `rsync` binary; on Windows
/// the binary is not available, so we silently return `None` and let the
/// consumer fall through to standard SFTP (identical shape to the
/// "non-SFTP provider" branch already handled upstream).
fn classic_binary_fallback(
    rsync_config: crate::rsync_over_ssh::RsyncConfig,
    handle: SharedSshHandle,
) -> Option<Box<dyn crate::delta_transport::DeltaTransport>> {
    #[cfg(unix)]
    {
        Some(Box::new(crate::delta_transport::RsyncBinaryTransport::new(
            rsync_config,
            Some(handle),
        )))
    }
    #[cfg(not(unix))]
    {
        let _ = (rsync_config, handle);
        tracing::debug!(
            "providers::sftp: no binary rsync on this platform; classic fallback returns None \
             (caller transparently drops to plain SFTP)"
        );
        None
    }
}

impl SftpProvider {
    /// Normalize path (ensure absolute)
    fn normalize_path(&self, path: &str) -> String {
        if path.starts_with('/') {
            path.to_string()
        } else if path.is_empty() || path == "." {
            self.current_dir.clone()
        } else if path == ".." {
            let parent = Path::new(&self.current_dir)
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| "/".to_string());
            if parent.is_empty() {
                "/".to_string()
            } else {
                parent
            }
        } else if path == "~" {
            self.home_dir.clone()
        } else if let Some(stripped) = path.strip_prefix("~/") {
            format!("{}/{}", self.home_dir.trim_end_matches('/'), stripped)
        } else {
            format!("{}/{}", self.current_dir.trim_end_matches('/'), path)
        }
    }

    /// Get SFTP session or error if not connected
    fn get_sftp(&self) -> Result<&SftpSession, ProviderError> {
        self.sftp.as_ref().ok_or(ProviderError::NotConnected)
    }

    /// Get mutable SFTP session or error if not connected
    #[allow(dead_code)]
    fn get_sftp_mut(&mut self) -> Result<&mut SftpSession, ProviderError> {
        self.sftp.as_mut().ok_or(ProviderError::NotConnected)
    }

    /// Convert russh-sftp metadata to RemoteEntry.
    ///
    /// Free of `&self` so `list` can call it from inside the concurrent
    /// per-entry futures without capturing the provider.
    fn metadata_to_entry(
        name: String,
        path: String,
        metadata: &russh_sftp::protocol::FileAttributes,
    ) -> RemoteEntry {
        let is_dir = metadata
            .permissions
            .map(|p| (p & 0o40000) != 0)
            .unwrap_or(false);

        let permissions = metadata.permissions.map(|p| format_permissions(p, is_dir));

        let modified = metadata.mtime.map(|t| {
            chrono::DateTime::from_timestamp(t as i64, 0)
                .map(|dt| dt.format("%Y-%m-%d %H:%M:%SZ").to_string())
                .unwrap_or_default()
        });

        RemoteEntry {
            name,
            path,
            is_dir,
            size: metadata.size.unwrap_or(0),
            modified,
            permissions,
            owner: metadata.uid.map(|u| u.to_string()),
            group: metadata.gid.map(|g| g.to_string()),
            is_symlink: false, // Will be set separately for symlinks
            link_target: None,
            mime_type: None,
            metadata: Default::default(),
        }
    }

    /// Authenticate using SSH private key
    async fn authenticate_with_key(
        &self,
        handle: &mut Handle<SshHandler>,
    ) -> Result<bool, ProviderError> {
        let key_path = self.config.private_key_path.as_ref().ok_or_else(|| {
            ProviderError::AuthenticationFailed("No private key path specified".to_string())
        })?;

        let expanded_path = Self::expand_home_path(key_path);

        tracing::info!("SFTP: Loading private key from {}", expanded_path);

        // Load and parse the key using russh's built-in key loading
        use secrecy::ExposeSecret;
        let passphrase_str = self
            .config
            .key_passphrase
            .as_ref()
            .map(|s| s.expose_secret().to_string());
        let key_pair =
            keys::load_secret_key(&expanded_path, passphrase_str.as_deref()).map_err(|e| {
                ProviderError::AuthenticationFailed(format!("Failed to load key: {}", e))
            })?;

        // A1 finding: RSA keys authenticated with `None` (= ssh-rsa /
        // SHA-1) are rejected by OpenSSH 8.8+ because RSA-SHA1 is
        // disabled by default. We have to negotiate rsa-sha2-512 or
        // rsa-sha2-256 depending on the key type. For non-RSA keys
        // (ed25519, ecdsa) the hash is baked into the algorithm name so
        // `None` is correct and required.
        //
        // Strategy: try SHA-512 first (RFC 8332 preference), fall back
        // to SHA-256 on auth failure, then fall back to no-hash (ssh-rsa
        // SHA-1) for ancient servers that still accept it. Non-RSA
        // keys take the `None` path directly.
        let key_pair = Arc::new(key_pair);
        let is_rsa = matches!(key_pair.algorithm(), Algorithm::Rsa { .. });

        let attempts: Vec<Option<HashAlg>> = if is_rsa {
            vec![Some(HashAlg::Sha512), Some(HashAlg::Sha256), None]
        } else {
            vec![None]
        };

        let mut last_auth_error: Option<String> = None;
        for hash in attempts {
            let key_with_hash = PrivateKeyWithHashAlg::new(key_pair.clone(), hash);
            match handle
                .authenticate_publickey(&self.config.username, key_with_hash)
                .await
            {
                Ok(AuthResult::Success) => return Ok(true),
                Ok(AuthResult::Failure { .. }) => {
                    // Next hash algorithm; OpenSSH returns this for
                    // "publickey accepted but signature algo rejected".
                    continue;
                }
                Err(e) => {
                    last_auth_error = Some(e.to_string());
                    continue;
                }
            }
        }

        if let Some(err) = last_auth_error {
            return Err(ProviderError::AuthenticationFailed(format!(
                "Key authentication failed after RSA SHA-512/256/1 negotiation attempts: {err}"
            )));
        }
        Ok(false)
    }

    async fn verify_remote_upload_size(
        &self,
        sftp: &SftpSession,
        remote_path: &str,
        expected_size: u64,
    ) -> Result<(), ProviderError> {
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(3);
        let mut last_observation = format!("expected {} bytes, got no metadata yet", expected_size);

        loop {
            match sftp.metadata(remote_path).await {
                Ok(metadata) => {
                    let actual_size = metadata.size.unwrap_or(0);
                    if actual_size == expected_size {
                        return Ok(());
                    }
                    last_observation = format!(
                        "expected {} bytes, got {} bytes",
                        expected_size, actual_size
                    );
                }
                Err(error) => {
                    last_observation = error.to_string();
                }
            }

            if tokio::time::Instant::now() >= deadline {
                return Err(ProviderError::TransferFailed(format!(
                    "Upload verification failed for {}: {}",
                    remote_path, last_observation,
                )));
            }

            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        }
    }

    /// Align the remote file's mtime/atime with the local source so repeated
    /// sync scans don't re-upload unchanged files just because the server
    /// stamped the upload time. Best-effort: failures are logged, not fatal.
    /// Shared by `upload` and `resume_upload`.
    async fn preserve_remote_mtime(&self, sftp: &SftpSession, remote_path: &str, local_path: &str) {
        match tokio::fs::metadata(local_path).await {
            Ok(local_meta) => {
                if let Ok(modified) = local_meta.modified() {
                    if let Ok(duration) = modified.duration_since(std::time::UNIX_EPOCH) {
                        match u32::try_from(duration.as_secs()) {
                            Ok(epoch_secs) => {
                                let mut attrs = russh_sftp::protocol::FileAttributes::empty();
                                // SFTP's ACMODTIME attribute serializes both fields together;
                                // reuse the source mtime for atime to avoid sending a zero atime.
                                attrs.atime = Some(epoch_secs);
                                attrs.mtime = Some(epoch_secs);
                                if let Err(error) = sftp.set_metadata(remote_path, attrs).await {
                                    tracing::warn!(
                                        "SFTP: Failed to preserve remote mtime for {}: {}",
                                        remote_path,
                                        error
                                    );
                                }
                            }
                            Err(_) => tracing::warn!(
                                "SFTP: Skipping mtime preservation for {} because source mtime is out of range",
                                remote_path
                            ),
                        }
                    }
                }
            }
            Err(error) => tracing::warn!(
                "SFTP: Could not read local metadata for mtime preservation ({}): {}",
                local_path,
                error
            ),
        }
    }
}

/// How an interrupted upload should be resumed.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ResumeUploadPlan {
    /// Nothing usable on the remote (empty / stale offset): full upload from 0.
    FullUpload,
    /// The remote already holds at least the whole local file: nothing to send.
    AlreadyComplete,
    /// Append the local tail starting at this byte offset.
    Append(u64),
}

/// Decide how to resume an upload from the caller's requested offset, the
/// actual remote size, and the local file size. The offset is clamped to what
/// really landed (`remote_size`) so a stale caller offset can never make us
/// append past a short remote file, which would corrupt it.
pub(crate) fn plan_resume_upload(
    caller_offset: u64,
    remote_size: u64,
    local_size: u64,
) -> ResumeUploadPlan {
    let start = caller_offset.min(remote_size);
    if start == 0 {
        ResumeUploadPlan::FullUpload
    } else if start >= local_size {
        ResumeUploadPlan::AlreadyComplete
    } else {
        ResumeUploadPlan::Append(start)
    }
}

/// File-type mask and the symlink type of a POSIX mode word.
const S_IFMT: u32 = 0o170000;
const S_IFLNK: u32 = 0o120000;

/// Test `S_IFLNK` on a raw SFTP mode word.
///
/// `SSH_FXP_READDIR` replies carry each entry's own attributes with `lstat`
/// semantics, so the symlink bit is already in hand and the listing needs no
/// extra `SSH_FXP_LSTAT` per entry. This is the same assumption rclone's sftp
/// backend makes.
///
/// Returns `None` when the mode carries no file-type bits at all. Some
/// embedded firmware sends permission bits only, and an unknown type must be
/// probed rather than silently read as "not a symlink": `list` would then
/// hand a symlink-to-directory to callers as a real directory, and every
/// recursive walk would follow it (`GAP-A02`).
fn symlink_bit(mode: u32) -> Option<bool> {
    match mode & S_IFMT {
        0 => None,
        file_type => Some(file_type == S_IFLNK),
    }
}

/// Format Unix permissions as rwx string
fn format_permissions(mode: u32, is_dir: bool) -> String {
    let user = format!(
        "{}{}{}",
        if mode & 0o400 != 0 { 'r' } else { '-' },
        if mode & 0o200 != 0 { 'w' } else { '-' },
        if mode & 0o100 != 0 { 'x' } else { '-' }
    );
    let group = format!(
        "{}{}{}",
        if mode & 0o040 != 0 { 'r' } else { '-' },
        if mode & 0o020 != 0 { 'w' } else { '-' },
        if mode & 0o010 != 0 { 'x' } else { '-' }
    );
    let other = format!(
        "{}{}{}",
        if mode & 0o004 != 0 { 'r' } else { '-' },
        if mode & 0o002 != 0 { 'w' } else { '-' },
        if mode & 0o001 != 0 { 'x' } else { '-' }
    );
    format!(
        "{}{}{}{}",
        if is_dir { 'd' } else { '-' },
        user,
        group,
        other
    )
}

#[async_trait]
impl StorageProvider for SftpProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn provider_type(&self) -> ProviderType {
        ProviderType::Sftp
    }

    fn display_name(&self) -> String {
        format!("{}@{}", self.config.username, self.config.host)
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        tracing::info!(
            "SFTP: Connecting to {}:{}",
            self.config.host,
            self.config.port
        );

        // Create SSH config with keepalive to prevent server from closing connection
        let preferred = if self.compression_enabled {
            tracing::info!("SFTP: SSH compression enabled (zlib@openssh.com)");
            Preferred {
                compression: std::borrow::Cow::Borrowed(&[
                    compression::ZLIB_LEGACY,
                    compression::ZLIB,
                    compression::NONE,
                ]),
                ..Default::default()
            }
        } else {
            Preferred::default()
        };
        let config = Config {
            inactivity_timeout: Some(std::time::Duration::from_secs(self.config.timeout_secs * 2)),
            keepalive_interval: Some(std::time::Duration::from_secs(15)), // Send keepalive every 15s
            keepalive_max: 3, // Allow 3 missed keepalives before disconnect
            preferred,
            ..Default::default()
        };

        // Connect to SSH server
        let addr = format!("{}:{}", self.config.host, self.config.port);
        let mut handle = client::connect(
            Arc::new(config),
            &addr,
            SshHandler::with_trust_and_slot(
                &self.config.host,
                self.config.port,
                self.config.trust_unknown_hosts,
                self.host_key_sha256_hex.clone(),
            ),
        )
        .await
        .map_err(|e| ProviderError::ConnectionFailed(format!("SSH connection failed: {}", e)))?;

        tracing::info!("SFTP: SSH connection established, authenticating...");

        // Authenticate
        let authenticated = if self.config.private_key_path.is_some() {
            // Try key-based authentication
            self.authenticate_with_key(&mut handle).await?
        } else if let Some(password) = &self.config.password {
            // Try password authentication first, then keyboard-interactive as fallback
            use russh::client::KeyboardInteractiveAuthResponse;
            use secrecy::ExposeSecret;
            let pw = password.expose_secret().to_string();
            let result = handle
                .authenticate_password(&self.config.username, &pw)
                .await
                .map_err(|e| {
                    ProviderError::AuthenticationFailed(format!("Password auth failed: {}", e))
                })?;
            if matches!(result, AuthResult::Success) {
                true
            } else {
                // Fallback: keyboard-interactive (many servers like SourceForge require this)
                tracing::info!("SFTP: Password auth not accepted, trying keyboard-interactive...");
                let ki_result = handle
                    .authenticate_keyboard_interactive_start(&self.config.username, None::<String>)
                    .await
                    .map_err(|e| {
                        ProviderError::AuthenticationFailed(format!(
                            "Keyboard-interactive auth failed: {}",
                            e
                        ))
                    })?;
                match ki_result {
                    KeyboardInteractiveAuthResponse::Success => true,
                    KeyboardInteractiveAuthResponse::Failure { .. } => false,
                    KeyboardInteractiveAuthResponse::InfoRequest { prompts, .. } => {
                        // Server asks for responses - send password for each prompt
                        let responses: Vec<String> = prompts.iter().map(|_| pw.clone()).collect();
                        let resp = handle
                            .authenticate_keyboard_interactive_respond(responses)
                            .await
                            .map_err(|e| {
                                ProviderError::AuthenticationFailed(format!(
                                    "Keyboard-interactive respond failed: {}",
                                    e
                                ))
                            })?;
                        matches!(resp, KeyboardInteractiveAuthResponse::Success)
                    }
                }
            }
        } else {
            return Err(ProviderError::AuthenticationFailed(
                "No authentication method provided (need password or private key)".to_string(),
            ));
        };

        if !authenticated {
            return Err(ProviderError::AuthenticationFailed(
                "Authentication rejected by server".to_string(),
            ));
        }

        tracing::info!("SFTP: Authenticated successfully, opening SFTP channel...");

        // Open SFTP subsystem channel
        let channel = handle.channel_open_session().await.map_err(|e| {
            ProviderError::ConnectionFailed(format!("Failed to open session channel: {}", e))
        })?;

        channel.request_subsystem(true, "sftp").await.map_err(|e| {
            ProviderError::ConnectionFailed(format!("Failed to request SFTP subsystem: {}", e))
        })?;

        // Create SFTP session from channel
        let sftp = SftpSession::new(channel.into_stream()).await.map_err(|e| {
            ProviderError::ConnectionFailed(format!("Failed to create SFTP session: {}", e))
        })?;

        // Get home directory (canonicalize ".")
        let home = sftp.canonicalize(".").await.map_err(|e| {
            ProviderError::ConnectionFailed(format!("Failed to get home directory: {}", e))
        })?;

        self.home_dir = home;

        // Set initial directory
        if let Some(initial) = &self.config.initial_path {
            self.current_dir = self.normalize_path(initial);
        } else {
            self.current_dir = self.home_dir.clone();
        }

        self.ssh_handle = Some(Arc::new(TokioMutex::new(handle)));
        self.sftp = Some(sftp);

        // PD-SFTP-1: capture a secure connection spec now, while
        // `self.config` still holds the secrets (`provider_connect`
        // zeroizes the outer config only after this returns). The pinned
        // host-key fingerprint was populated by `SshHandler` during the
        // handshake; preserve an earlier pin if this dial reused one.
        let prior_pin = self
            .connection_spec
            .as_ref()
            .and_then(|s| s.pinned_host_key_sha256.clone());
        self.connection_spec = Some(SftpConnectionSpec {
            host: self.config.host.clone(),
            port: self.config.port,
            username: self.config.username.clone(),
            password: self.config.password.clone(),
            private_key_path: self.config.private_key_path.clone(),
            key_passphrase: self.config.key_passphrase.clone(),
            initial_path: self.config.initial_path.clone(),
            timeout_secs: self.config.timeout_secs,
            pinned_host_key_sha256: self.host_key_sha256_hex.get().cloned().or(prior_pin),
        });

        tracing::info!(
            "SFTP: Connected successfully to {} (home: {})",
            self.config.host,
            self.home_dir
        );
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        tracing::info!("SFTP: Disconnecting from {}", self.config.host);

        // Close SFTP session
        if let Some(sftp) = self.sftp.take() {
            let _ = sftp.close().await;
        }

        // Close SSH handle. Arc<Mutex<_>> means other clones (e.g. rsync-over-SSH borrowers)
        // may still hold references; the disconnect message is sent through the shared sender,
        // which is exactly what we want: the session is tore down once for everyone.
        if let Some(handle) = self.ssh_handle.take() {
            let guard = handle.lock().await;
            let _ = guard
                .disconnect(russh::Disconnect::ByApplication, "", "en")
                .await;
        }

        self.current_dir = "/".to_string();
        self.home_dir = "/".to_string();

        tracing::info!("SFTP: Disconnected");
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.sftp.is_some()
    }

    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let sftp = self.get_sftp()?;
        let full_path = self.normalize_path(path);

        tracing::debug!("SFTP: Listing directory: {}", full_path);

        let entries = sftp.read_dir(&full_path).await.map_err(|e| {
            classify_russh_err(e, |s| {
                ProviderError::NotFound(format!("Failed to list directory: {}", s))
            })
        })?;

        // Build the work list from the READDIR reply without any further I/O.
        // Every entry's own attributes are already in hand; a follow-up request
        // is needed only for the attr-less-server recovery and for real
        // symlinks. Collect them first, then resolve those follow-ups
        // concurrently over the one SFTP channel below instead of awaiting one
        // entry at a time (lever 2). `entry.metadata()` returns owned, Copy
        // attributes, so nothing borrows the directory reader past this loop.
        let mut pending = Vec::new();
        for entry in entries {
            let name = entry.file_name();

            // Skip . and ..
            if name == "." || name == ".." {
                continue;
            }

            // Tolerate malformed directory entries instead of letting one
            // bad name break the whole listing (FileZilla fzssh 1.2.1 class).
            // russh-sftp already decodes names with from_utf8_lossy, so a
            // non-UTF8 name survives as replacement chars; an empty name is
            // the only remaining unusable case and is skipped with a log.
            if name.is_empty() {
                tracing::warn!(
                    "SFTP: skipping directory entry with empty name in {}",
                    full_path
                );
                continue;
            }

            let entry_path = if full_path == "/" {
                format!("/{}", name)
            } else {
                format!("{}/{}", full_path.trim_end_matches('/'), name)
            };

            pending.push((name, entry_path, entry.metadata()));
        }

        // Max metadata follow-ups in flight over the single SFTP channel.
        // russh-sftp tags each request with an id from an atomic counter and
        // demultiplexes replies by id, and every SftpSession method takes
        // &self, so these pipeline on one connection with no extra sockets and
        // no trait change. 48 matches rclone's sftp backend default. After
        // lever 1 a well-behaved server issues no follow-up at all for a plain
        // file or directory, so this only bites for symlink-heavy trees and
        // for capability-poor servers, which is exactly where the serial walk
        // used to stall.
        const LIST_FOLLOWUP_CONCURRENCY: usize = 48;

        use futures_util::stream::StreamExt;
        let mut result: Vec<RemoteEntry> = futures_util::stream::iter(pending)
            .map(|(name, entry_path, readdir_attrs)| async move {
                let mut remote_entry =
                    Self::metadata_to_entry(name.clone(), entry_path.clone(), &readdir_attrs);

                // Minimal/embedded SFTP servers (some NAS firmware) omit file
                // attributes in READDIR replies. Without permission bits neither
                // our code nor russh-sftp's file_type() can tell a directory
                // from a file, so metadata_to_entry reports it as a file and the
                // directory becomes unenterable. Recover with an explicit STAT,
                // which these servers answer with full attributes (FileZilla
                // fzssh 1.2.1 / rclone sftp behaviour for capability-poor
                // servers). Bounded to the attr-less case so well-behaved
                // servers pay no extra round-trip.
                if remote_entry.permissions.is_none() {
                    if let Ok(stat) = sftp.metadata(&entry_path).await {
                        remote_entry =
                            Self::metadata_to_entry(name.clone(), entry_path.clone(), &stat);
                    }
                }

                // Check if it's a symlink. The READDIR attributes already carry
                // the entry's own mode (lstat semantics), so a well-behaved
                // server answers this for free. Only when the server sent no
                // file-type bits do we spend an SSH_FXP_LSTAT: note that
                // `remote_entry` may by then hold the recovered STAT attributes,
                // which follow the link and so can never show S_IFLNK.
                let is_symlink = match readdir_attrs.permissions.and_then(symlink_bit) {
                    Some(flag) => flag,
                    None => sftp
                        .symlink_metadata(&entry_path)
                        .await
                        .ok()
                        .and_then(|link_meta| link_meta.permissions)
                        .and_then(symlink_bit)
                        .unwrap_or(false),
                };

                if is_symlink {
                    remote_entry.is_symlink = true;
                    if let Ok(target) = sftp.read_link(&entry_path).await {
                        remote_entry.link_target = Some(target);
                    }
                    // Follow the symlink to determine the real type (file vs directory)
                    // metadata() follows symlinks, unlike symlink_metadata()
                    if let Ok(target_meta) = sftp.metadata(&entry_path).await {
                        if let Some(target_perms) = target_meta.permissions {
                            remote_entry.is_dir = (target_perms & 0o40000) != 0;
                        }
                        // Update size from target if available
                        if let Some(target_size) = target_meta.size {
                            remote_entry.size = target_size;
                        }
                    }
                }

                remote_entry
            })
            .buffer_unordered(LIST_FOLLOWUP_CONCURRENCY)
            .collect()
            .await;

        // Sort: directories first, then by name. buffer_unordered yields in
        // completion order, so the tiebreak on the exact name (not only the
        // lowercased one) keeps the output fully deterministic no matter which
        // follow-up finished first.
        result.sort_by(|a, b| match (a.is_dir, b.is_dir) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a
                .name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then_with(|| a.name.cmp(&b.name)),
        });

        tracing::debug!("SFTP: Listed {} entries", result.len());
        Ok(result)
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> {
        Ok(self.current_dir.clone())
    }

    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        let sftp = self.get_sftp()?;
        let full_path = self.normalize_path(path);

        // Verify the directory exists. Transport-level failures (server
        // idle reaper, broken pipe) are routed to ConnectionLost so the
        // command layer can reconnect+replay instead of misclassifying
        // them as a missing path.
        let metadata = sftp.metadata(&full_path).await.map_err(|e| {
            classify_russh_err(e, |s| {
                ProviderError::NotFound(format!("Directory not found: {}", s))
            })
        })?;

        if let Some(perms) = metadata.permissions {
            if (perms & 0o40000) == 0 {
                return Err(ProviderError::InvalidPath(format!(
                    "{} is not a directory",
                    full_path
                )));
            }
        }

        self.current_dir = full_path;
        tracing::debug!("SFTP: Changed directory to {}", self.current_dir);
        Ok(())
    }

    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        self.cd("..").await
    }

    async fn download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        self.ensure_connected().await?;
        let sftp = self.get_sftp()?;
        let full_path = self.normalize_path(remote_path);

        tracing::info!("SFTP: Downloading {} to {}", full_path, local_path);

        // Get file size
        let metadata = sftp.metadata(&full_path).await.map_err(|e| {
            classify_russh_err(e, |s| {
                ProviderError::NotFound(format!("File not found: {}", s))
            })
        })?;
        let total_size = metadata.size.unwrap_or(0);

        // PD-SFTP-2: intra-file parallelism. Engaged only when the user opted
        // in (`set_multi_thread_download(streams >= 2, ...)`), the file is
        // at/above the cutoff, and a real connection spec exists so we can
        // re-dial N independent SSH connections (the SftpConnectionPool kind).
        // Without all three this is a no-op and the single-stream path below
        // is unchanged: honest non-regression, no protocol overclaim.
        if self.multi_thread_streams >= 2
            && total_size >= self.multi_thread_cutoff
            && self.connection_spec.is_some()
        {
            return self
                .download_intra_file_pooled(remote_path, local_path, total_size, on_progress)
                .await;
        }

        // PD-PIPE-1: opt-in pipelined single-stream read on the *one
        // existing* SFTP session (no new connection, no pool). Off by
        // default = the serial loop below, byte-identical. Skipped when the
        // size is unknown/zero or a bandwidth limit is active (the serial
        // loop owns the exact throttling); the SHA-256 live gate guards it.
        if let Some(window) = sftp_read_pipeline_window() {
            if total_size > 0 && self.download_limit_bps == 0 {
                let sftp = self.get_sftp()?;
                let mut atomic = super::atomic_write::AtomicFile::new(local_path)
                    .await
                    .map_err(|e| {
                        ProviderError::TransferFailed(format!("Failed to create local file: {}", e))
                    })?;
                sftp_pipelined_download(
                    sftp,
                    &full_path,
                    total_size,
                    &mut atomic,
                    self.buffer_size,
                    window,
                    on_progress,
                )
                .await?;
                atomic.commit().await.map_err(|e| {
                    ProviderError::TransferFailed(format!("Failed to finalize download: {}", e))
                })?;
                tracing::info!(
                    "SFTP: Download complete (pipelined, window={}): {} bytes",
                    window,
                    total_size
                );
                return Ok(());
            }
        }

        // Open remote file
        let mut remote_file = sftp.open(&full_path).await.map_err(|e| {
            classify_russh_err(e, |s| {
                ProviderError::TransferFailed(format!("Failed to open remote file: {}", s))
            })
        })?;

        // Resumable local file: writes to `.aerotmp`, KEEPS the partial on
        // cancel/error (drop) so a later re-download resumes, renames on commit.
        let mut resumable = super::atomic_write::ResumableFile::open(local_path)
            .await
            .map_err(|e| {
                ProviderError::TransferFailed(format!("Failed to create local file: {}", e))
            })?;

        let mut resume_offset = resumable.offset();
        // A partial larger than the current remote file is stale (the remote
        // changed): discard it and start fresh rather than appending to bad data.
        if total_size > 0 && resume_offset > total_size {
            resumable.discard().await.ok();
            resumable = super::atomic_write::ResumableFile::open_fresh(local_path)
                .await
                .map_err(|e| {
                    ProviderError::TransferFailed(format!("Failed to create local file: {}", e))
                })?;
            resume_offset = 0;
        }

        // The partial already holds the whole file: finalize, nothing to read.
        if total_size > 0 && resume_offset == total_size {
            if let Some(ref progress) = on_progress {
                progress(total_size, total_size);
            }
            resumable.commit().await.map_err(|e| {
                ProviderError::TransferFailed(format!("Failed to finalize download: {}", e))
            })?;
            tracing::info!(
                "SFTP: Download already complete from partial: {} bytes",
                total_size
            );
            return Ok(());
        }

        // Resume: seek the remote read to the partial's end so we append the
        // correct bytes instead of re-fetching from zero.
        if resume_offset > 0 {
            use tokio::io::AsyncSeekExt;
            remote_file
                .seek(std::io::SeekFrom::Start(resume_offset))
                .await
                .map_err(|e| {
                    ProviderError::TransferFailed(format!("Failed to seek for resume: {}", e))
                })?;
            tracing::info!("SFTP: Resuming download from offset {}", resume_offset);
        }

        // Read and write in chunks with optional rate limiting
        let mut buffer = vec![0u8; self.buffer_size];
        let mut transferred: u64 = resume_offset;
        if let Some(ref progress) = on_progress {
            progress(transferred, total_size);
        }
        let start = std::time::Instant::now();

        loop {
            let bytes_read = remote_file.read(&mut buffer).await.map_err(|e| {
                classify_russh_err(e, |s| {
                    ProviderError::TransferFailed(format!("Read error: {}", s))
                })
            })?;

            if bytes_read == 0 {
                break;
            }

            resumable
                .write_all(&buffer[..bytes_read])
                .await
                .map_err(|e| ProviderError::TransferFailed(format!("Write error: {}", e)))?;

            transferred += bytes_read as u64;

            if let Some(ref progress) = on_progress {
                progress(transferred, total_size);
            }

            // Apply bandwidth throttling on bytes moved THIS session, so a
            // resume does not over-sleep for already-downloaded data.
            if self.download_limit_bps > 0 {
                let session_bytes = transferred - resume_offset;
                let expected = std::time::Duration::from_secs_f64(
                    session_bytes as f64 / self.download_limit_bps as f64,
                );
                let elapsed = start.elapsed();
                if expected > elapsed {
                    tokio::time::sleep(expected - elapsed).await;
                }
            }
        }

        resumable.commit().await.map_err(|e| {
            ProviderError::TransferFailed(format!("Failed to finalize download: {}", e))
        })?;

        tracing::info!("SFTP: Download complete: {} bytes", transferred);
        Ok(())
    }

    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        let sftp = self.get_sftp()?;
        let full_path = self.normalize_path(remote_path);
        let limit = super::MAX_DOWNLOAD_TO_BYTES;

        tracing::debug!("SFTP: Reading file to bytes: {}", full_path);

        // H2: Check file size before reading to prevent OOM
        if let Ok(metadata) = sftp.metadata(&full_path).await {
            if metadata.size.unwrap_or(0) > limit {
                return Err(ProviderError::TransferFailed(format!(
                    "File too large for in-memory download ({:.1} MB). Use streaming download for files over {:.0} MB.",
                    metadata.size.unwrap_or(0) as f64 / 1_048_576.0,
                    limit as f64 / 1_048_576.0,
                )));
            }
        }

        let data = sftp.read(&full_path).await.map_err(|e| {
            classify_russh_err(e, |s| {
                ProviderError::TransferFailed(format!("Failed to read file: {}", s))
            })
        })?;

        if data.len() as u64 > limit {
            return Err(ProviderError::TransferFailed(format!(
                "Download exceeded {:.0} MB size limit. Use streaming download for large files.",
                limit as f64 / 1_048_576.0,
            )));
        }

        Ok(data)
    }

    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        use tokio::io::AsyncWriteExt;

        self.ensure_connected().await?;
        let sftp = self.get_sftp()?;
        let full_path = self.normalize_path(remote_path);

        tracing::info!("SFTP: Uploading {} to {}", local_path, full_path);

        // Get local file size for progress reporting
        let total_size = tokio::fs::metadata(local_path)
            .await
            .map(|m| m.len())
            .unwrap_or(0);

        tracing::info!("SFTP: Upload local file size: {} bytes", total_size);

        // Open local file
        let mut local_file = tokio::fs::File::open(local_path).await.map_err(|e| {
            ProviderError::TransferFailed(format!("Failed to open local file: {}", e))
        })?;

        // Create remote file via russh_sftp (uses existing SSH session, no second connection)
        let mut remote_file = sftp.create(&full_path).await.map_err(|e| {
            classify_russh_err(e, |s| {
                ProviderError::TransferFailed(format!("Failed to create remote file: {}", s))
            })
        })?;

        // Read and write in chunks with optional rate limiting
        let mut buffer = vec![0u8; self.buffer_size];
        let mut transferred: u64 = 0;
        let start = std::time::Instant::now();

        loop {
            let bytes_read = tokio::io::AsyncReadExt::read(&mut local_file, &mut buffer)
                .await
                .map_err(|e| ProviderError::TransferFailed(format!("Local read error: {}", e)))?;

            if bytes_read == 0 {
                break;
            }

            remote_file
                .write_all(&buffer[..bytes_read])
                .await
                .map_err(|e| {
                    classify_russh_err(e, |s| {
                        ProviderError::TransferFailed(format!("Remote write error: {}", s))
                    })
                })?;

            transferred += bytes_read as u64;

            if let Some(ref progress) = on_progress {
                progress(transferred, total_size);
            }

            // Apply bandwidth throttling
            if self.upload_limit_bps > 0 {
                let expected = std::time::Duration::from_secs_f64(
                    transferred as f64 / self.upload_limit_bps as f64,
                );
                let elapsed = start.elapsed();
                if expected > elapsed {
                    tokio::time::sleep(expected - elapsed).await;
                }
            }
        }

        // Ensure all data is flushed to remote
        remote_file.shutdown().await.map_err(|e| {
            classify_russh_err(e, |s| {
                ProviderError::TransferFailed(format!("Failed to flush remote file: {}", s))
            })
        })?;

        self.verify_remote_upload_size(sftp, &full_path, total_size)
            .await?;

        // Keep remote mtime aligned with the local source so repeated sync
        // scans don't re-upload unchanged files just because the server stamped
        // the file with upload time.
        self.preserve_remote_mtime(sftp, &full_path, local_path)
            .await;

        tracing::info!(
            "SFTP: Upload complete via russh_sftp: {} bytes",
            transferred
        );
        Ok(())
    }

    /// SFTP can append the tail of an interrupted upload from a byte offset
    /// (the GUI "Resume" action), so the remote partial is not re-sent.
    fn supports_resume_upload_append(&self) -> bool {
        true
    }

    /// Resume an interrupted upload: append the local file's tail onto the
    /// remote partial instead of re-sending from zero. The caller offset is
    /// clamped to the real remote size (re-stat) so a stale offset can never
    /// append past a short remote file and corrupt it. Falls back to a full
    /// `upload` when there is nothing usable to resume from.
    async fn resume_upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        offset: u64,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        use russh_sftp::protocol::OpenFlags;
        use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

        self.ensure_connected().await?;

        let total_size = tokio::fs::metadata(local_path).await.map_err(|e| {
            ProviderError::TransferFailed(format!("Failed to stat local file: {}", e))
        })?;
        let total_size = total_size.len();

        // Re-stat the remote so the resume offset reflects what actually landed.
        let remote_size = {
            let sftp = self.get_sftp()?;
            let full_path = self.normalize_path(remote_path);
            sftp.metadata(&full_path)
                .await
                .ok()
                .and_then(|m| m.size)
                .unwrap_or(0)
        };

        match plan_resume_upload(offset, remote_size, total_size) {
            ResumeUploadPlan::FullUpload => {
                // No usable partial on the remote: do a normal full upload.
                return self.upload(local_path, remote_path, on_progress).await;
            }
            ResumeUploadPlan::AlreadyComplete => {
                if let Some(ref progress) = on_progress {
                    progress(total_size, total_size);
                }
                tracing::info!(
                    "SFTP: Resume upload no-op, remote already has {} bytes",
                    remote_size
                );
                return Ok(());
            }
            ResumeUploadPlan::Append(start_offset) => {
                let sftp = self.get_sftp()?;
                let full_path = self.normalize_path(remote_path);
                tracing::info!(
                    "SFTP: Resuming upload of {} from offset {} (local {} bytes)",
                    full_path,
                    start_offset,
                    total_size
                );

                // Open WRITE|CREATE (no TRUNCATE) and seek to the partial's end.
                // We seek explicitly instead of using APPEND: some servers ignore
                // the seek when APPEND is set and always write at EOF.
                let mut remote_file = sftp
                    .open_with_flags(&full_path, OpenFlags::WRITE | OpenFlags::CREATE)
                    .await
                    .map_err(|e| {
                        classify_russh_err(e, |s| {
                            ProviderError::TransferFailed(format!(
                                "Failed to open remote for resume: {}",
                                s
                            ))
                        })
                    })?;
                remote_file
                    .seek(std::io::SeekFrom::Start(start_offset))
                    .await
                    .map_err(|e| {
                        ProviderError::TransferFailed(format!(
                            "Failed to seek remote for resume: {}",
                            e
                        ))
                    })?;

                let mut local_file = tokio::fs::File::open(local_path).await.map_err(|e| {
                    ProviderError::TransferFailed(format!("Failed to open local file: {}", e))
                })?;
                local_file
                    .seek(std::io::SeekFrom::Start(start_offset))
                    .await
                    .map_err(|e| {
                        ProviderError::TransferFailed(format!(
                            "Failed to seek local for resume: {}",
                            e
                        ))
                    })?;

                let mut buffer = vec![0u8; self.buffer_size];
                let mut transferred: u64 = start_offset;
                if let Some(ref progress) = on_progress {
                    progress(transferred, total_size);
                }
                let start = std::time::Instant::now();

                loop {
                    let bytes_read = AsyncReadExt::read(&mut local_file, &mut buffer)
                        .await
                        .map_err(|e| {
                            ProviderError::TransferFailed(format!("Local read error: {}", e))
                        })?;
                    if bytes_read == 0 {
                        break;
                    }
                    remote_file
                        .write_all(&buffer[..bytes_read])
                        .await
                        .map_err(|e| {
                            classify_russh_err(e, |s| {
                                ProviderError::TransferFailed(format!("Remote write error: {}", s))
                            })
                        })?;
                    transferred += bytes_read as u64;
                    if let Some(ref progress) = on_progress {
                        progress(transferred, total_size);
                    }
                    // Throttle only on bytes moved THIS session so a resume does
                    // not over-sleep for already-uploaded data.
                    if self.upload_limit_bps > 0 {
                        let session_bytes = transferred - start_offset;
                        let expected = std::time::Duration::from_secs_f64(
                            session_bytes as f64 / self.upload_limit_bps as f64,
                        );
                        let elapsed = start.elapsed();
                        if expected > elapsed {
                            tokio::time::sleep(expected - elapsed).await;
                        }
                    }
                }

                remote_file.shutdown().await.map_err(|e| {
                    classify_russh_err(e, |s| {
                        ProviderError::TransferFailed(format!("Failed to flush remote file: {}", s))
                    })
                })?;

                self.verify_remote_upload_size(sftp, &full_path, total_size)
                    .await?;

                self.preserve_remote_mtime(sftp, &full_path, local_path)
                    .await;

                tracing::info!("SFTP: Resume upload complete: {} bytes total", transferred);
                Ok(())
            }
        }
    }

    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let sftp = self.get_sftp()?;
        let full_path = self.normalize_path(path);

        tracing::info!("SFTP: Creating directory: {}", full_path);

        sftp.create_dir(&full_path).await.map_err(|e| {
            classify_russh_err(e, |s| {
                ProviderError::ServerError(format!("Failed to create directory: {}", s))
            })
        })?;

        Ok(())
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        let sftp = self.get_sftp()?;
        let full_path = self.normalize_path(path);

        tracing::info!("SFTP: Deleting file: {}", full_path);

        sftp.remove_file(&full_path).await.map_err(|e| {
            classify_russh_err(e, |s| {
                ProviderError::ServerError(format!("Failed to delete file: {}", s))
            })
        })?;

        Ok(())
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let sftp = self.get_sftp()?;
        let full_path = self.normalize_path(path);

        tracing::info!("SFTP: Removing directory: {}", full_path);

        sftp.remove_dir(&full_path).await.map_err(|e| {
            classify_russh_err(e, |s| {
                ProviderError::ServerError(format!("Failed to remove directory: {}", s))
            })
        })?;

        Ok(())
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        let full_path = self.normalize_path(path);

        tracing::info!("SFTP: Recursively removing directory: {}", full_path);

        // List all entries
        let entries = self.list(&full_path).await?;

        // Delete all entries recursively (GAP-A02: skip symlinks to prevent following into target dirs)
        for entry in entries {
            if entry.is_symlink {
                self.delete(&entry.path).await?;
            } else if entry.is_dir {
                // Use Box::pin to avoid infinite recursion type issues
                Box::pin(self.rmdir_recursive(&entry.path)).await?;
            } else {
                self.delete(&entry.path).await?;
            }
        }

        // Now remove the empty directory
        self.rmdir(&full_path).await
    }

    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let sftp = self.get_sftp()?;
        let from_path = self.normalize_path(from);
        let to_path = self.normalize_path(to);

        tracing::info!("SFTP: Renaming {} to {}", from_path, to_path);

        sftp.rename(&from_path, &to_path).await.map_err(|e| {
            classify_russh_err(e, |s| {
                ProviderError::ServerError(format!("Failed to rename: {}", s))
            })
        })?;

        Ok(())
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        let sftp = self.get_sftp()?;
        let full_path = self.normalize_path(path);

        let metadata = sftp.metadata(&full_path).await.map_err(|e| {
            classify_russh_err(e, |s| {
                ProviderError::NotFound(format!("File not found: {}", s))
            })
        })?;

        let name = Path::new(&full_path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| full_path.clone());

        let mut entry = Self::metadata_to_entry(name, full_path.clone(), &metadata);

        // Check for symlink
        if let Ok(link_meta) = sftp.symlink_metadata(&full_path).await {
            if let Some(perms) = link_meta.permissions {
                if (perms & 0o170000) == 0o120000 {
                    entry.is_symlink = true;
                    if let Ok(target) = sftp.read_link(&full_path).await {
                        entry.link_target = Some(target);
                    }
                }
            }
        }

        Ok(entry)
    }

    async fn size(&mut self, path: &str) -> Result<u64, ProviderError> {
        let sftp = self.get_sftp()?;
        let full_path = self.normalize_path(path);

        let metadata = sftp.metadata(&full_path).await.map_err(|e| {
            classify_russh_err(e, |s| {
                ProviderError::NotFound(format!("File not found: {}", s))
            })
        })?;

        Ok(metadata.size.unwrap_or(0))
    }

    async fn exists(&mut self, path: &str) -> Result<bool, ProviderError> {
        let sftp = self.get_sftp()?;
        let full_path = self.normalize_path(path);

        match sftp.try_exists(&full_path).await {
            Ok(exists) => Ok(exists),
            Err(_) => Ok(false),
        }
    }

    fn supports_checksum(&self) -> bool {
        // We can attempt a server-side hash whenever the SSH session is up.
        // `checksum()` degrades to an empty map (consumers then omit) if the
        // server has no `sha256sum`: honest, like rclone.
        self.ssh_handle.is_some()
    }

    /// Server-side SHA-256 computed by the remote host via an SSH exec
    /// channel (`sha256sum`). The file is read and hashed entirely on the
    /// server: no file content crosses the wire to us, unlike a download.
    ///
    /// Returns an empty map (not an error) when the server lacks
    /// `sha256sum`, the command exits non-zero, or the output is
    /// unparseable: callers then omit the hash, matching rclone's
    /// behaviour of silently skipping hashes a backend cannot provide.
    async fn checksum(
        &mut self,
        path: &str,
    ) -> Result<std::collections::HashMap<String, String>, ProviderError> {
        let handle = self.ssh_handle.clone().ok_or(ProviderError::NotConnected)?;
        let full_path = self.normalize_path(path);
        // `--` ends option parsing; the path is fully single-quoted so no
        // shell metacharacter (`$()`, backtick, `;`, space, newline) in a
        // listing-derived name can break out. See `shell_single_quote`.
        let cmd = format!("sha256sum -- {}", shell_single_quote(&full_path));

        let (stdout, _stderr, _exit) = match ssh_exec_collect(handle, &cmd, 4096).await {
            Ok(v) => v,
            // A transport/channel failure is reported as "no server hash"
            // so consumers gracefully omit rather than failing the listing.
            Err(_) => return Ok(std::collections::HashMap::new()),
        };

        // `sha256sum` prints the `<64-hex>  name` line to stdout ONLY on
        // success; on any error it writes to stderr and emits no digest.
        // A well-formed digest is therefore itself proof of success, so we
        // do not gate on the exec exit status: some SSH servers deliver
        // `exit-status` after `eof`/`close`, and `ssh_exec_collect` then
        // reports the EXIT_ABNORMAL sentinel even though stdout is complete
        // and correct (observed on the OpenSSH lab box with an SFTP
        // subsystem channel concurrently open on the same handle).
        let mut out = std::collections::HashMap::new();
        if let Some(token) = String::from_utf8_lossy(&stdout).split_whitespace().next() {
            let digest = token.to_ascii_lowercase();
            if digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()) {
                out.insert("sha256".to_string(), digest);
            }
        }
        Ok(out)
    }

    async fn keep_alive(&mut self) -> Result<(), ProviderError> {
        // SFTP over SSH is a persistent connection
        // Just check if we're still connected
        if self.sftp.is_none() {
            return Err(ProviderError::NotConnected);
        }

        // Optionally do a simple operation to verify connection.
        // canonicalize(".") is lightweight. A failure here means the
        // server idle reaper or NAT closed the channel: classify as
        // ConnectionLost so the caller can reconnect+replay rather than
        // treating it as a permanent disconnect.
        if let Some(sftp) = &self.sftp {
            sftp.canonicalize(".").await.map_err(|e| {
                classify_russh_err(e, |s| {
                    ProviderError::ConnectionLost(format!("SFTP keepalive failed: {}", s))
                })
            })?;
        }

        Ok(())
    }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        Ok(format!(
            "SFTP Server: {}:{} (user: {}, home: {})",
            self.config.host, self.config.port, self.config.username, self.home_dir
        ))
    }

    fn supports_chmod(&self) -> bool {
        true // SFTP supports chmod
    }

    async fn chmod(&mut self, path: &str, mode: u32) -> Result<(), ProviderError> {
        let sftp = self.get_sftp()?;
        let full_path = self.normalize_path(path);

        tracing::info!("SFTP: chmod {} to {:o}", full_path, mode);

        let attrs = russh_sftp::protocol::FileAttributes {
            permissions: Some(mode),
            ..Default::default()
        };

        sftp.set_metadata(&full_path, attrs).await.map_err(|e| {
            classify_russh_err(e, |s| {
                ProviderError::ServerError(format!("Failed to chmod: {}", s))
            })
        })?;

        Ok(())
    }

    fn supports_symlinks(&self) -> bool {
        true // SFTP supports symlinks
    }

    fn supports_find(&self) -> bool {
        true
    }

    async fn find(&mut self, path: &str, pattern: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let sftp = self.get_sftp()?;
        let root = self.normalize_path(path);
        let mut results = Vec::new();
        let mut dirs_to_scan = vec![root];

        while let Some(dir) = dirs_to_scan.pop() {
            let entries = match sftp.read_dir(&dir).await {
                Ok(e) => e,
                Err(_) => continue, // Skip inaccessible directories
            };

            for entry in entries {
                let name = entry.file_name();
                if name == "." || name == ".." {
                    continue;
                }

                let entry_path = if dir == "/" {
                    format!("/{}", name)
                } else {
                    format!("{}/{}", dir.trim_end_matches('/'), name)
                };

                let remote_entry =
                    Self::metadata_to_entry(name.clone(), entry_path.clone(), &entry.metadata());

                if remote_entry.is_dir {
                    dirs_to_scan.push(entry_path.clone());
                }

                if super::matches_find_pattern(&name, pattern) {
                    results.push(remote_entry);
                    if results.len() >= 500 {
                        return Ok(results);
                    }
                }
            }
        }

        Ok(results)
    }

    async fn storage_info(&mut self) -> Result<super::StorageInfo, ProviderError> {
        let sftp = self.get_sftp()?;
        let path = self.normalize_path(".");

        let stat = sftp
            .fs_info(path)
            .await
            .map_err(|e| {
                classify_russh_err(e, |s| {
                    ProviderError::ServerError(format!("statvfs failed: {}", s))
                })
            })?
            .ok_or_else(|| {
                ProviderError::NotSupported("Server does not support statvfs".to_string())
            })?;

        let total = stat.blocks * stat.fragment_size;
        let free = stat.blocks_avail * stat.fragment_size;
        let used = total.saturating_sub(free);

        Ok(super::StorageInfo {
            used,
            total,
            free,
            versioning_bytes: None,
        })
    }

    async fn set_speed_limit(
        &mut self,
        upload_kb: u64,
        download_kb: u64,
    ) -> Result<(), ProviderError> {
        self.upload_limit_bps = upload_kb * 1024;
        self.download_limit_bps = download_kb * 1024;
        tracing::info!(
            "SFTP: Speed limits set: download={}KB/s upload={}KB/s",
            download_kb,
            upload_kb
        );
        Ok(())
    }

    async fn get_speed_limit(&mut self) -> Result<(u64, u64), ProviderError> {
        Ok((self.upload_limit_bps / 1024, self.download_limit_bps / 1024))
    }

    fn transfer_optimization_hints(&self) -> super::TransferOptimizationHints {
        // Shaped-graph multipart trait (S3-T09): intentionally NotSupported
        // by design on SFTP.
        //
        // SFTP v3 (the version russh and the broader OpenSSH ecosystem
        // expose) writes to an open file handle with `SSH_FXP_WRITE` at
        // explicit offsets. In principle a single file could be written
        // by multiple concurrent `SSH_FXP_WRITE` packets at different
        // offsets over one channel, but in practice (a) most servers
        // serialise writes on the open file handle, (b) `russh-sftp`
        // does not expose per-write concurrency controls, and (c) the
        // ssh2-libssh2 SCP backend we use for uploads (workaround for
        // russh 0.57 write buffering races on embedded SFTP servers like
        // WD MyCloud NAS) is strictly stream-oriented. Real file-level
        // parallelism on SFTP comes from `SftpConnectionPool` re-dialling
        // independent SSH channels (see `transfer_executor_kind` below).
        //
        // Wiring a per-part SFTP backend is tracked as T-DEBT-09
        // (`--sftp-concurrency` flag) for v4.x: it would require both
        // dropping the SCP write workaround and parametrising
        // `SftpConnectionPool` with a per-file fan-out. Until then we
        // leave `supports_multipart=false` and let the runner pick the
        // legacy single-stream path.
        super::TransferOptimizationHints {
            supports_resume_download: false,
            supports_resume_upload: false,
            supports_range_download: true,
            supports_compression: true,
            supports_delta_sync: true,
            ..Default::default()
        }
    }

    /// PD-SFTP-1: advertise real file-level parallelism only once a secure
    /// connection spec exists to re-dial independent SSH connections from.
    /// Without it (never connected, or credentials unavailable) SFTP stays
    /// a single locked lease: honest non-regression, no overclaim.
    fn transfer_executor_kind(&self) -> ProviderTransferExecutorKind {
        if self.connection_spec.is_some() {
            ProviderTransferExecutorKind::SftpConnectionPool
        } else {
            ProviderTransferExecutorKind::LockedSingle
        }
    }

    /// Conservative initial cap, mirroring the FTP pool clamp (1..8).
    /// Each lease is a full independent SSH connection; raise only after a
    /// live benchmark on the target server says it pays.
    fn transfer_executor_max_sessions(&self) -> u16 {
        4
    }

    /// Produce an independent transfer worker. It is **not connected**:
    /// it carries only the secure `SftpConnectionSpec` and dials its own
    /// separate SSH connection lazily on the first transfer
    /// (`ensure_connected`). No SSH handle or channel is shared, so N
    /// workers are N independent connections, exactly like the FTP pool.
    fn clone_for_transfer(&self) -> Result<Box<dyn StorageProvider>, ProviderError> {
        let spec = self.connection_spec.clone().ok_or_else(|| {
            ProviderError::NotSupported(
                "SFTP clone_for_transfer requires a captured connection spec".to_string(),
            )
        })?;
        let mut worker = SftpProvider::new(spec.to_config());
        worker.current_dir = self.current_dir.clone();
        worker.home_dir = self.home_dir.clone();
        worker.download_limit_bps = self.download_limit_bps;
        worker.upload_limit_bps = self.upload_limit_bps;
        worker.compression_enabled = self.compression_enabled;
        worker.buffer_size = self.buffer_size;
        worker.connection_spec = Some(spec);
        worker.multi_thread_streams = self.multi_thread_streams;
        worker.multi_thread_cutoff = self.multi_thread_cutoff;
        Ok(Box::new(worker))
    }

    fn set_chunk_sizes(&mut self, upload: Option<u64>, download: Option<u64>) {
        // Cap at 16 MB (larger buffers waste memory without improving throughput).
        // BUFFER-01: SFTP uses a single transfer buffer for both directions, so
        // when both --chunk-size and --buffer-size are given we apply the larger
        // value deterministically instead of silently letting the last writer win.
        let cap = 16 * 1024 * 1024;
        let requested = upload.into_iter().chain(download).max();
        if let Some(size) = requested {
            self.buffer_size = (size as usize).clamp(4096, cap);
        }
    }

    /// PD-SFTP-2: opt into intra-file parallelism. `streams <= 1` keeps the
    /// single-stream path (honest default). `cutoff` floors at 1 MiB so a
    /// degenerate value can never split a tiny file into N SSH handshakes.
    /// The intra-file path additionally requires a real connection spec
    /// (`SftpConnectionPool`) at `download()` time, so a not-connected /
    /// credential-less provider never overclaims.
    fn set_multi_thread_download(&mut self, streams: usize, cutoff_bytes: u64) {
        self.multi_thread_streams = streams.clamp(1, SFTP_MULTI_THREAD_MAX_STREAMS);
        self.multi_thread_cutoff = cutoff_bytes.max(1024 * 1024);
    }

    fn supports_delta_sync(&self) -> bool {
        true
    }

    async fn read_range(
        &mut self,
        path: &str,
        offset: u64,
        len: u64,
    ) -> Result<Vec<u8>, ProviderError> {
        // PD-SFTP-1: clone-for-transfer workers start unconnected and
        // re-dial on first transfer. `download()` already does this; the
        // segmented engine (`run_provider_segmented_download`) calls
        // `read_range` directly on the pool worker, so the same self-dial
        // must happen here or every segmented download against a clone
        // pool fails with `Not connected`.
        self.ensure_connected().await?;
        let sftp = self
            .sftp
            .as_ref()
            .ok_or_else(|| ProviderError::NotConnected)?;
        let full_path = self.normalize_path(path);

        let mut file = sftp.open(&full_path).await.map_err(|e| {
            classify_russh_err(e, |s| {
                ProviderError::ServerError(format!("Failed to open file for range read: {}", s))
            })
        })?;

        // Seek to offset
        use tokio::io::{AsyncReadExt, AsyncSeekExt};
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(|e| {
                classify_russh_err(e, |s| {
                    ProviderError::ServerError(format!("Failed to seek: {}", s))
                })
            })?;

        // GAP-A03: Cap read_range allocation to prevent attacker-controlled OOM
        const MAX_READ_RANGE: u64 = 100 * 1024 * 1024; // 100 MB
        if len > MAX_READ_RANGE {
            return Err(ProviderError::Other(format!(
                "Read range size {} exceeds maximum {} bytes",
                len, MAX_READ_RANGE
            )));
        }

        // Read exact len bytes
        let mut buf = vec![0u8; len as usize];
        let mut total_read = 0usize;
        while total_read < len as usize {
            let n = file.read(&mut buf[total_read..]).await.map_err(|e| {
                classify_russh_err(e, |s| {
                    ProviderError::ServerError(format!("Failed to read range: {}", s))
                })
            })?;
            if n == 0 {
                break;
            }
            total_read += n;
        }
        buf.truncate(total_read);
        Ok(buf)
    }
}

/// PD-PIPE-1: parse `AEROFTP_SFTP_READ_PIPELINE` into a pipeline window.
///
/// Mirrors the `AEROFTP_RANGE_GRAPH` opt-in discipline (PD-ADAPT-1d): the
/// flag is **off by default**, so an unset/`0`/`1`/`false` value keeps the
/// exact serial `download()` loop (byte-identical, diff-0). A value `>= 2`
/// (or a truthy word) enables bounded read pipelining on the **single
/// existing** SFTP session and is the only thing that changes the read
/// scheduling. The window is capped so a hostile value cannot blow memory
/// (`window * buffer_size` is the worst-case in-flight footprint).
const SFTP_PIPELINE_MAX_WINDOW: usize = 64;
const SFTP_PIPELINE_DEFAULT_WINDOW: usize = 16;

/// Pure parser for the PD-PIPE-1 flag value (env read split out so it is
/// deterministically unit-testable without mutating process-global env).
fn parse_sftp_read_pipeline_window(raw: Option<&str>) -> Option<usize> {
    let v = raw?.trim().to_ascii_lowercase();
    match v.as_str() {
        "" | "0" | "1" | "false" | "off" | "no" => None,
        "true" | "on" | "yes" => Some(SFTP_PIPELINE_DEFAULT_WINDOW),
        _ => match v.parse::<usize>() {
            Ok(n) if n >= 2 => Some(n.min(SFTP_PIPELINE_MAX_WINDOW)),
            _ => None,
        },
    }
}

fn sftp_read_pipeline_window() -> Option<usize> {
    parse_sftp_read_pipeline_window(std::env::var("AEROFTP_SFTP_READ_PIPELINE").ok().as_deref())
}

/// PD-PIPE-1: read exactly `want` bytes from `file` starting at the absolute
/// offset `abs_off`, looping on short protocol reads. A `read() == 0` before
/// `want` is **not** an error here: it means EOF (the remote file is shorter
/// than the metadata size); the caller treats a returned buffer shorter than
/// the requested window as the end of the transfer, exactly as the serial
/// loop stops on `read() == 0`.
async fn sftp_pipelined_read_window(
    file: &mut russh_sftp::client::fs::File,
    abs_off: u64,
    want: usize,
) -> Result<Vec<u8>, ProviderError> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    file.seek(std::io::SeekFrom::Start(abs_off))
        .await
        .map_err(|e| {
            classify_russh_err(e, |s| {
                ProviderError::TransferFailed(format!("Seek error (pipeline): {}", s))
            })
        })?;

    let mut buf = vec![0u8; want];
    let mut filled = 0usize;
    while filled < want {
        let n = file.read(&mut buf[filled..]).await.map_err(|e| {
            classify_russh_err(e, |s| {
                ProviderError::TransferFailed(format!("Read error (pipeline): {}", s))
            })
        })?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    buf.truncate(filled);
    Ok(buf)
}

/// PD-PIPE-1: pipelined single-stream SFTP download over the **one existing**
/// SFTP session.
///
/// Root cause this addresses (PD-BENCH-1, master 9.6.4): the serial
/// `download()` loop issues a single outstanding `SSH_FXP_READ` and then
/// serially `write_all`s it, with no net/disk overlap, so one stream is far
/// below the link. russh-sftp's `SftpSession` multiplexes concurrent
/// requests by id over one channel (`RawSftpSession`, an internal `Arc`
/// shared by every `File` it opens), so opening `window` cheap file handles
/// to the same path and reading disjoint stripes concurrently keeps `window`
/// `SSH_FXP_READ` in flight on the same connection. **No new TCP connection,
/// no new pool, one SFTP session** (this is deliberately not the PD-SFTP-2
/// independent-connection pool).
///
/// Diff-0: the bytes written are `[0, min(total_size, EOF))` in strict
/// ascending order, identical to the serial loop for a static file (the
/// real, gated scenario). Chunks are produced in `window`-sized batches and
/// written in order; the first short read ends the transfer just like the
/// serial loop's `read() == 0`. Only the read scheduling differs. Trusting
/// the metadata size for windowing is the same accepted discipline as the
/// shipped PD-SFTP-2 range worker; every run is SHA-256 gated.
async fn sftp_pipelined_download(
    sftp: &SftpSession,
    full_path: &str,
    total_size: u64,
    atomic: &mut super::atomic_write::AtomicFile,
    chunk: usize,
    window: usize,
    on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
) -> Result<(), ProviderError> {
    let chunk = chunk.max(4096) as u64;
    let window = window.clamp(2, 64);
    let chunks_needed = total_size.div_ceil(chunk).max(1) as usize;
    let eff_window = window.min(chunks_needed);

    // `eff_window` handles, all on the SAME session: one SSH channel, the
    // RawSftpSession multiplexes the concurrent reads by request id.
    let mut handles: Vec<russh_sftp::client::fs::File> = Vec::with_capacity(eff_window);
    for _ in 0..eff_window {
        let f = sftp.open(full_path).await.map_err(|e| {
            classify_russh_err(e, |s| {
                ProviderError::TransferFailed(format!(
                    "Failed to open remote file (pipeline): {}",
                    s
                ))
            })
        })?;
        handles.push(f);
    }

    let mut transferred: u64 = 0;
    let mut offset: u64 = 0;
    'outer: while offset < total_size {
        // Plan up to `eff_window` consecutive chunks for this batch.
        let mut wants: Vec<usize> = Vec::with_capacity(eff_window);
        for _ in 0..eff_window {
            if offset >= total_size {
                break;
            }
            let want = std::cmp::min(chunk, total_size - offset) as usize;
            wants.push(want);
            offset += want as u64;
        }
        if wants.is_empty() {
            break;
        }

        // Issue the batch concurrently: each future borrows one distinct
        // handle (disjoint &mut via split_at_mut), so `window` reads are in
        // flight on the single connection at once.
        let n = wants.len();
        let batch_base = offset - wants.iter().map(|w| *w as u64).sum::<u64>();
        let (used, _rest) = handles.split_at_mut(n);
        let mut futs = Vec::with_capacity(n);
        let mut abs = batch_base;
        for (f, &want) in used.iter_mut().zip(wants.iter()) {
            futs.push(sftp_pipelined_read_window(f, abs, want));
            abs += want as u64;
        }
        let results = futures_util::future::try_join_all(futs).await?;

        // Write in strict offset order; the first short read is EOF.
        for (buf, &want) in results.iter().zip(wants.iter()) {
            atomic
                .write_all(buf)
                .await
                .map_err(|e| ProviderError::TransferFailed(format!("Write error: {}", e)))?;
            transferred += buf.len() as u64;
            if let Some(ref progress) = on_progress {
                progress(transferred, total_size);
            }
            if buf.len() < want {
                break 'outer;
            }
        }
    }

    Ok(())
}

/// PD-PIPE-2 strict gate (pure, unit-tested): inside a range sub-window a
/// `read() == 0` before the requested length is a **hard error**, never a
/// silent short read. This is exactly the serial PD-SFTP-2 worker's `n == 0`
/// gate (same message shape: expected/at-offset/got), factored out so the
/// strict-short-read-before-length path is deterministically testable
/// without a live SFTP server.
fn sftp_strict_short_read_check(
    n: usize,
    filled: usize,
    want: usize,
    abs_off: u64,
) -> Result<(), ProviderError> {
    if n == 0 && filled < want {
        return Err(ProviderError::TransferFailed(format!(
            "SFTP range short read: expected {} bytes at offset {}, got {}",
            want, abs_off, filled
        )));
    }
    Ok(())
}

/// PD-PIPE-2: read **exactly** `want` bytes from `file` at the absolute
/// offset `abs_off`. Unlike the PD-PIPE-1 [`sftp_pipelined_read_window`]
/// (EOF-tolerant: a short read is the end of a single-stream `download()`),
/// this is the **strict** sub-window read for the PD-SFTP-2 range worker:
/// a `read() == 0` before `want` is a hard [`ProviderError::TransferFailed`]
/// via [`sftp_strict_short_read_check`], byte-for-byte the serial worker's
/// strict gate. Each window must yield its full length or fail loud.
async fn sftp_pipelined_range_read_strict(
    file: &mut russh_sftp::client::fs::File,
    abs_off: u64,
    want: usize,
) -> Result<Vec<u8>, ProviderError> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    file.seek(std::io::SeekFrom::Start(abs_off))
        .await
        .map_err(|e| {
            classify_russh_err(e, |s| {
                ProviderError::ServerError(format!("Failed to seek to range start: {}", s))
            })
        })?;

    let mut buf = vec![0u8; want];
    let mut filled = 0usize;
    while filled < want {
        let n = file.read(&mut buf[filled..]).await.map_err(|e| {
            classify_russh_err(e, |s| {
                ProviderError::TransferFailed(format!("Range read error: {}", s))
            })
        })?;
        sftp_strict_short_read_check(n, filled, want, abs_off)?;
        filled += n;
    }
    Ok(buf)
}

/// PD-PIPE-2: pipeline the PD-SFTP-2 range worker's read of
/// `[start, start + expected)` on the worker's **own single** SFTP session.
///
/// Compounds with PD-SFTP-2: that slice gives N independent connections
/// (one per range window); this gives K pipelined reads per connection over
/// distinct `File` handles on **that connection's one `SftpSession`**
/// (russh-sftp multiplexes concurrent requests by id over one channel; the
/// internal `Arc<RawSftpSession>` is shared by every `File`). **No new TCP
/// connection, no new pool, no second channel** (the same vehicle PD-PIPE-1
/// proved, applied inside the worker). Byte-identical to the serial worker
/// loop: the same bytes written at the same absolute offsets, the same
/// strict short-read hard error and the same per-batch cancellation; only
/// the read scheduling differs.
#[allow(clippy::too_many_arguments)]
async fn sftp_pipelined_range_into(
    sftp: &SftpSession,
    full_path: &str,
    start: u64,
    expected: u64,
    out: &mut tokio::fs::File,
    chunk: usize,
    window: usize,
    aggregate: &Arc<AtomicU64>,
    cancel: &CancellationToken,
) -> Result<(), ProviderError> {
    use tokio::io::{AsyncSeekExt, AsyncWriteExt};

    let chunk = chunk.max(4096) as u64;
    let window = window.clamp(2, SFTP_PIPELINE_MAX_WINDOW);
    let chunks_needed = expected.div_ceil(chunk).max(1) as usize;
    let eff_window = window.min(chunks_needed);

    // `eff_window` handles, all on the worker's ONE existing session: one
    // SSH channel, the RawSftpSession multiplexes the concurrent reads by id.
    let mut handles: Vec<russh_sftp::client::fs::File> = Vec::with_capacity(eff_window);
    for _ in 0..eff_window {
        let f = sftp.open(full_path).await.map_err(|e| {
            classify_russh_err(e, |s| {
                ProviderError::TransferFailed(format!(
                    "Failed to open remote file for range (pipeline): {}",
                    s
                ))
            })
        })?;
        handles.push(f);
    }

    let stop = start + expected;
    let mut offset: u64 = start;
    while offset < stop {
        // Plan up to `eff_window` consecutive strict sub-stripes.
        let batch_base = offset;
        let mut wants: Vec<usize> = Vec::with_capacity(eff_window);
        for _ in 0..eff_window {
            if offset >= stop {
                break;
            }
            let want = std::cmp::min(chunk, stop - offset) as usize;
            wants.push(want);
            offset += want as u64;
        }
        if wants.is_empty() {
            break;
        }

        // Issue the batch concurrently: each future borrows one distinct
        // handle (disjoint &mut via split_at_mut), so `window` strict reads
        // are in flight on the single connection at once.
        let n = wants.len();
        let (used, _rest) = handles.split_at_mut(n);
        let mut futs = Vec::with_capacity(n);
        let mut abs = batch_base;
        for (f, &want) in used.iter_mut().zip(wants.iter()) {
            futs.push(sftp_pipelined_range_read_strict(f, abs, want));
            abs += want as u64;
        }

        // Cancellation stays responsive per batch and returns the EXACT
        // serial-worker "Transfer cancelled by user" error.
        let results = tokio::select! {
            _ = cancel.cancelled() => {
                return Err(ProviderError::TransferFailed(
                    "Transfer cancelled by user".to_string(),
                ));
            }
            r = futures_util::future::try_join_all(futs) => r?,
        };

        // Write each strict sub-stripe in strict offset order at its
        // absolute file offset; aggregate per chunk (the shared progress
        // counter, same as the serial worker).
        let mut abs = batch_base;
        for buf in results.iter() {
            out.seek(std::io::SeekFrom::Start(abs))
                .await
                .map_err(ProviderError::IoError)?;
            out.write_all(buf).await.map_err(ProviderError::IoError)?;
            aggregate.fetch_add(buf.len() as u64, Ordering::Relaxed);
            abs += buf.len() as u64;
        }
    }

    Ok(())
}

/// PD-SFTP-2 per-range worker: dial an **independent** SSH+SFTP connection
/// from `spec`, seek to `start`, and stream **exactly** `end - start + 1`
/// bytes into `temp_path` at absolute offset `start`. One call == one fresh
/// SSH connection (host-key pinned re-dial via `ensure_connected`), so N
/// ranges of one file = N independent connections, the same model as the
/// PD-SFTP-1 file-level pool and rclone's pooled SFTP.
///
/// Strict gate: a `read() == 0` before `expected` bytes is a hard
/// [`ProviderError`], never a silent short read. Writes are clamped to the
/// window so a remote file that grew mid-transfer cannot corrupt the
/// neighbouring range.
#[allow(clippy::too_many_arguments)]
async fn sftp_download_one_range(
    spec: SftpConnectionSpec,
    remote_path: String,
    current_dir: String,
    home_dir: String,
    buffer_size: usize,
    limit_bps: u64,
    compression_enabled: bool,
    start: u64,
    end: u64,
    temp_path: PathBuf,
    aggregate: Arc<AtomicU64>,
    cancel: CancellationToken,
) -> Result<ConcurrentRangeOutcome, ProviderError> {
    use tokio::io::{AsyncSeekExt, AsyncWriteExt};

    let expected = end - start + 1;

    // Independent worker: own socket + own auth, host-key pinned re-dial.
    let mut worker = SftpProvider::new(spec.to_config());
    worker.current_dir = current_dir;
    worker.home_dir = home_dir;
    worker.buffer_size = buffer_size;
    worker.compression_enabled = compression_enabled;
    worker.connection_spec = Some(spec);
    worker.ensure_connected().await?;

    let full_path = worker.normalize_path(&remote_path);
    let sftp = worker.get_sftp()?;

    // PD-PIPE-2: when the opt-in flag yields a window and no bandwidth cap
    // is active, pipeline this range worker's read of
    // `[start, start + expected)` on its OWN single SFTP session. Compounds
    // with PD-SFTP-2's N independent connections; no new connection / pool /
    // channel. Default (flag unset) or an active cap falls through to the
    // exact serial loop below = diff-0 (the serial loop owns the precise
    // throttle, same discipline as PD-PIPE-1 / `AEROFTP_RANGE_GRAPH`).
    if limit_bps == 0 {
        if let Some(window) = sftp_read_pipeline_window() {
            let mut out = tokio::fs::OpenOptions::new()
                .write(true)
                .open(&temp_path)
                .await
                .map_err(ProviderError::IoError)?;
            sftp_pipelined_range_into(
                sftp,
                &full_path,
                start,
                expected,
                &mut out,
                buffer_size,
                window,
                &aggregate,
                &cancel,
            )
            .await?;
            out.flush().await.map_err(ProviderError::IoError)?;
            out.sync_all().await.map_err(ProviderError::IoError)?;
            let _ = worker.disconnect().await;
            return Ok(ConcurrentRangeOutcome::Completed);
        }
    }

    let mut remote_file = sftp.open(&full_path).await.map_err(|e| {
        classify_russh_err(e, |s| {
            ProviderError::TransferFailed(format!("Failed to open remote file for range: {}", s))
        })
    })?;
    remote_file
        .seek(std::io::SeekFrom::Start(start))
        .await
        .map_err(|e| {
            classify_russh_err(e, |s| {
                ProviderError::ServerError(format!("Failed to seek to range start: {}", s))
            })
        })?;

    let mut out = tokio::fs::OpenOptions::new()
        .write(true)
        .open(&temp_path)
        .await
        .map_err(ProviderError::IoError)?;
    out.seek(std::io::SeekFrom::Start(start))
        .await
        .map_err(ProviderError::IoError)?;

    let mut buf = vec![0u8; buffer_size];
    let mut written: u64 = 0;
    let started = std::time::Instant::now();

    while written < expected {
        tokio::select! {
            _ = cancel.cancelled() => {
                return Err(ProviderError::TransferFailed(
                    "Transfer cancelled by user".to_string(),
                ));
            }
            read = remote_file.read(&mut buf) => {
                let n = read.map_err(|e| {
                    classify_russh_err(e, |s| {
                        ProviderError::TransferFailed(format!("Range read error: {}", s))
                    })
                })?;
                if n == 0 {
                    // Strict gate: premature EOF, no silent short read.
                    return Err(ProviderError::TransferFailed(format!(
                        "SFTP range short read: expected {} bytes at offset {}, got {}",
                        expected, start, written
                    )));
                }
                let take = std::cmp::min(n as u64, expected - written) as usize;
                out.write_all(&buf[..take])
                    .await
                    .map_err(ProviderError::IoError)?;
                aggregate.fetch_add(take as u64, Ordering::Relaxed);
                written += take as u64;

                if limit_bps > 0 {
                    let expected_elapsed = std::time::Duration::from_secs_f64(
                        written as f64 / limit_bps as f64,
                    );
                    let elapsed = started.elapsed();
                    if expected_elapsed > elapsed {
                        tokio::time::sleep(expected_elapsed - elapsed).await;
                    }
                }
            }
        }
    }

    out.flush().await.map_err(ProviderError::IoError)?;
    out.sync_all().await.map_err(ProviderError::IoError)?;
    let _ = worker.disconnect().await;
    Ok(ConcurrentRangeOutcome::Completed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pd_pipe1_flag_is_off_by_default_and_capped() {
        // diff-0 default: unset / falsey / the degenerate "1" window all
        // mean "serial loop", i.e. None.
        assert_eq!(parse_sftp_read_pipeline_window(None), None);
        for off in ["", " ", "0", "1", "false", "off", "no", "FALSE", "Off"] {
            assert_eq!(parse_sftp_read_pipeline_window(Some(off)), None, "{off:?}");
        }
        // Truthy words enable the default window.
        for on in ["true", "on", "yes", "TRUE", " On "] {
            assert_eq!(
                parse_sftp_read_pipeline_window(Some(on)),
                Some(SFTP_PIPELINE_DEFAULT_WINDOW),
                "{on:?}"
            );
        }
        // Explicit numeric window, only >= 2 enables; capped at the max.
        assert_eq!(parse_sftp_read_pipeline_window(Some("2")), Some(2));
        assert_eq!(parse_sftp_read_pipeline_window(Some("16")), Some(16));
        assert_eq!(
            parse_sftp_read_pipeline_window(Some("9999")),
            Some(SFTP_PIPELINE_MAX_WINDOW)
        );
        // Junk is safe-off.
        for junk in ["abc", "-3", "2.5", "0x10"] {
            assert_eq!(
                parse_sftp_read_pipeline_window(Some(junk)),
                None,
                "{junk:?}"
            );
        }
    }

    #[test]
    fn pd_pipe2_strict_short_read_before_length_is_hard_error() {
        // The strict-short-read-before-length hard-error path of the
        // PD-PIPE-2 assembler (deterministic, no live server). A protocol
        // `read() == 0` with fewer than `want` bytes filled is the exact
        // serial PD-SFTP-2 worker error: never a silent short read.
        let err = sftp_strict_short_read_check(0, 100, 4096, 1_048_576)
            .expect_err("zero read before length must be a hard error");
        match err {
            ProviderError::TransferFailed(m) => {
                assert!(m.contains("short read"), "message shape: {m}");
                assert!(m.contains("4096"), "want in message: {m}");
                assert!(m.contains("1048576"), "abs offset in message: {m}");
                assert!(m.contains("100"), "filled-so-far in message: {m}");
            }
            other => panic!("expected TransferFailed, got {other:?}"),
        }
        // A non-zero read is always progress (loop continues).
        assert!(sftp_strict_short_read_check(1, 0, 4096, 0).is_ok());
        assert!(sftp_strict_short_read_check(4096, 0, 4096, 0).is_ok());
        // Zero read exactly AT the requested length is not an error (the
        // while-loop would already have stopped: filled == want).
        assert!(sftp_strict_short_read_check(0, 4096, 4096, 0).is_ok());
        // Zero read past the length (defensive) is likewise not an error.
        assert!(sftp_strict_short_read_check(0, 5000, 4096, 0).is_ok());
    }

    #[test]
    fn shell_single_quote_neutralises_injection() {
        // Plain path: just wrapped.
        assert_eq!(shell_single_quote("/srv/file.txt"), "'/srv/file.txt'");
        // Spaces stay literal.
        assert_eq!(shell_single_quote("/a b/c"), "'/a b/c'");
        // Embedded single quote: close, escaped quote, reopen.
        assert_eq!(shell_single_quote("a'b"), "'a'\\''b'");
        // Command substitution / backticks are inert inside single quotes.
        assert_eq!(shell_single_quote("$(rm -rf /)"), "'$(rm -rf /)'");
        assert_eq!(shell_single_quote("`id`"), "'`id`'");
        // Separators and logical operators cannot break out.
        assert_eq!(shell_single_quote("x; rm -rf /"), "'x; rm -rf /'");
        assert_eq!(shell_single_quote("a && b"), "'a && b'");
        // Newline injection stays inside the quotes.
        assert_eq!(shell_single_quote("a\nrm -rf /"), "'a\nrm -rf /'");
        // The classic break-out attempt: '; rm -rf / ; echo '
        let evil = "'; rm -rf / ; echo '";
        let q = shell_single_quote(evil);
        assert!(q.starts_with('\'') && q.ends_with('\''));
        // Every original `'` became the 4-char `'\''` sequence; there is no
        // bare unescaped quote that could terminate the literal early.
        assert_eq!(q, "''\\''; rm -rf / ; echo '\\'''");
    }

    #[test]
    fn test_sftp_provider_creation() {
        let config = SftpConfig {
            host: "example.com".to_string(),
            port: 22,
            username: "testuser".to_string(),
            password: Some(secrecy::SecretString::from("testpass".to_string())),
            private_key_path: None,
            key_passphrase: None,
            initial_path: None,
            timeout_secs: 30,
            trust_unknown_hosts: false,
        };

        let provider = SftpProvider::new(config);
        assert_eq!(provider.provider_type(), ProviderType::Sftp);
        assert!(!provider.is_connected());
    }

    #[test]
    fn delta_rsync_config_accepts_password_only_profile() {
        use crate::rsync_over_ssh::AuthMethod;
        use secrecy::ExposeSecret;

        let config = SftpConfig {
            host: "nas.local".to_string(),
            port: 2222,
            username: "alice".to_string(),
            password: Some(secrecy::SecretString::from("secret".to_string())),
            private_key_path: None,
            key_passphrase: None,
            initial_path: None,
            timeout_secs: 30,
            trust_unknown_hosts: false,
        };

        let provider = SftpProvider::new(config);
        let cfg = provider
            .rsync_config_for_delta(Some(std::path::PathBuf::from("/tmp/known_hosts")))
            .expect("password-only profile should be delta-config eligible");

        assert_eq!(cfg.auth_method, AuthMethod::Password);
        assert!(cfg.ssh_key_path.is_none());
        assert_eq!(cfg.ssh_host, "nas.local");
        assert_eq!(cfg.ssh_port, Some(2222));
        assert_eq!(cfg.ssh_password.as_ref().unwrap().expose_secret(), "secret");
        assert!(cfg.validate_auth_material().is_ok());
    }

    #[test]
    fn delta_rsync_config_rejects_empty_password_without_key() {
        let config = SftpConfig {
            host: "nas.local".to_string(),
            port: 22,
            username: "alice".to_string(),
            password: Some(secrecy::SecretString::from(String::new())),
            private_key_path: None,
            key_passphrase: None,
            initial_path: None,
            timeout_secs: 30,
            trust_unknown_hosts: false,
        };

        let provider = SftpProvider::new(config);
        assert!(provider.rsync_config_for_delta(None).is_none());
    }

    #[test]
    fn test_normalize_path() {
        let config = SftpConfig {
            host: "example.com".to_string(),
            port: 22,
            username: "testuser".to_string(),
            password: None,
            private_key_path: None,
            key_passphrase: None,
            initial_path: None,
            timeout_secs: 30,
            trust_unknown_hosts: false,
        };

        let mut provider = SftpProvider::new(config);
        provider.current_dir = "/home/user".to_string();
        provider.home_dir = "/home/user".to_string();

        assert_eq!(provider.normalize_path("/absolute"), "/absolute");
        assert_eq!(provider.normalize_path("relative"), "/home/user/relative");
        assert_eq!(provider.normalize_path(".."), "/home");
        assert_eq!(provider.normalize_path("."), "/home/user");
        assert_eq!(provider.normalize_path("~"), "/home/user");
        assert_eq!(
            provider.normalize_path("~/documents"),
            "/home/user/documents"
        );
    }

    #[test]
    fn test_format_permissions() {
        assert_eq!(format_permissions(0o755, true), "drwxr-xr-x");
        assert_eq!(format_permissions(0o644, false), "-rw-r--r--");
        assert_eq!(format_permissions(0o777, true), "drwxrwxrwx");
        assert_eq!(format_permissions(0o600, false), "-rw-------");
    }

    #[test]
    fn test_symlink_bit_reads_the_readdir_mode() {
        // A mode word carrying file-type bits answers on its own, which is
        // what lets `list` skip one SSH_FXP_LSTAT per directory entry.
        assert_eq!(symlink_bit(0o120777), Some(true)); // symlink
        assert_eq!(symlink_bit(0o100644), Some(false)); // regular file
        assert_eq!(symlink_bit(0o040755), Some(false)); // directory
        assert_eq!(symlink_bit(0o140755), Some(false)); // socket
        assert_eq!(symlink_bit(0o060660), Some(false)); // block device
    }

    #[test]
    fn test_symlink_bit_is_unknown_without_file_type_bits() {
        // Firmware that sends permission bits only must not be read as
        // "not a symlink": the caller has to fall back to an LSTAT probe,
        // otherwise a symlink-to-directory is walked into (GAP-A02).
        assert_eq!(symlink_bit(0o755), None);
        assert_eq!(symlink_bit(0o644), None);
        assert_eq!(symlink_bit(0), None);
    }

    #[test]
    fn test_plan_resume_upload() {
        // Happy path: remote holds a valid prefix, append the tail.
        assert_eq!(
            plan_resume_upload(15, 15, 100),
            ResumeUploadPlan::Append(15)
        );
        // No partial on the remote: full upload from zero.
        assert_eq!(plan_resume_upload(0, 0, 100), ResumeUploadPlan::FullUpload);
        // Caller offset present but remote is empty: clamp to 0 -> full upload
        // (never trust an offset the remote can't back).
        assert_eq!(plan_resume_upload(50, 0, 100), ResumeUploadPlan::FullUpload);
        // Stale caller offset larger than the real remote size: clamp down to
        // the remote size and append from there, never past it.
        assert_eq!(
            plan_resume_upload(90, 40, 100),
            ResumeUploadPlan::Append(40)
        );
        // Remote already has the whole file: nothing to send.
        assert_eq!(
            plan_resume_upload(100, 100, 100),
            ResumeUploadPlan::AlreadyComplete
        );
        // Remote somehow larger than local (stale/other file): treat as complete
        // rather than appending garbage.
        assert_eq!(
            plan_resume_upload(120, 120, 100),
            ResumeUploadPlan::AlreadyComplete
        );
    }
}
