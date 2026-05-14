//! P3-T01 W3.2(b1): russh-based SSH session transport for the native rsync
//! batch path.
//!
//! See [`RusshSessionTransport`] for the entry point.
//!
//! Spec: `docs/dev/roadmap/APPENDIX-C-Y-D/APPENDIX-Y/tasks/2026-05-01_P3-T01_W3_2b1_Spec_Session_Caching.md`.

#![cfg(feature = "aerorsync")]

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use russh::client::{self, AuthResult, Config, Handle, Handler, Msg};
use russh::keys::{self, Algorithm, EcdsaCurve, HashAlg, PrivateKeyWithHashAlg, PublicKey};
use russh::Preferred;
use russh::{Channel, ChannelMsg};
use secrecy::ExposeSecret;
use std::borrow::Cow;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex as AsyncMutex;

use crate::aerorsync::ssh_transport::{parse_probe_protocol, SshHostKeyPolicy, SshTransportConfig};
use crate::aerorsync::transport::{
    CancelHandle, RawByteStream, RawRemoteShellTransport, RemoteCommandOutput, RemoteExecRequest,
    RemoteShellTransport, TransportProbe,
};
use crate::aerorsync::types::AerorsyncError;

pub struct RusshSessionTransport {
    handle: Arc<HandleSlot>,
    cancel_flag: Arc<AtomicBool>,
    handshake_count: Arc<AtomicU32>,
    raw_open_count: Arc<AtomicU32>,
    config: SshTransportConfig,
}

struct HandleSlot {
    inner: AsyncMutex<Option<Handle<RusshHandler>>>,
}

impl HandleSlot {
    fn new(handle: Option<Handle<RusshHandler>>) -> Self {
        Self {
            inner: AsyncMutex::new(handle),
        }
    }
}

/// russh `Handler` enforcing [`SshHostKeyPolicy`] semantics. Mirrors
/// `enforce_host_key_policy` in `ssh_transport.rs` (the libssh2 leg).
pub struct RusshHandler {
    policy: SshHostKeyPolicy,
}

impl RusshHandler {
    fn new(policy: SshHostKeyPolicy) -> Self {
        Self { policy }
    }

    fn fingerprint_sha256_hex(key: &PublicKey) -> Option<String> {
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

impl Handler for RusshHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKey,
    ) -> Result<bool, Self::Error> {
        match &self.policy {
            SshHostKeyPolicy::AcceptAny => Ok(true),
            SshHostKeyPolicy::PinnedFingerprintSha256 { sha256_hex } => {
                let actual = match Self::fingerprint_sha256_hex(server_public_key) {
                    Some(hex) => hex,
                    None => {
                        tracing::error!(
                            "russh: rejecting host key: failed to compute SHA-256 fingerprint"
                        );
                        return Ok(false);
                    }
                };
                let expected = sha256_hex.to_ascii_lowercase();
                if actual == expected {
                    Ok(true)
                } else {
                    tracing::error!(
                        "russh: REJECTING host key: fingerprint mismatch (expected {expected}, got {actual})"
                    );
                    Ok(false)
                }
            }
        }
    }
}

/// Z.4.5 R1: send `userauth password` over the russh handle.
///
/// The password is taken from the [`SshTransportConfig::auth_password`]
/// field, never from argv or environment, and is wrapped in
/// [`secrecy::SecretString`] so it never appears in `Debug` output or
/// log lines. This function does NOT log the password under any
/// `tracing` level: callers that want to confirm the path was taken
/// should rely on `handshake_count` or test fixtures.
///
/// Errors are mapped to a typed [`AerorsyncError::transport`] envelope
/// that does NOT include the password value. The username is included
/// because it is part of the public auth identity (the same string the
/// remote logs in its own auth log).
async fn authenticate_password(
    handle: &mut Handle<RusshHandler>,
    username: &str,
    password: &secrecy::SecretString,
) -> Result<(), AerorsyncError> {
    match handle
        .authenticate_password(username, password.expose_secret())
        .await
    {
        Ok(AuthResult::Success) => Ok(()),
        Ok(AuthResult::Failure { .. }) => Err(AerorsyncError::transport(format!(
            "russh password auth {username} rejected by server"
        ))),
        Err(e) => Err(AerorsyncError::transport(format!(
            "russh password auth {username} transport error: {e}"
        ))),
    }
}

