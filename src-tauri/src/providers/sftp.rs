//! SFTP Provider Implementation
//!
//! This module provides SFTP (SSH File Transfer Protocol) support using the russh crate.
//! Supports both password and SSH key-based authentication.
//!
//! Status: v1.3.0

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

use super::{ProviderError, ProviderType, RemoteEntry, SftpConfig, StorageProvider};
use async_trait::async_trait;
use russh::client::AuthResult;
use russh::client::{self, Config, Handle, Handler};
use russh::keys::{self, known_hosts, Algorithm, HashAlg, PrivateKeyWithHashAlg, PublicKey};
use russh::{compression, Preferred};
use russh_sftp::client::SftpSession;
use std::path::Path;
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio::sync::Mutex as TokioMutex;

/// Shared, lock-protected handle to the underlying russh SSH session.
/// Used by sibling modules (e.g. rsync-over-SSH) to open additional channels
/// (exec, direct-tcpip) without re-authenticating.
pub type SharedSshHandle = Arc<TokioMutex<Handle<SshHandler>>>;

/// SSH Client Handler for server key verification.
///
/// Exposed as `pub` because [`SharedSshHandle`] (a public type alias in the same
/// module) names it, and clippy's `exported_private_dependencies` lint requires the
/// visibility levels to match. Callers outside this module don't construct or
/// manipulate it — they only hold the handle and pass it back through APIs that
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
    /// second SSH connection — U-02 closes the MITM hole that
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
    /// — in that case the native path will refuse to enable because
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
                    // SEC-P1-06: Host not in known_hosts — reject here.
                    // Frontend must call sftp_check_host_key + sftp_accept_host_key first.
                    tracing::warn!(
                        "SFTP: Host key for {} not pre-approved via TOFU dialog — rejecting",
                        self.host
                    );
                    Ok(false)
                }
            }
            Err(keys::Error::KeyChanged { line }) => {
                tracing::error!(
                    "SFTP: REJECTING connection to {} - host key changed at known_hosts line {} (possible MITM attack)",
                    self.host, line
                );
                Ok(false)
            }
            Err(e) => {
                // SEC: Reject on unknown errors — do not silently accept.
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
    /// `rsync_proto_serve` does not skip host-key verification.
    host_key_sha256_hex: Arc<std::sync::OnceLock<String>>,
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
            buffer_size: 32768,
            host_key_sha256_hex: Arc::new(std::sync::OnceLock::new()),
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

    /// Return a cloneable handle to the underlying SSH session, if connected.
    ///
    /// Exposed to let sibling modules (rsync-over-SSH, port forwarding, ...)
    /// open additional channels on the same authenticated session. The handle
    /// is protected by a Tokio [`Mutex`](TokioMutex) — callers should hold the
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
    /// - SSH authentication uses a private key path on disk (Fase 1 limits itself
    ///   to key-based auth; password auth falls back to classic transfer)
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
    /// - **Unix + `proto_native_rsync`**: attempts `NativeRsyncDeltaTransport`
    ///   first (if the runtime toggle and host-key pinning allow), otherwise
    ///   falls back to `RsyncBinaryTransport`.
    /// - **Windows + `proto_native_rsync`**: uses the native transport only.
    ///   Without the feature compiled in, this method returns `None` so the
    ///   consumer transparently drops to classic SFTP (same shape the adapter
    ///   already accepts for non-SFTP providers).
    pub fn delta_transport(&self) -> Option<Box<dyn crate::delta_transport::DeltaTransport>> {
        let handle = self.ssh_handle.clone()?;
        let key_path_str = self.config.private_key_path.as_ref()?;
        let key_path = std::path::PathBuf::from(Self::expand_home_path(key_path_str));

        let known_hosts_path = dirs::home_dir().map(|h| h.join(".ssh").join("known_hosts"));

        let rsync_config = crate::rsync_over_ssh::RsyncConfig {
            compress: true,
            preserve_times: true,
            progress: true,
            min_file_size: crate::rsync_over_ssh::DEFAULT_MIN_FILE_SIZE,
            ssh_key_path: Some(key_path),
            ssh_port: Some(self.config.port),
            ssh_user: self.config.username.clone(),
            ssh_host: self.config.host.clone(),
            // Classic SFTP flow already verified the host key via `SshHandler::check_server_key`;
            // rsync's SSH transport can trust that verification for the same session.
            strict_host_key_check: "accept-new".to_string(),
            known_hosts_path,
        };

        #[cfg(feature = "proto_native_rsync")]
        {
            // Runtime toggle - read from settings. When on, attempt
            // NativeRsyncDeltaTransport and fall through to classic
            // binary on any construction error.
            //
            // U-02 security gate: the native path opens its own SSH
            // connection (separate TCP socket, separate libssh2 session)
            // and must not weaken the host-key posture of the parent
            // SFTP session. We only enable the native leg when the
            // classic SFTP flow has already captured the accepted host
            // key's SHA-256 fingerprint. Without a fingerprint we refuse
            // to enable native — the fresh SSH connection would otherwise
            // ride `AcceptAny`, which is a MITM window on a second
            // independent socket.
            if crate::settings::load_native_rsync_enabled() {
                use crate::rsync_native_proto::delta_transport_impl::NativeRsyncDeltaTransport;
                use crate::rsync_native_proto::ssh_transport::SshHostKeyPolicy;

                let host_key_policy = match self.accepted_host_key_sha256_hex() {
                    Some(hex) => SshHostKeyPolicy::pinned_hex(hex),
                    None => {
                        tracing::warn!(
                            "providers::sftp: native rsync disabled for this session — parent \
                             SFTP handshake did not capture a host key fingerprint (possible \
                             password-only auth or early error); falling back to classic"
                        );
                        return classic_binary_fallback(rsync_config, handle);
                    }
                };

                match NativeRsyncDeltaTransport::from_rsync_config(&rsync_config, host_key_policy) {
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
        Some(Box::new(
            crate::delta_transport::RsyncBinaryTransport::new(rsync_config, Some(handle)),
        ))
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

    /// Convert russh-sftp metadata to RemoteEntry
    fn metadata_to_entry(
        &self,
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
        // which is exactly what we want — the session is tore down once for everyone.
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

        let entries = sftp
            .read_dir(&full_path)
            .await
            .map_err(|e| ProviderError::NotFound(format!("Failed to list directory: {}", e)))?;

        let mut result = Vec::new();

        for entry in entries {
            let name = entry.file_name();

            // Skip . and ..
            if name == "." || name == ".." {
                continue;
            }

            let entry_path = if full_path == "/" {
                format!("/{}", name)
            } else {
                format!("{}/{}", full_path.trim_end_matches('/'), name)
            };

            let mut remote_entry =
                self.metadata_to_entry(name.clone(), entry_path.clone(), &entry.metadata());

            // Check if it's a symlink
            if let Ok(link_meta) = sftp.symlink_metadata(&entry_path).await {
                if let Some(perms) = link_meta.permissions {
                    // S_IFLNK = 0o120000
                    if (perms & 0o170000) == 0o120000 {
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
                }
            }

            result.push(remote_entry);
        }

        // Sort: directories first, then alphabetically
        result.sort_by(|a, b| match (a.is_dir, b.is_dir) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
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

        // Verify the directory exists
        let metadata = sftp
            .metadata(&full_path)
            .await
            .map_err(|e| ProviderError::NotFound(format!("Directory not found: {}", e)))?;

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
        let sftp = self.get_sftp()?;
        let full_path = self.normalize_path(remote_path);

        tracing::info!("SFTP: Downloading {} to {}", full_path, local_path);

        // Get file size
        let metadata = sftp
            .metadata(&full_path)
            .await
            .map_err(|e| ProviderError::NotFound(format!("File not found: {}", e)))?;
        let total_size = metadata.size.unwrap_or(0);

        // Open remote file
        let mut remote_file = sftp.open(&full_path).await.map_err(|e| {
            ProviderError::TransferFailed(format!("Failed to open remote file: {}", e))
        })?;

        // Create atomic local file (writes to .aerotmp, renames on commit)
        let mut atomic = super::atomic_write::AtomicFile::new(local_path)
            .await
            .map_err(|e| {
                ProviderError::TransferFailed(format!("Failed to create local file: {}", e))
            })?;

        // Read and write in chunks with optional rate limiting
        let mut buffer = vec![0u8; self.buffer_size];
        let mut transferred: u64 = 0;
        let start = std::time::Instant::now();

        loop {
            let bytes_read = remote_file
                .read(&mut buffer)
                .await
                .map_err(|e| ProviderError::TransferFailed(format!("Read error: {}", e)))?;

            if bytes_read == 0 {
                break;
            }

            atomic
                .write_all(&buffer[..bytes_read])
                .await
                .map_err(|e| ProviderError::TransferFailed(format!("Write error: {}", e)))?;

            transferred += bytes_read as u64;

            if let Some(ref progress) = on_progress {
                progress(transferred, total_size);
            }

            // Apply bandwidth throttling
            if self.download_limit_bps > 0 {
                let expected = std::time::Duration::from_secs_f64(
                    transferred as f64 / self.download_limit_bps as f64,
                );
                let elapsed = start.elapsed();
                if expected > elapsed {
                    tokio::time::sleep(expected - elapsed).await;
                }
            }
        }

        atomic.commit().await.map_err(|e| {
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

        let data = sftp
            .read(&full_path)
            .await
            .map_err(|e| ProviderError::TransferFailed(format!("Failed to read file: {}", e)))?;

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
            ProviderError::TransferFailed(format!("Failed to create remote file: {}", e))
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
                .map_err(|e| ProviderError::TransferFailed(format!("Remote write error: {}", e)))?;

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
            ProviderError::TransferFailed(format!("Failed to flush remote file: {}", e))
        })?;

        // Verify upload size
        if let Err(error) = self
            .verify_remote_upload_size(sftp, &full_path, total_size)
            .await
        {
            tracing::warn!(
                "SFTP: Upload size verification warning for {}: {}",
                full_path,
                error
            );
        }

        // Keep remote mtime aligned with the local source so repeated sync
        // scans don't re-upload unchanged files just because the server stamped
        // the file with upload time.
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
                                if let Err(error) = sftp.set_metadata(&full_path, attrs).await {
                                    tracing::warn!(
                                        "SFTP: Failed to preserve remote mtime for {}: {}",
                                        full_path,
                                        error
                                    );
                                }
                            }
                            Err(_) => tracing::warn!(
                                "SFTP: Skipping mtime preservation for {} because source mtime is out of range",
                                full_path
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

        tracing::info!(
            "SFTP: Upload complete via russh_sftp: {} bytes",
            transferred
        );
        Ok(())
    }

    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let sftp = self.get_sftp()?;
        let full_path = self.normalize_path(path);

        tracing::info!("SFTP: Creating directory: {}", full_path);

        sftp.create_dir(&full_path).await.map_err(|e| {
            ProviderError::ServerError(format!("Failed to create directory: {}", e))
        })?;

        Ok(())
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        let sftp = self.get_sftp()?;
        let full_path = self.normalize_path(path);

        tracing::info!("SFTP: Deleting file: {}", full_path);

        sftp.remove_file(&full_path)
            .await
            .map_err(|e| ProviderError::ServerError(format!("Failed to delete file: {}", e)))?;

        Ok(())
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let sftp = self.get_sftp()?;
        let full_path = self.normalize_path(path);

        tracing::info!("SFTP: Removing directory: {}", full_path);

        sftp.remove_dir(&full_path).await.map_err(|e| {
            ProviderError::ServerError(format!("Failed to remove directory: {}", e))
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

        sftp.rename(&from_path, &to_path)
            .await
            .map_err(|e| ProviderError::ServerError(format!("Failed to rename: {}", e)))?;

        Ok(())
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        let sftp = self.get_sftp()?;
        let full_path = self.normalize_path(path);

        let metadata = sftp
            .metadata(&full_path)
            .await
            .map_err(|e| ProviderError::NotFound(format!("File not found: {}", e)))?;

        let name = Path::new(&full_path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| full_path.clone());

        let mut entry = self.metadata_to_entry(name, full_path.clone(), &metadata);

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

        let metadata = sftp
            .metadata(&full_path)
            .await
            .map_err(|e| ProviderError::NotFound(format!("File not found: {}", e)))?;

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

    async fn keep_alive(&mut self) -> Result<(), ProviderError> {
        // SFTP over SSH is a persistent connection
        // Just check if we're still connected
        if self.sftp.is_none() {
            return Err(ProviderError::NotConnected);
        }

        // Optionally do a simple operation to verify connection
        // canonicalize(".") is lightweight
        if let Some(sftp) = &self.sftp {
            sftp.canonicalize(".")
                .await
                .map_err(|_| ProviderError::NotConnected)?;
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

        sftp.set_metadata(&full_path, attrs)
            .await
            .map_err(|e| ProviderError::ServerError(format!("Failed to chmod: {}", e)))?;

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
                    self.metadata_to_entry(name.clone(), entry_path.clone(), &entry.metadata());

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
            .map_err(|e| ProviderError::ServerError(format!("statvfs failed: {}", e)))?
            .ok_or_else(|| {
                ProviderError::NotSupported("Server does not support statvfs".to_string())
            })?;

        let total = stat.blocks * stat.fragment_size;
        let free = stat.blocks_avail * stat.fragment_size;
        let used = total.saturating_sub(free);

        Ok(super::StorageInfo { used, total, free })
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
        super::TransferOptimizationHints {
            supports_resume_download: false,
            supports_resume_upload: false,
            supports_range_download: true,
            supports_compression: true,
            supports_delta_sync: true,
            ..Default::default()
        }
    }

    fn set_chunk_sizes(&mut self, upload: Option<u64>, download: Option<u64>) {
        // Cap at 16 MB (larger buffers waste memory without improving throughput)
        let cap = 16 * 1024 * 1024;
        if let Some(size) = upload {
            self.buffer_size = (size as usize).clamp(4096, cap);
        }
        if let Some(size) = download {
            self.buffer_size = (size as usize).clamp(4096, cap);
        }
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
        let sftp = self
            .sftp
            .as_ref()
            .ok_or_else(|| ProviderError::NotConnected)?;
        let full_path = self.normalize_path(path);

        let mut file = sftp.open(&full_path).await.map_err(|e| {
            ProviderError::ServerError(format!("Failed to open file for range read: {}", e))
        })?;

        // Seek to offset
        use tokio::io::{AsyncReadExt, AsyncSeekExt};
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(|e| ProviderError::ServerError(format!("Failed to seek: {}", e)))?;

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
            let n = file
                .read(&mut buf[total_read..])
                .await
                .map_err(|e| ProviderError::ServerError(format!("Failed to read range: {}", e)))?;
            if n == 0 {
                break;
            }
            total_read += n;
        }
        buf.truncate(total_read);
        Ok(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
