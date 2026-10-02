//! Private outbound STDIO supervisor. No app command or model tool reaches it.
//! Activation requires a separate backend authorization and isolation gate.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use std::collections::VecDeque;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Map, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::task::JoinHandle;
use tokio::time;
use tokio_util::sync::CancellationToken;

use crate::mcp_client_config::{McpServerConfig, ResolvedMcpEnvironment};
use crate::mcp_client_framing::{FrameError, McpLineDecoder, MAX_MCP_FRAME_BYTES};
use crate::mcp_client_protocol::{self as protocol, Era, ProbeReply, ProbeVerdict, ProtocolError};
use crate::mcp_client_sandbox::{self, SandboxError};

const CLIENT_NAME: &str = "AeroFTP";
const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransportError {
    InvalidConfig,
    Spawn,
    SandboxUnavailable,
    Io,
    Frame(FrameError),
    Protocol(ProtocolError),
    Timeout,
    Cancelled,
    Eof,
    TooLarge,
    Closed,
    RestartExhausted,
}

impl From<ProtocolError> for TransportError {
    fn from(error: ProtocolError) -> Self {
        Self::Protocol(error)
    }
}

impl From<FrameError> for TransportError {
    fn from(error: FrameError) -> Self {
        Self::Frame(error)
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Limits {
    pub request: Duration,
    pub shutdown: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            request: Duration::from_secs(10),
            shutdown: Duration::from_secs(2),
        }
    }
}

struct Peer {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: ChildStdout,
    stderr_drain: Option<JoinHandle<usize>>,
    decoder: McpLineDecoder,
    queued: VecDeque<Value>,
}

impl Peer {
    fn spawn(
        config: &McpServerConfig,
        env: &ResolvedMcpEnvironment,
    ) -> Result<Self, TransportError> {
        config
            .validate()
            .map_err(|_| TransportError::InvalidConfig)?;
        if !config.enabled {
            return Err(TransportError::InvalidConfig);
        }
        if !config.env.keys().eq(env.vars.keys()) {
            return Err(TransportError::InvalidConfig);
        }
        let mut command =
            mcp_client_sandbox::peer_command(config).map_err(|error| match error {
                SandboxError::Unavailable => TransportError::SandboxUnavailable,
                SandboxError::InvalidPath => TransportError::InvalidConfig,
            })?;
        mcp_client_sandbox::clear_peer_environment(&mut command);
        #[cfg(target_os = "linux")]
        command
            .env("HOME", "/tmp")
            .env("XDG_CONFIG_HOME", "/tmp/.config")
            .env("XDG_DATA_HOME", "/tmp/.local/share")
            .env("TMPDIR", "/tmp");
        for (name, secret) in &env.vars {
            command.env(name, secret.as_str());
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| TransportError::Spawn)?;
        let stdin = child.stdin.take().ok_or(TransportError::Spawn)?;
        let stdout = child.stdout.take().ok_or(TransportError::Spawn)?;
        let mut stderr = child.stderr.take().ok_or(TransportError::Spawn)?;
        // Never retain peer text, even when the pipe contains secrets or floods.
        let stderr_drain = tokio::spawn(async move {
            let mut bytes = 0usize;
            let mut chunk = [0u8; 8192];
            while let Ok(count) = stderr.read(&mut chunk).await {
                if count == 0 {
                    break;
                }
                bytes = bytes.saturating_add(count);
            }
            bytes
        });
        Ok(Self {
            child,
            stdin: Some(stdin),
            stdout,
            stderr_drain: Some(stderr_drain),
            decoder: McpLineDecoder::default(),
            queued: VecDeque::new(),
        })
    }

    async fn send(&mut self, message: &Value) -> Result<(), TransportError> {
        let mut bytes = serde_json::to_vec(message).map_err(|_| TransportError::Io)?;
        if bytes.len().saturating_add(1) > MAX_MCP_FRAME_BYTES {
            return Err(TransportError::TooLarge);
        }
        bytes.push(b'\n');
        self.stdin
            .as_mut()
            .ok_or(TransportError::Closed)?
            .write_all(&bytes)
            .await
            .map_err(|_| TransportError::Io)
    }

    async fn receive(&mut self) -> Result<Value, TransportError> {
        if let Some(message) = self.queued.pop_front() {
            return Ok(message);
        }
        let mut chunk = [0u8; 8192];
        loop {
            let count = self
                .stdout
                .read(&mut chunk)
                .await
                .map_err(|_| TransportError::Io)?;
            if count == 0 {
                std::mem::take(&mut self.decoder).finish()?;
                return Err(TransportError::Eof);
            }
            self.queued.extend(self.decoder.push(&chunk[..count])?);
            if let Some(message) = self.queued.pop_front() {
                return Ok(message);
            }
        }
    }