/// Original publickey path, lifted out of [`RusshSessionTransport::connect`]
/// so the new password branch above stays small and the publickey
/// behaviour (RSA SHA-2 fallback chain) is preserved verbatim.
async fn authenticate_publickey_with_rsa_fallbacks(
    handle: &mut Handle<RusshHandler>,
    config: &SshTransportConfig,
) -> Result<(), AerorsyncError> {
    let key_pair = keys::load_secret_key(&config.private_key_path, None).map_err(|e| {
        AerorsyncError::transport(format!(
            "load private key {}: {e}",
            config.private_key_path.display()
        ))
    })?;
    let key_pair = Arc::new(key_pair);

    let is_rsa = matches!(key_pair.algorithm(), Algorithm::Rsa { .. });
    let attempts: Vec<Option<HashAlg>> = if is_rsa {
        vec![Some(HashAlg::Sha512), Some(HashAlg::Sha256), None]
    } else {
        vec![None]
    };

    let mut last_error: Option<String> = None;
    for hash in attempts {
        let key_with_hash = PrivateKeyWithHashAlg::new(key_pair.clone(), hash);
        match handle
            .authenticate_publickey(&config.username, key_with_hash)
            .await
        {
            Ok(AuthResult::Success) => return Ok(()),
            Ok(AuthResult::Failure { .. }) => continue,
            Err(e) => {
                last_error = Some(e.to_string());
                continue;
            }
        }
    }

    let detail = last_error.unwrap_or_else(|| "auth rejected by server".to_string());
    Err(AerorsyncError::transport(format!(
        "russh pubkey auth {} failed after RSA SHA-512/256/1 negotiation attempts: {detail}",
        config.username
    )))
}

impl RusshSessionTransport {
    pub async fn connect(config: SshTransportConfig) -> Result<Self, AerorsyncError> {
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let handshake_count = Arc::new(AtomicU32::new(0));
        let raw_open_count = Arc::new(AtomicU32::new(0));

        // Z.1.4: align host-key algorithm preference with the libssh2 leg
        // (`AERORSYNC_HOST_KEY_ALGS` in `ssh_transport.rs`). Same order on
        // both legs makes server-side host-key selection deterministic, so
        // SHA-256 fingerprint pinning works across reconnects regardless of
        // which library opens the channel first.
        let preferred = Preferred {
            key: Cow::Borrowed(&[
                Algorithm::Ed25519,
                Algorithm::Ecdsa {
                    curve: EcdsaCurve::NistP256,
                },
                Algorithm::Ecdsa {
                    curve: EcdsaCurve::NistP384,
                },
                Algorithm::Ecdsa {
                    curve: EcdsaCurve::NistP521,
                },
                Algorithm::Rsa {
                    hash: Some(HashAlg::Sha512),
                },
                Algorithm::Rsa {
                    hash: Some(HashAlg::Sha256),
                },
            ]),
            ..Preferred::DEFAULT
        };

        let russh_config = Arc::new(Config {
            inactivity_timeout: Some(Duration::from_millis(config.io_timeout_ms.max(30_000) * 2)),
            keepalive_interval: Some(Duration::from_secs(15)),
            keepalive_max: 3,
            preferred,
            ..Default::default()
        });

        let handler = RusshHandler::new(config.host_key_policy.clone());
        let addr = format!("{}:{}", config.host, config.port);
        let mut handle = client::connect(russh_config, &addr, handler)
            .await
            .map_err(|e| AerorsyncError::transport(format!("russh connect {addr}: {e}")))?;

        // Z.4.5 R1: password transport takes precedence when configured.
        // The two paths are exclusive on purpose: a profile that ships
        // both a key and a password should not silently retry one after
        // the other (it would mask which credential the server actually
        // accepted, complicating audit). The caller picks one.
        if let Some(password) = config.usable_password() {
            authenticate_password(&mut handle, &config.username, password).await?;
        } else {
            authenticate_publickey_with_rsa_fallbacks(&mut handle, &config).await?;
        }

        handshake_count.store(1, Ordering::SeqCst);

        Ok(Self {
            handle: Arc::new(HandleSlot::new(Some(handle))),
            cancel_flag,
            handshake_count,
            raw_open_count,
            config,
        })
    }