    fn ensure_no_partial_frame(&mut self) -> Result<(), TransportError> {
        std::mem::take(&mut self.decoder)
            .finish()
            .map_err(Into::into)
    }

    async fn shutdown(&mut self, timeout: Duration) -> bool {
        self.stdin.take(); // EOF before escalation.
        let graceful = matches!(time::timeout(timeout, self.child.wait()).await, Ok(Ok(_)));
        if !graceful {
            let _ = time::timeout(timeout, self.child.kill()).await;
            let _ = time::timeout(timeout, self.child.wait()).await;
        }
        if let Some(mut drain) = self.stderr_drain.take() {
            if time::timeout(timeout, &mut drain).await.is_err() {
                drain.abort();
            }
        }
        graceful
    }
}

async fn bounded<T>(
    timeout: Duration,
    cancel: &CancellationToken,
    operation: impl std::future::Future<Output = Result<T, TransportError>>,
) -> Result<T, TransportError> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(TransportError::Cancelled),
        result = time::timeout(timeout, operation) => result.map_err(|_| TransportError::Timeout)?,
    }
}

async fn start_session(
    config: &McpServerConfig,
    env: &ResolvedMcpEnvironment,
    era: Era,
    limits: Limits,
    cancel: &CancellationToken,
) -> Result<(Peer, Era), TransportError> {
    let mut peer = Peer::spawn(config, env)?;
    let result = async {
        let mut negotiated = era;
        if matches!(era, Era::Legacy(_)) {
            let request = protocol::initialize_request(2, CLIENT_NAME, CLIENT_VERSION)?;
            bounded(limits.request, cancel, peer.send(&request)).await?;
            let reply = bounded(limits.request, cancel, peer.receive()).await?;
            negotiated = protocol::accept_legacy_initialize(&reply, 2)?;
            bounded(
                limits.request,
                cancel,
                peer.send(&protocol::initialized_notification()),
            )
            .await?;
        }
        Ok::<Era, TransportError>(negotiated)
    }
    .await;
    match result {
        Ok(negotiated) => Ok((peer, negotiated)),
        Err(error) => {
            peer.shutdown(limits.shutdown).await;
            Err(error)
        }
    }
}

/// Private until MCLIENT-05C2 adds authorization and an OS isolation decision.
pub(crate) struct StdioSupervisor {
    config: McpServerConfig,
    env: ResolvedMcpEnvironment,
    peer: Option<Peer>,
    era: Era,
    next_id: u64,
    restarts: u8,
    restart_enabled: bool,
    limits: Limits,
}

impl StdioSupervisor {
    #[cfg(test)]
    pub(crate) async fn connect(
        config: McpServerConfig,
        env: ResolvedMcpEnvironment,
        limits: Limits,
        cancel: &CancellationToken,
    ) -> Result<Self, TransportError> {
        let mut peer =
            Self::connect_checked(config, env, limits, cancel, || Ok::<(), TransportError>(()))
                .await?;
        // Only transport-only fixtures exercise the generic restart contract.
        peer.restart_enabled = true;
        Ok(peer)
    }

    pub(crate) async fn connect_checked<F, E>(
        config: McpServerConfig,
        env: ResolvedMcpEnvironment,
        limits: Limits,
        cancel: &CancellationToken,
        mut fresh: F,
    ) -> Result<Self, E>
    where
        F: FnMut() -> Result<(), E>,
        E: From<TransportError>,
    {
        fresh()?;
        if cancel.is_cancelled() {
            return Err(TransportError::Cancelled.into());
        }
        let mut probe = Peer::spawn(&config, &env)?;
        let verdict = async {
            let request = protocol::discover_request(1, CLIENT_NAME, CLIENT_VERSION)?;
            bounded(limits.request, cancel, probe.send(&request)).await?;
            let observed = match bounded(limits.request, cancel, probe.receive()).await {
                Ok(message) => protocol::classify_probe(ProbeReply::Message(&message), 1)?,
                Err(TransportError::Eof) => {
                    if !matches!(
                        time::timeout(limits.shutdown, probe.child.wait()).await,
                        Ok(Ok(_))
                    ) {
                        return Err(TransportError::Eof);
                    }
                    protocol::classify_probe(ProbeReply::ProbeChildExited, 1)?
                }
                Err(TransportError::Timeout) => {
                    probe.ensure_no_partial_frame()?;
                    protocol::classify_probe(ProbeReply::SilentProbeTimeout, 1)?
                }
                Err(error) => return Err(error),
            };
            Ok::<_, TransportError>(observed)
        }
        .await;
        probe.shutdown(limits.shutdown).await;
        let era = match verdict? {
            ProbeVerdict::Modern => Era::Modern,
            ProbeVerdict::LegacyHandshakeRequired => Era::Legacy(protocol::LEGACY_PREFERRED),
        };
        // Discovery and probe shutdown await external work. Do not launch the
        // real session with secrets/config retained before those awaits.
        fresh()?;
        if cancel.is_cancelled() {
            return Err(TransportError::Cancelled.into());
        }
        let (peer, era) = start_session(&config, &env, era, limits, cancel).await?;
        Ok(Self {
            config,
            env,
            peer: Some(peer),
            era,
            next_id: 3,
            restarts: 0,
            restart_enabled: false,
            limits,
        })
    }