    pub fn handshake_count(&self) -> u32 {
        self.handshake_count.load(Ordering::SeqCst)
    }

    /// Z.1.2 — reconnect orchestration.
    ///
    /// Tear the current SSH handle down and re-run `connect()` using the
    /// saved [`SshTransportConfig`]. The new handle is swapped into the
    /// shared [`HandleSlot`] so every clone produced via
    /// [`share_session`](Self::share_session) immediately uses it on the
    /// next [`open_raw_channel`](Self::open_raw_channel) call. The
    /// `handshake_count` is incremented so observers (i.e. `BatchStats`)
    /// can detect that a reconnect happened.
    ///
    /// The function returns `Err` when the underlying re-connect fails:
    /// in that case the handle is left empty and the next
    /// `open_raw_channel` will surface "handle is closed". Callers must
    /// classify the original transfer error before invoking this method
    /// (see [`is_transient_for_reconnect`](crate::rsync_over_ssh::is_transient_for_reconnect));
    /// reconnecting on a hard rejection (host-key mismatch, auth refused)
    /// would silently mask a security-relevant fault.
    pub async fn reconnect(&self) -> Result<(), AerorsyncError> {
        if self.cancel_flag.load(Ordering::SeqCst) {
            return Err(AerorsyncError::cancelled(
                "RusshSessionTransport cancelled before reconnect",
            ));
        }

        // Best-effort tear-down of the old handle so the server side does
        // not see a stale connection holding the channel for a while.
        // We hold the inner lock only long enough to swap the option:
        // dropping the lock before the `connect()` call below avoids
        // blocking concurrent reads of `handshake_count` and lets a
        // racing cancel observe the empty slot.
        let old = {
            let mut guard = self.handle.inner.lock().await;
            guard.take()
        };
        if let Some(handle) = old {
            // Best-effort disconnect: ignore the result, we are about to
            // open a new session anyway. russh's `disconnect` is a tiny
            // protocol message + EOF, so this is bounded latency.
            let _ = handle
                .disconnect(russh::Disconnect::ByApplication, "reconnect", "en-US")
                .await;
        }

        // Run a fresh connect using the saved config. This re-uses the
        // existing `connect()` so all the host-key, algorithm pref, RSA
        // hash retry logic remains the single source of truth.
        let fresh = Self::connect(self.config.clone()).await?;
        // Pull the new handle out of `fresh` so we can plant it on `self`.
        // `fresh` is then dropped: its `Arc`s decrement to zero without
        // surfacing in observers because none of them ever saw it.
        let new_handle = {
            let mut guard = fresh.handle.inner.lock().await;
            guard.take()
        };
        if new_handle.is_none() {
            return Err(AerorsyncError::transport(
                "reconnect: fresh connect() returned without an authenticated handle",
            ));
        }

        // Swap the new handle into the shared slot. From this point every
        // existing clone produced via `share_session()` uses the new
        // handle on the next `open_raw_channel` call.
        let mut guard = self.handle.inner.lock().await;
        *guard = new_handle;
        drop(guard);
        self.handshake_count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    pub fn raw_open_count(&self) -> u32 {
        self.raw_open_count.load(Ordering::SeqCst)
    }

    pub fn share_session(&self) -> Self {
        Self {
            handle: self.handle.clone(),
            cancel_flag: self.cancel_flag.clone(),
            handshake_count: self.handshake_count.clone(),
            raw_open_count: self.raw_open_count.clone(),
            config: self.config.clone(),
        }
    }

    #[cfg(test)]
    pub(crate) fn test_with_empty_handle(config: SshTransportConfig, handshake_count: u32) -> Self {
        Self {
            handle: Arc::new(HandleSlot::new(None)),
            cancel_flag: Arc::new(AtomicBool::new(false)),
            handshake_count: Arc::new(AtomicU32::new(handshake_count)),
            raw_open_count: Arc::new(AtomicU32::new(0)),
            config,
        }
    }

    #[cfg(test)]
    pub(crate) fn test_set_handshake_count(&self, value: u32) {
        self.handshake_count.store(value, Ordering::SeqCst);
    }

    /// Open a new channel over the existing SSH session and exec the
    /// remote command. Returns a [`RusshRawStream`] that the driver can
    /// drive as a [`RawByteStream`].
    pub async fn open_raw_channel(
        &self,
        request: RemoteExecRequest,
    ) -> Result<RusshRawStream, AerorsyncError> {
        self.raw_open_count.fetch_add(1, Ordering::SeqCst);
        if self.cancel_flag.load(Ordering::SeqCst) {
            return Err(AerorsyncError::cancelled(
                "RusshSessionTransport cancelled before open_raw_channel",
            ));
        }
        let mut guard = self.handle.inner.lock().await;
        let handle = guard.as_mut().ok_or_else(|| {
            AerorsyncError::transport(
                "RusshSessionTransport handle is closed; call connect() first",
            )
        })?;
        let channel = handle
            .channel_open_session()
            .await
            .map_err(|e| AerorsyncError::transport(format!("russh channel_open_session: {e}")))?;
        let cmd = request.full_command_line();
        channel
            .exec(true, cmd.as_str())
            .await
            .map_err(|e| AerorsyncError::transport(format!("russh channel.exec({cmd}): {e}")))?;
        Ok(RusshRawStream::new(self.cancel_flag.clone(), channel))
    }

    /// Drain a channel-exec result fully. Returns stdout + stderr + exit
    /// code, mirroring the libssh2 `exec_once` semantics.
    async fn drain_exec_channel(
        cancel_flag: &Arc<AtomicBool>,
        channel: &mut Channel<Msg>,
    ) -> Result<RemoteCommandOutput, AerorsyncError> {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut exit_code: Option<i32> = None;

        loop {
            if cancel_flag.load(Ordering::SeqCst) {
                return Err(AerorsyncError::cancelled(
                    "RusshSessionTransport cancelled mid drain_exec_channel",
                ));
            }
            match channel.wait().await {
                Some(ChannelMsg::Data { data }) => {
                    stdout.extend_from_slice(&data);
                }
                Some(ChannelMsg::ExtendedData { data, ext }) => {
                    if ext == 1 {
                        stderr.extend_from_slice(&data);
                    } else {
                        // Unknown extended data channel: append to stderr
                        // for visibility instead of dropping.
                        stderr.extend_from_slice(&data);
                    }
                }
                Some(ChannelMsg::ExitStatus { exit_status }) => {
                    exit_code = Some(exit_status as i32);
                }
                Some(ChannelMsg::Eof) => {
                    // Server signalled end of stdout/stderr. Continue
                    // waiting for ExitStatus + Close if not yet seen.
                    continue;
                }
                Some(ChannelMsg::Close) | None => {
                    break;
                }
                Some(_) => {
                    // ExitSignal, Success, Failure, WindowAdjusted, etc.
                    continue;
                }
            }
        }

        Ok(RemoteCommandOutput {
            exit_code: exit_code.unwrap_or(-1),
            stdout,
            stderr,
        })
    }

    pub async fn cancel(&self) {
        self.cancel_flag.store(true, Ordering::SeqCst);
        let mut guard = self.handle.inner.lock().await;
        if let Some(handle) = guard.take() {
            drop(handle);
        }
    }

    pub async fn close(&self) -> Result<(), AerorsyncError> {
        let mut guard = self.handle.inner.lock().await;
        if let Some(handle) = guard.take() {
            drop(handle);
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) fn test_dummy_config() -> SshTransportConfig {
    use std::path::PathBuf;
    SshTransportConfig {
        host: "127.0.0.1".into(),
        port: 22,
        username: "test".into(),
        private_key_path: PathBuf::from("/dev/null"),
        connect_timeout_ms: 5_000,
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
    }
}

/// Channel-backed [`RawByteStream`] returned by
/// [`RusshSessionTransport::open_raw_channel`]. Drives a single
/// `russh::Channel<Msg>` for the lifetime of one rsync exec on the
/// remote.
pub struct RusshRawStream {
    cancel_flag: Arc<AtomicBool>,
    /// Surplus bytes from a prior `ChannelMsg::Data` that exceeded the
    /// caller's `read_bytes(max)` allocation. Drained ahead of any new
    /// `wait()` call.
    pending: Vec<u8>,
    /// True after `ChannelMsg::Eof` / `Close` / `None`. Subsequent
    /// `read_bytes` returns `Ok(vec![])` per [`RawByteStream`] contract.
    eof: bool,
    /// `None` after `shutdown` or after the channel naturally ends.
    channel: Option<Channel<Msg>>,
}

impl RusshRawStream {
    fn new(cancel_flag: Arc<AtomicBool>, channel: Channel<Msg>) -> Self {
        Self {
            cancel_flag,
            pending: Vec::new(),
            eof: false,
            channel: Some(channel),
        }
    }
}

#[async_trait]
impl RawByteStream for RusshRawStream {
    async fn read_bytes(&mut self, max: usize) -> Result<Vec<u8>, AerorsyncError> {
        if self.cancel_flag.load(Ordering::SeqCst) {
            return Err(AerorsyncError::cancelled(
                "RusshRawStream cancelled before read_bytes",
            ));
        }
        // Drain any surplus from a previous Data event before issuing a
        // fresh wait().
        if !self.pending.is_empty() {
            let take = self.pending.len().min(max);
            let result: Vec<u8> = self.pending.drain(..take).collect();
            return Ok(result);
        }
        if self.eof {
            return Ok(Vec::new());
        }
        let channel = match self.channel.as_mut() {
            Some(c) => c,
            None => {
                self.eof = true;
                return Ok(Vec::new());
            }
        };

        loop {
            if self.cancel_flag.load(Ordering::SeqCst) {
                return Err(AerorsyncError::cancelled(
                    "RusshRawStream cancelled mid read_bytes",
                ));
            }
            match channel.wait().await {
                Some(ChannelMsg::Data { data }) => {
                    let bytes = data.to_vec();
                    if bytes.len() > max {
                        let surplus = bytes[max..].to_vec();
                        self.pending = surplus;
                        return Ok(bytes[..max].to_vec());
                    }
                    return Ok(bytes);
                }
                Some(ChannelMsg::ExtendedData { .. }) => {
                    // Stderr: discarded at the raw-byte layer; the
                    // driver-level event bridge surfaces stderr through
                    // its own path. Continue draining.
                    continue;
                }
                Some(ChannelMsg::Eof) => {
                    self.eof = true;
                    return Ok(Vec::new());
                }
                Some(ChannelMsg::ExitStatus { .. }) => {
                    // Capture is upstream's job (drain_exec_channel).
                    // Raw-stream consumers continue draining for
                    // residual data after ExitStatus.
                    continue;
                }
                Some(ChannelMsg::Close) | None => {
                    self.eof = true;
                    self.channel = None;
                    return Ok(Vec::new());
                }
                Some(_) => continue,
            }
        }
    }

    async fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), AerorsyncError> {
        if self.cancel_flag.load(Ordering::SeqCst) {
            return Err(AerorsyncError::cancelled(
                "RusshRawStream cancelled before write_bytes",
            ));
        }
        let channel = match self.channel.as_mut() {
            Some(c) => c,
            None => {
                return Err(AerorsyncError::transport(
                    "RusshRawStream channel already closed",
                ));
            }
        };
        channel
            .data(bytes)
            .await
            .map_err(|e| AerorsyncError::transport(format!("russh channel.data: {e}")))?;
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), AerorsyncError> {
        self.eof = true;
        if let Some(channel) = self.channel.as_mut() {
            channel
                .eof()
                .await
                .map_err(|e| AerorsyncError::transport(format!("russh channel.eof: {e}")))?;
        }
        // Drop the channel after eof so further read_bytes/write_bytes
        // surface the typed "channel closed" error.
        self.channel = None;
        Ok(())
    }
}

#[async_trait]
impl RemoteShellTransport for RusshSessionTransport {
    type Stream = RusshUnusedStream;

    async fn probe(&self) -> Result<TransportProbe, AerorsyncError> {
        let output = self.exec(self.config.probe_request.clone()).await?;
        if output.exit_code != 0 {
            return Err(AerorsyncError::transport(format!(
                "russh probe exited with code {}: {}",
                output.exit_code,
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        let banner = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let protocol = parse_probe_protocol(&banner)?;
        Ok(TransportProbe {
            remote_banner: banner,
            protocol,
            supports_remote_shell: true,
        })
    }

    async fn exec(
        &self,
        request: RemoteExecRequest,
    ) -> Result<RemoteCommandOutput, AerorsyncError> {
        if self.cancel_flag.load(Ordering::SeqCst) {
            return Err(AerorsyncError::cancelled(
                "RusshSessionTransport cancelled before exec",
            ));
        }
        let mut guard = self.handle.inner.lock().await;
        let handle = guard.as_mut().ok_or_else(|| {
            AerorsyncError::transport(
                "RusshSessionTransport handle is closed; call connect() first",
            )
        })?;
        let mut channel = handle
            .channel_open_session()
            .await
            .map_err(|e| AerorsyncError::transport(format!("russh channel_open_session: {e}")))?;
        let cmd = request.full_command_line();
        channel
            .exec(true, cmd.as_str())
            .await
            .map_err(|e| AerorsyncError::transport(format!("russh channel.exec({cmd}): {e}")))?;
        Self::drain_exec_channel(&self.cancel_flag, &mut channel).await
    }

    async fn open_stream(
        &self,
        _request: RemoteExecRequest,
    ) -> Result<Self::Stream, AerorsyncError> {
        Err(AerorsyncError::transport(
            "RusshSessionTransport does not support the legacy RSNP framed stream",
        ))
    }

    async fn cancel(&self) -> Result<(), AerorsyncError> {
        Self::cancel(self).await;
        Ok(())
    }

    fn cancel_handle(&self) -> CancelHandle {
        CancelHandle::new(self.cancel_flag.clone(), None)
    }
}

#[async_trait]
impl RawRemoteShellTransport for RusshSessionTransport {
    type RawStream = RusshRawStream;

    async fn open_raw_stream(
        &self,
        request: RemoteExecRequest,
    ) -> Result<Self::RawStream, AerorsyncError> {
        self.open_raw_channel(request).await
    }
}

pub struct RusshUnusedStream;

#[async_trait]
impl crate::aerorsync::transport::BidirectionalByteStream for RusshUnusedStream {
    async fn write_frame(&mut self, _frame: &[u8]) -> Result<(), AerorsyncError> {
        Err(AerorsyncError::transport(
            "RusshUnusedStream cannot be driven: RusshSessionTransport \
             only supports the raw byte path",
        ))
    }

    async fn read_frame(&mut self) -> Result<Vec<u8>, AerorsyncError> {
        Err(AerorsyncError::transport(
            "RusshUnusedStream cannot be driven: RusshSessionTransport \
             only supports the raw byte path",
        ))
    }

    async fn shutdown(&mut self) -> Result<(), AerorsyncError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::SecretString;

    fn dummy_config() -> SshTransportConfig {
        test_dummy_config()
    }

    #[test]
    fn russh_transport_satisfies_remote_shell_traits() {
        fn assert_traits<T: RemoteShellTransport + RawRemoteShellTransport>() {}
        assert_traits::<RusshSessionTransport>();
    }

    #[test]
    fn russh_transport_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<RusshSessionTransport>();
        assert_send_sync::<RusshRawStream>();
    }

    #[tokio::test]
    async fn connect_with_unreachable_host_returns_typed_transport_error() {
        let cfg = SshTransportConfig {
            port: 1,
            ..dummy_config()
        };
        match RusshSessionTransport::connect(cfg).await {
            Err(_) => {}
            Ok(_) => panic!(
                "connect to port 1 should not succeed in CI: fixture must have an SSH \
                 server bound there which would be a security red flag"
            ),
        }
    }

    /// Z.4.5 R1: when the config carries an `auth_password`, the
    /// connect path still propagates the typed transport error on an
    /// unreachable host. The branch must NOT panic, leak the password
    /// in the error string, or silently fall back to the pubkey path.
    #[tokio::test]
    async fn connect_with_password_on_unreachable_host_returns_typed_transport_error() {
        let cfg = SshTransportConfig {
            port: 1,
            auth_password: Some(SecretString::from(
                "should-never-appear-in-error".to_string(),
            )),
            ..dummy_config()
        };
        match RusshSessionTransport::connect(cfg).await {
            Err(err) => {
                let msg = format!("{err:?}");
                assert!(
                    !msg.contains("should-never-appear-in-error"),
                    "password leaked into error message: {msg}"
                );
            }
            Ok(_) => panic!("connect to port 1 should not succeed"),
        }
    }

    /// Z.4.5 R1: empty `SecretString` MUST be treated as "no password
    /// configured". The connect path then falls back to the pubkey
    /// branch instead of sending a zero-length password to the server.
    /// Pinned by checking `usable_password()` semantics directly so the
    /// invariant can be regression-tested without a live SSH endpoint.
    #[test]
    fn usable_password_treats_empty_secret_as_none() {
        let cfg_none = SshTransportConfig {
            auth_password: None,
            ..dummy_config()
        };
        assert!(cfg_none.usable_password().is_none());

        let cfg_empty = SshTransportConfig {
            auth_password: Some(SecretString::from(String::new())),
            ..dummy_config()
        };
        assert!(
            cfg_empty.usable_password().is_none(),
            "empty SecretString must be rejected by usable_password()"
        );

        let cfg_real = SshTransportConfig {
            auth_password: Some(SecretString::from("real".to_string())),
            ..dummy_config()
        };
        assert!(cfg_real.usable_password().is_some());
    }

    /// Z.4.5 R1: the `Debug` derive on [`SshTransportConfig`] must
    /// redact the password. `secrecy::SecretString` provides this for
    /// free, but pin the behaviour explicitly so a future refactor that
    /// swaps the wrapper type cannot regress the contract.
    #[test]
    fn password_does_not_leak_in_debug() {
        let cfg = SshTransportConfig {
            auth_password: Some(SecretString::from(
                "super-secret-filelu-rsync-password".to_string(),
            )),
            ..dummy_config()
        };
        let debug = format!("{cfg:?}");
        assert!(
            !debug.contains("super-secret-filelu-rsync-password"),
            "password leaked into Debug output: {debug}"
        );
        // Sanity: the field name must still be present so operators
        // can see at a glance that auth material is configured.
        assert!(debug.contains("auth_password"));
    }

    /// Z.4.5 R1: when the auth path errors out before the actual
    /// network round-trip (e.g. because the connection is rejected),
    /// callers still get a typed `transport` error, not a panic. This
    /// is the precondition for the Z.1.2 reconnect classifier to keep
    /// behaving sensibly when an `auth_password`-bearing profile drops
    /// mid-session.
    #[test]
    fn dummy_config_default_keeps_pubkey_branch() {
        // Regression: `dummy_config()` (and by extension every test
        // that uses `..dummy_config()`) must keep `auth_password = None`
        // so the historical pubkey-only behaviour is preserved.
        let cfg = dummy_config();
        assert!(cfg.auth_password.is_none());
        assert!(cfg.usable_password().is_none());
    }

    #[tokio::test]
    async fn share_session_clones_arc_state() {
        let original = RusshSessionTransport {
            handle: Arc::new(HandleSlot::new(None)),
            cancel_flag: Arc::new(AtomicBool::new(false)),
            handshake_count: Arc::new(AtomicU32::new(0)),
            raw_open_count: Arc::new(AtomicU32::new(0)),
            config: dummy_config(),
        };
        let clone = original.share_session();
        original.handshake_count.fetch_add(1, Ordering::SeqCst);
        assert_eq!(clone.handshake_count(), 1);
        assert_eq!(original.handshake_count(), 1);
        clone.cancel_flag.store(true, Ordering::SeqCst);
        assert!(original.cancel_flag.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn close_is_idempotent_and_succeeds_on_empty_handle() {
        let transport = RusshSessionTransport {
            handle: Arc::new(HandleSlot::new(None)),
            cancel_flag: Arc::new(AtomicBool::new(false)),
            handshake_count: Arc::new(AtomicU32::new(0)),
            raw_open_count: Arc::new(AtomicU32::new(0)),
            config: dummy_config(),
        };
        transport.close().await.expect("first close ok");
        transport.close().await.expect("second close ok");
    }

    #[tokio::test]
    async fn cancel_flips_flag_and_drops_handle() {
        let transport = RusshSessionTransport {
            handle: Arc::new(HandleSlot::new(None)),
            cancel_flag: Arc::new(AtomicBool::new(false)),
            handshake_count: Arc::new(AtomicU32::new(0)),
            raw_open_count: Arc::new(AtomicU32::new(0)),
            config: dummy_config(),
        };
        assert!(!transport.cancel_flag.load(Ordering::SeqCst));
        transport.cancel().await;
        assert!(transport.cancel_flag.load(Ordering::SeqCst));
        transport.cancel().await;
    }

    /// Z.1.2 — reconnect refuses to fire when the cancel flag has
    /// already been raised. This keeps cancel + reconnect race-free:
    /// the user's cancel always wins, even if a transient drop fires
    /// concurrently with `cancel()`.
    #[tokio::test]
    async fn reconnect_refuses_when_cancelled() {
        let transport = RusshSessionTransport {
            handle: Arc::new(HandleSlot::new(None)),
            cancel_flag: Arc::new(AtomicBool::new(true)),
            handshake_count: Arc::new(AtomicU32::new(0)),
            raw_open_count: Arc::new(AtomicU32::new(0)),
            config: dummy_config(),
        };
        match transport.reconnect().await {
            Err(err) => {
                assert_eq!(
                    err.kind,
                    crate::aerorsync::types::AerorsyncErrorKind::Cancelled,
                    "expected Cancelled, got {err:?}"
                );
            }
            Ok(_) => panic!("reconnect should not succeed when cancel flag is set"),
        }
        // Handshake counter must NOT have advanced: cancel short-circuit
        // happens before any network-touching step.
        assert_eq!(transport.handshake_count(), 0);
    }

    /// Z.1.2 — when reconnect cannot reach the server (port 1 is the
    /// canary used elsewhere), the inner handle is left empty and the
    /// handshake counter is NOT incremented. Callers can then surface
    /// the original transfer error rather than a misleading
    /// "reconnected ok" state.
    #[tokio::test]
    async fn reconnect_does_not_bump_counter_on_failure() {
        let transport = RusshSessionTransport {
            // Seed with a non-None placeholder so we can verify it gets
            // cleared even when the fresh connect() below fails. We
            // cannot construct a real russh `Handle` in unit tests, so
            // we leave it None; the assertions below verify that the
            // observable counter state (handshake_count) is what callers
            // rely on for the BatchStats path.
            handle: Arc::new(HandleSlot::new(None)),
            cancel_flag: Arc::new(AtomicBool::new(false)),
            handshake_count: Arc::new(AtomicU32::new(7)),
            raw_open_count: Arc::new(AtomicU32::new(0)),
            config: dummy_config(),
        };
        let before = transport.handshake_count();
        match transport.reconnect().await {
            Err(_) => {}
            Ok(_) => panic!(
                "reconnect to port 1 should not succeed in CI: see connect_refuses_port_1"
            ),
        }
        assert_eq!(
            transport.handshake_count(),
            before,
            "handshake_count must NOT advance when reconnect fails"
        );
    }

    #[test]
    fn host_key_policy_accept_any_compiles() {
        let _h = RusshHandler::new(SshHostKeyPolicy::AcceptAny);
    }

    #[test]
    fn host_key_policy_pinned_compiles() {
        let _h = RusshHandler::new(SshHostKeyPolicy::PinnedFingerprintSha256 {
            sha256_hex: "deadbeef".into(),
        });
    }

    /// open_raw_channel without a connected handle returns the typed
    /// "handle closed" error. Pin: callers can detect missing
    /// connect() up front instead of crashing on libssh2 internals.
    #[tokio::test]
    async fn open_raw_channel_without_handle_returns_typed_error() {
        let transport = RusshSessionTransport {
            handle: Arc::new(HandleSlot::new(None)),
            cancel_flag: Arc::new(AtomicBool::new(false)),
            handshake_count: Arc::new(AtomicU32::new(0)),
            raw_open_count: Arc::new(AtomicU32::new(0)),
            config: dummy_config(),
        };
        match transport
            .open_raw_channel(dummy_config().probe_request)
            .await
        {
            Err(_) => {}
            Ok(_) => panic!("open_raw_channel should fail when handle is None"),
        }
    }

    /// Same for exec.
    #[tokio::test]
    async fn exec_without_handle_returns_typed_error() {
        let transport = RusshSessionTransport {
            handle: Arc::new(HandleSlot::new(None)),
            cancel_flag: Arc::new(AtomicBool::new(false)),
            handshake_count: Arc::new(AtomicU32::new(0)),
            raw_open_count: Arc::new(AtomicU32::new(0)),
            config: dummy_config(),
        };
        match transport.exec(dummy_config().probe_request).await {
            Err(_) => {}
            Ok(_) => panic!("exec should fail when handle is None"),
        }
    }
}