    /// One-shot dispatches have no later request to benefit from a restart.
    pub(crate) fn disable_restart(&mut self) {
        self.restart_enabled = false;
    }

    #[cfg(test)]
    pub(crate) fn era(&self) -> Era {
        self.era
    }

    /// A failed request is never replayed: it might already have taken effect.
    /// One clean session restart is allowed for a subsequent request only.
    pub(crate) async fn call(
        &mut self,
        method: &str,
        params: Map<String, Value>,
        cancel: &CancellationToken,
    ) -> Result<Value, TransportError> {
        let id = self.next_id;
        self.next_id = self.next_id.checked_add(1).ok_or(TransportError::Closed)?;
        let request = protocol::request(self.era, id, method, params, CLIENT_NAME, CLIENT_VERSION)?;
        let peer = self.peer.as_mut().ok_or(TransportError::Closed)?;
        let result = async {
            bounded(self.limits.request, cancel, peer.send(&request)).await?;
            let message = bounded(self.limits.request, cancel, async {
                loop {
                    let message = peer.receive().await?;
                    let notification = message.get("jsonrpc").and_then(Value::as_str)
                        == Some("2.0")
                        && message.get("id").is_none()
                        && message.get("result").is_none()
                        && message.get("error").is_none()
                        && message.get("params").is_none_or(Value::is_object)
                        && message
                            .get("method")
                            .and_then(Value::as_str)
                            .is_some_and(|m| m.starts_with("notifications/") && m.len() > 14);
                    if !notification {
                        return Ok::<_, TransportError>(message);
                    }
                }
            })
            .await?;
            Ok::<_, TransportError>(protocol::accept_result(&message, id, self.era)?.clone())
        }
        .await;
        if let Err(error) = result {
            let mut peer = self.peer.take().expect("peer existed at call start");
            let error = if error == TransportError::Timeout {
                peer.ensure_no_partial_frame().err().unwrap_or(error)
            } else {
                error
            };
            if error == TransportError::Cancelled {
                // Best effort cancellation is followed by process teardown.
                let notice = serde_json::json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":id}});
                let _ = time::timeout(self.limits.request, peer.send(&notice)).await;
            }
            peer.shutdown(self.limits.shutdown).await;
            if matches!(
                error,
                TransportError::Eof | TransportError::Io | TransportError::Timeout
            ) {
                if !self.restart_enabled {
                    return Err(error);
                }
                if self.restarts == 1 {
                    return Err(TransportError::RestartExhausted);
                }
                self.restarts = 1;
                let (peer, era) =
                    start_session(&self.config, &self.env, self.era, self.limits, cancel).await?;
                if era != self.era {
                    let mut peer = peer;
                    peer.shutdown(self.limits.shutdown).await;
                    return Err(TransportError::Protocol(ProtocolError::UnsupportedVersion));
                }
                self.peer = Some(peer);
            }
            return Err(error);
        }
        result
    }

    pub(crate) async fn shutdown(&mut self) -> bool {
        if let Some(mut peer) = self.peer.take() {
            peer.shutdown(self.limits.shutdown).await
        } else {
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn fixture(mode: &str) -> (McpServerConfig, ResolvedMcpEnvironment) {
        let executable = if cfg!(windows) { "node.exe" } else { "node" };
        let node = std::env::split_paths(&std::env::var_os("PATH").expect("PATH"))
            .map(|dir| dir.join(executable))
            .find(|path| path.is_file())
            .expect("Node fixture runtime");
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mcp_stdio_fixture.mjs");
        (
            McpServerConfig {
                id: "fixture".into(),
                command: node.to_string_lossy().into_owned(),
                args: vec![path.to_string_lossy().into_owned(), mode.into()],
                env: BTreeMap::new(),
                enabled: true,
                revision: 1,
            },
            ResolvedMcpEnvironment {
                effective_revision: "test-only".into(),
                vars: BTreeMap::new(),
            },
        )
    }

    fn limits() -> Limits {
        Limits {
            request: Duration::from_secs(2),
            shutdown: Duration::from_millis(250),
        }
    }

    async fn connect(mode: &str) -> Result<StdioSupervisor, TransportError> {
        let (config, env) = fixture(mode);
        StdioSupervisor::connect(config, env, limits(), &CancellationToken::new()).await
    }

    #[tokio::test]
    async fn notifications_before_replies_are_skipped_but_floods_and_requests_fail_closed() {
        let mut peer = connect("notifications").await.unwrap();
        assert!(peer
            .call("tools/list", Map::new(), &CancellationToken::new())
            .await
            .is_ok());
        let mut params = Map::new();
        params.insert("name".into(), serde_json::json!("echo"));
        params.insert("arguments".into(), serde_json::json!({"text":"reply"}));
        assert!(peer
            .call("tools/call", params, &CancellationToken::new())
            .await
            .is_ok());
        peer.shutdown().await;
        let mut peer = connect("notification-request").await.unwrap();
        peer.disable_restart();
        assert!(matches!(
            peer.call("tools/list", Map::new(), &CancellationToken::new())
                .await,
            Err(TransportError::Protocol(ProtocolError::UnexpectedMessage))
        ));
        let mut peer = connect("notification-flood").await.unwrap();
        peer.disable_restart();
        peer.limits.request = Duration::from_millis(100);
        assert_eq!(
            peer.call("tools/list", Map::new(), &CancellationToken::new())
                .await,
            Err(TransportError::Timeout)
        );
    }

    #[tokio::test]
    async fn freshness_after_probe_prevents_session_launch() {
        use std::cell::Cell;
        let checks = Cell::new(0);
        let (config, env) = fixture("modern");
        let result = StdioSupervisor::connect_checked(
            config,
            env,
            limits(),
            &CancellationToken::new(),
            || {
                checks.set(checks.get() + 1);
                if checks.get() == 2 {
                    Err(TransportError::Closed)
                } else {
                    Ok(())
                }
            },
        )
        .await;
        assert!(matches!(result, Err(TransportError::Closed)));
        assert_eq!(checks.get(), 2);
    }

    #[tokio::test]
    async fn modern_probe_uses_disposable_child_and_request_metadata() {
        let mut supervisor = connect("modern").await.unwrap();
        assert_eq!(supervisor.era(), Era::Modern);
        let result = supervisor
            .call("tools/list", Map::new(), &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result["tools"][0]["name"], "echo");
        assert!(supervisor.shutdown().await);
        let mut split = connect("split").await.unwrap();
        assert_eq!(
            split
                .call("tools/list", Map::new(), &CancellationToken::new())
                .await
                .unwrap()["tools"][0]["name"],
            "echo"
        );
        assert!(split.shutdown().await);
    }

    #[tokio::test]
    async fn delayed_modern_probe_stays_modern_and_keeps_metadata() {
        // The fixture answers discovery after 3.5 s. A generous budget keeps a
        // loaded runner's slow Node start from turning this into a timeout.
        let (config, env) = fixture("modern-slow-probe");
        let limits = Limits {
            request: Duration::from_secs(20),
            shutdown: Duration::from_millis(250),
        };
        let mut supervisor =
            StdioSupervisor::connect(config, env, limits, &CancellationToken::new())
                .await
                .unwrap();
        assert_eq!(supervisor.era(), Era::Modern);
        let mut params = Map::new();
        params.insert("name".into(), Value::String("echo".into()));
        params.insert("arguments".into(), serde_json::json!({"text":"slow reply"}));
        let result = supervisor
            .call("tools/call", params, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result["content"][0]["text"], "slow reply");
        assert!(supervisor.shutdown().await);
    }

    #[tokio::test]
    async fn legacy_exit_requires_valid_fresh_initialize() {
        let mut supervisor = connect("legacy").await.unwrap();
        assert_eq!(supervisor.era(), Era::Legacy(protocol::LEGACY_AEROFTP));
        assert_eq!(
            supervisor
                .call("tools/list", Map::new(), &CancellationToken::new())
                .await
                .unwrap()["tools"][0]["name"],
            "echo"
        );
        assert!(supervisor.shutdown().await);
        assert_eq!(
            connect("legacy-other").await.err(),
            Some(TransportError::Protocol(ProtocolError::UnsupportedVersion))
        );
    }

    #[tokio::test]
    async fn only_typed_probe_silence_can_reach_legacy_handshake() {
        assert_eq!(connect("silent").await.err(), Some(TransportError::Timeout));
        assert_eq!(
            connect("no-overlap").await.err(),
            Some(TransportError::Protocol(ProtocolError::UnsupportedVersion))
        );
        assert_eq!(
            connect("invalid-discovery").await.err(),
            Some(TransportError::Protocol(ProtocolError::InvalidReply))
        );
        assert_eq!(
            connect("malformed").await.err(),
            Some(TransportError::Frame(FrameError::InvalidJson))
        );
        assert_eq!(
            connect("partial-hang").await.err(),
            Some(TransportError::Frame(FrameError::Incomplete))
        );
        assert_eq!(
            connect("oversized").await.err(),
            Some(TransportError::Frame(FrameError::TooLarge))
        );
    }

    #[tokio::test]
    async fn session_eof_restarts_once_without_replaying_request() {
        let mut supervisor = connect("session-eof").await.unwrap();
        let first = supervisor
            .call("tools/list", Map::new(), &CancellationToken::new())
            .await;
        assert!(matches!(
            first,
            Err(TransportError::Eof | TransportError::Io)
        ));
        assert_eq!(supervisor.restarts, 1);
        assert_eq!(
            supervisor
                .call("tools/list", Map::new(), &CancellationToken::new())
                .await,
            Err(TransportError::RestartExhausted)
        );
        assert_eq!(
            supervisor
                .call("tools/list", Map::new(), &CancellationToken::new())
                .await,
            Err(TransportError::Closed)
        );
    }

    #[tokio::test]
    async fn malformed_session_output_and_partial_eof_fail_closed() {
        for (mode, expected) in [
            (
                "session-malformed",
                TransportError::Frame(FrameError::InvalidJson),
            ),
            (
                "session-oversized",
                TransportError::Frame(FrameError::TooLarge),
            ),
            (
                "session-partial",
                TransportError::Frame(FrameError::Incomplete),
            ),
            (
                "session-partial-hang",
                TransportError::Frame(FrameError::Incomplete),
            ),
        ] {
            let mut supervisor = connect(mode).await.unwrap();
            assert_eq!(
                supervisor
                    .call("tools/list", Map::new(), &CancellationToken::new())
                    .await,
                Err(expected),
                "{mode}"
            );
            assert_eq!(supervisor.restarts, 0);
            assert_eq!(
                supervisor
                    .call("tools/list", Map::new(), &CancellationToken::new())
                    .await,
                Err(TransportError::Closed)
            );
        }
    }

    #[tokio::test]
    async fn session_timeout_is_bounded_and_restart_is_not_a_retry() {
        let mut supervisor = connect("session-silent").await.unwrap();
        assert_eq!(
            supervisor
                .call("tools/list", Map::new(), &CancellationToken::new())
                .await,
            Err(TransportError::Timeout)
        );
        assert_eq!(supervisor.restarts, 1);
        assert_eq!(
            supervisor
                .call("tools/list", Map::new(), &CancellationToken::new())
                .await,
            Err(TransportError::RestartExhausted)
        );
    }

    #[tokio::test]
    async fn cancellation_tears_down_session_without_result_or_restart() {
        let mut supervisor = connect("modern").await.unwrap();
        let cancel = CancellationToken::new();
        let trigger = cancel.clone();
        tokio::spawn(async move {
            time::sleep(Duration::from_millis(50)).await;
            trigger.cancel();
        });
        let mut params = Map::new();
        params.insert("name".into(), Value::String("wait".into()));
        params.insert("arguments".into(), Value::Object(Map::new()));
        assert_eq!(
            supervisor.call("tools/call", params, &cancel).await,
            Err(TransportError::Cancelled)
        );
        assert_eq!(supervisor.restarts, 0);
        assert_eq!(
            supervisor
                .call("tools/list", Map::new(), &CancellationToken::new())
                .await,
            Err(TransportError::Closed)
        );
    }

    #[tokio::test]
    async fn stderr_flood_is_drained_and_eof_shutdown_escalates() {
        let mut supervisor = connect("stderr-flood").await.unwrap();
        assert_eq!(
            supervisor
                .call("tools/list", Map::new(), &CancellationToken::new())
                .await
                .unwrap()["tools"][0]["name"],
            "echo"
        );
        assert!(supervisor.shutdown().await);
        let (config, env) = fixture("stubborn");
        let mut peer = Peer::spawn(&config, &env).unwrap();
        assert!(!peer.shutdown(limits().shutdown).await);
    }
}
