//! Rsync-over-SSH orchestration for AeroSync delta transfers.
//!
//! ## Architecture
//! AeroFTP shells out to the local `rsync` binary with `-e ssh` transport. The local
//! rsync speaks the rsync wire protocol with `rsync --server` on the remote end; we
//! simply parse its stdout/stderr for progress, stats, and errors.
//!
//! ## Why not drive `rsync --server` directly via ssh_exec?
//! Speaking the rsync protocol natively in Rust is a multi-month project; rsync has
//! a long, version-dependent wire format. Wrapping the local rsync binary gives us
//! real delta savings with a fraction of the implementation cost. The cost: rsync
//! must be installed locally.
//!
//! ## Scope (Fase 1)
//! - **Auth**: SSH key only. Password auth falls back to classic transfer.
//! - **Platform**: Unix (Linux/macOS). Windows has no native rsync; falls back to classic.
//! - **Providers**: SFTP only (this module is not reachable for other provider types).
//! - **Probe**: `ssh_exec` is used to verify remote rsync presence and version before
//!   committing to a transfer.
//!
//! ## Fallback policy
//! Every `RsyncError` variant describes a condition under which the caller (the sync
//! loop in `delta_sync_rsync.rs`) must transparently fall back to the classic
//! download/upload path. This is a feature, not a failure mode: delta sync is always
//! an optimization, never a requirement.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// PR-T11 cross-OS surgical removal: the shared types (`RsyncCapability`,
// `RsyncConfig`, `RsyncError`, `RsyncStats`, `DEFAULT_MIN_FILE_SIZE`) are
// consumed by both the Unix-only binary transport and the cross-platform
// native prototype, so they must compile on Windows. The operations that
// actually spawn the system `rsync` binary stay gated with
// `#[cfg(unix)]` below.
#![allow(dead_code)]

#[cfg(unix)]
use crate::providers::sftp::SharedSshHandle;
#[cfg(unix)]
use crate::rsync_output::{parse_line, RsyncEvent};
#[cfg(unix)]
use crate::ssh_exec::ssh_exec_collect;
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
#[cfg(unix)]
use std::process::Stdio;
#[cfg(unix)]
use std::time::Instant;
#[cfg(unix)]
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
#[cfg(unix)]
use tokio::process::Command;

/// Minimum rsync protocol version required; 30 corresponds to rsync 3.0+ which all
/// modern distros ship. Older protocols work functionally but the output format
/// diverges enough that our parser would need variants.
const MIN_RSYNC_PROTOCOL: u32 = 30;

/// File-size threshold below which delta sync is skipped; the SSH + rsync handshake
/// overhead exceeds the saving for tiny files. Configurable per-call via
/// [`RsyncConfig::min_file_size`].
pub const DEFAULT_MIN_FILE_SIZE: u64 = 1024 * 1024; // 1 MiB

/// Result of probing the remote for rsync availability.
#[derive(Debug, Clone)]
pub struct RsyncCapability {
    pub version: String,
    pub protocol: u32,
}

/// Configuration for a single rsync transfer.
#[derive(Debug, Clone)]
pub struct RsyncConfig {
    pub compress: bool,
    pub preserve_times: bool,
    /// Verbose progress reporting on stdout (`--info=progress2`).
    pub progress: bool,
    /// Files smaller than this are rejected with [`RsyncError::TooSmall`] so the caller can fallback.
    pub min_file_size: u64,
    /// Absolute path to an SSH private key on the local filesystem. `None` → classic fallback.
    pub ssh_key_path: Option<PathBuf>,
    /// SSH port (defaults to 22 if `None`).
    pub ssh_port: Option<u16>,
    /// SSH username on the remote.
    pub ssh_user: String,
    /// Remote hostname.
    pub ssh_host: String,
    /// `StrictHostKeyChecking` setting ("yes" / "no" / "accept-new").
    pub strict_host_key_check: String,
    /// Path to known_hosts file (typically `~/.ssh/known_hosts`). Required when
    /// `strict_host_key_check` is not `"no"`.
    pub known_hosts_path: Option<PathBuf>,
}

impl Default for RsyncConfig {
    fn default() -> Self {
        Self {
            compress: true,
            preserve_times: true,
            progress: true,
            min_file_size: DEFAULT_MIN_FILE_SIZE,
            ssh_key_path: None,
            ssh_port: None,
            ssh_user: String::new(),
            ssh_host: String::new(),
            strict_host_key_check: "accept-new".to_string(),
            known_hosts_path: None,
        }
    }
}

/// Post-transfer statistics. `bytes_sent` / `bytes_received` are the key delta-sync
/// metrics: on an unchanged file these should be << `total_size`.
#[derive(Debug, Clone, Default)]
pub struct RsyncStats {
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub total_size: u64,
    pub speedup: f64,
    pub duration_ms: u64,
    /// Warnings collected during transfer (non-fatal). Empty on a clean run.
    /// `pub(crate)` because entries may contain remote file paths and must
    /// not flow to logs, UI, or MCP responses without sanitization.
    pub(crate) warnings: Vec<String>,
}

/// Error conditions. Every variant maps to a fallback-to-classic signal.
#[derive(Debug)]
pub enum RsyncError {
    /// `rsync` binary not present on the remote server.
    RemoteNotAvailable,
    /// `rsync` binary not present on the local machine.
    LocalNotAvailable,
    /// Remote rsync reports a protocol version older than [`MIN_RSYNC_PROTOCOL`].
    VersionTooOld { remote: String, required: u32 },
    /// File is below [`RsyncConfig::min_file_size`]; delta savings would not outweigh overhead.
    TooSmall { size: u64, threshold: u64 },
    /// Probe command failed (SSH error, timeout, non-zero exit).
    ProbeFailed(String),
    /// Local rsync process failed to spawn.
    SpawnFailed(String),
    /// Local rsync exited non-zero.
    TransferFailed { exit: i32, stderr: String },
    /// Caller needs password auth but Fase 1 is key-only.
    PasswordAuthUnsupported,
    /// Required SSH key path is missing or unreadable.
    MissingKey(String),
    /// Operation was cancelled by the caller (future drop).
    Cancelled,
    /// Unhandled I/O error.
    Io(std::io::Error),
    /// Native-path rejection that MUST NOT silently fall back to classic SFTP.
    /// Reserved for failures with security implications (e.g. SSH host-key
    /// pinning mismatch) or for protocol invariants whose re-attempt via
    /// classic wrapper would mask a bug. `transfer_with_delta` translates this
    /// into `DeltaSyncResult::hard_error` instead of the usual
    /// `DeltaSyncResult::fallback`.
    HardRejection(String),
}

impl std::fmt::Display for RsyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RemoteNotAvailable => write!(f, "rsync not available on remote"),
            Self::LocalNotAvailable => write!(f, "rsync not available on local machine"),
            Self::VersionTooOld { remote, required } => write!(
                f,
                "rsync protocol too old on remote ({}); need >= {}",
                remote, required
            ),
            Self::TooSmall { size, threshold } => {
                write!(f, "file too small for delta ({} < {})", size, threshold)
            }
            Self::ProbeFailed(s) => write!(f, "rsync probe failed: {}", s),
            Self::SpawnFailed(s) => write!(f, "local rsync spawn failed: {}", s),
            Self::TransferFailed { exit, stderr } => {
                write!(f, "rsync exit {}: {}", exit, stderr.trim())
            }
            Self::PasswordAuthUnsupported => {
                write!(f, "password-based SSH auth is not supported in Fase 1")
            }
            Self::MissingKey(s) => write!(f, "ssh key unusable: {}", s),
            Self::Cancelled => write!(f, "cancelled"),
            Self::Io(e) => write!(f, "io: {}", e),
            Self::HardRejection(s) => write!(f, "native delta hard rejection: {}", s),
        }
    }
}

impl std::error::Error for RsyncError {}

/// Z.1.2 — classify a [`RsyncError`] as a transient SSH channel drop
/// (worth a single reconnect-then-retry) versus a terminal failure that
/// MUST propagate.
///
/// The retry window is intentionally narrow:
///
/// - `Io` whose `ErrorKind` is one of `BrokenPipe`, `ConnectionAborted`,
///   `ConnectionReset`, `UnexpectedEof`, `TimedOut`, `WouldBlock` → the
///   wire was healthy enough to authenticate but the SSH channel
///   dropped mid-transfer. Re-authing on the saved
///   [`crate::aerorsync::ssh_transport::SshTransportConfig`] gets a new
///   channel without surfacing the blip to the user.
/// - `TransferFailed` whose stderr mentions a recognisable channel-drop
///   string (server logs vary slightly across OpenSSH versions, so we
///   match on a small allowlist of substrings).
///
/// Everything else is terminal:
///
/// - `HardRejection` — host-key mismatch or other security-flagged
///   condition. Retrying would silently mask a fault.
/// - `PasswordAuthUnsupported`, `MissingKey`, `VersionTooOld`,
///   `RemoteNotAvailable`, `LocalNotAvailable` — credential or capability
///   issues that no retry can fix.
/// - `Cancelled` — caller intent, must not be overridden.
/// - `TooSmall`, `SpawnFailed`, `ProbeFailed` — non-recoverable for this
///   file (caller should treat as "skip and continue", not "retry").
pub fn is_transient_for_reconnect(err: &RsyncError) -> bool {
    use std::io::ErrorKind;

    match err {
        RsyncError::Io(io_err) => matches!(
            io_err.kind(),
            ErrorKind::BrokenPipe
                | ErrorKind::ConnectionAborted
                | ErrorKind::ConnectionReset
                | ErrorKind::UnexpectedEof
                | ErrorKind::TimedOut
                | ErrorKind::WouldBlock
        ),
        RsyncError::TransferFailed { stderr, .. } => {
            // Two paths land here:
            //
            // 1. Classic rsync wire failure — OpenSSH/rsync produce a
            //    handful of stable substrings when the SSH channel drops
            //    (we keep the list narrow so we never retry on
            //    "Permission denied" or "Host key verification failed").
            //
            // 2. Native delta fallback envelope produced by
            //    `map_native_error_to_rsync` when `classify_fallback`
            //    returns `AttemptClassicSftpFallback`. The format is
            //    `"native fallback ({Kind:?}): {detail}"`. This is the
            //    pre-commit twin of the `HardRejection` envelope that
            //    fires post-commit. Same Kind allowlist applies (see
            //    `is_transient_native_envelope` below).
            //
            // Lane 2026-05-14 confirmed the live pulse delivers the
            // failure as `TransferFailed` (pre-commit path), not
            // `HardRejection`. Z.1.2 retry MUST pick this case up too.
            let lower = stderr.to_ascii_lowercase();
            if lower.contains("native fallback (") {
                return is_transient_native_envelope(&lower);
            }
            const TRANSIENT_NEEDLES: &[&str] = &[
                "connection reset by peer",
                "connection closed by remote",
                "broken pipe",
                "channel closed",
                "unexpected eof",
                "connection timed out",
                "network is unreachable",
            ];
            TRANSIENT_NEEDLES.iter().any(|needle| lower.contains(needle))
        }
        // Z.1.2 (refined 2026-05-14 lane): `HardRejection` is the
        // formatter-injected envelope `"native hard rejection
        // ({Kind:?}): {detail}"`. The `Kind` substring tells us whether
        // the rejection is a wire-level transport drop (russh channel
        // closed mid-stream → safe to reconnect+retry) or a true hard
        // fault (host-key mismatch, protocol bug → must propagate).
        //
        // Post-commit + TransportFailure is the path the live lane
        // exercised: classify_fallback() promotes it to HardError
        // because the AerorsyncDriver already started writing wire
        // bytes for the failing file, but the AerorsyncBatch retry
        // boundary lives ABOVE that — each file gets its own .aerotmp
        // + atomic rename, so retrying the file is safe.
        RsyncError::HardRejection(msg) => is_transient_native_envelope(&msg.to_ascii_lowercase()),
        RsyncError::PasswordAuthUnsupported
        | RsyncError::MissingKey(_)
        | RsyncError::VersionTooOld { .. }
        | RsyncError::RemoteNotAvailable
        | RsyncError::LocalNotAvailable
        | RsyncError::Cancelled
        | RsyncError::TooSmall { .. }
        | RsyncError::SpawnFailed(_)
        | RsyncError::ProbeFailed(_) => false,
    }
}

/// Shared classifier for the formatter-injected native delta envelope.
///
/// The same Kind set is produced by both `map_native_error_to_rsync`
/// arms: `HardRejection("native hard rejection ({Kind:?}): {detail}")`
/// (post-commit, classify_fallback → HardError) and
/// `TransferFailed { stderr: "native fallback ({Kind:?}): {detail}" }`
/// (pre-commit, classify_fallback → AttemptClassicSftpFallback).
///
/// Z.1.2 batch retry is safe in both cases because every file gets its
/// own `.aerotmp` + atomic rename, so a reconnect+retry never observes
/// a partial commit. The Kind tells us whether the failure is wire-level
/// (TransportFailure / NegotiationFailed / UnsupportedVersion → safe)
/// or a security / protocol fault (HostKeyRejected / InvalidFrame /
/// IllegalStateTransition / PlannerRejected / UnexpectedMessage /
/// Internal / RemoteError → must propagate).
///
/// The caller passes the message lowercased (Debug output of the kind
/// enum is mixed-case, e.g. `TransportFailure`, but the substring match
/// stays case-insensitive). The denylist wins: any deny marker present
/// returns false even if a transient kind is also mentioned.
fn is_transient_native_envelope(lower: &str) -> bool {
    const DENY: &[&str] = &[
        "hostkeyrejected",
        "host key",
        "fingerprint",
        "invalidframe",
        "illegalstatetransition",
        "plannerrejected",
        "unexpectedmessage",
        "internal",
        "remoteerror",
    ];
    if DENY.iter().any(|needle| lower.contains(needle)) {
        return false;
    }
    const TRANSIENT_KINDS: &[&str] = &[
        "(transportfailure)",
        "(negotiationfailed)",
        "(unsupportedversion)",
    ];
    TRANSIENT_KINDS.iter().any(|needle| lower.contains(needle))
}

#[cfg(test)]
mod is_transient_hard_rejection_tests {
    //! Z.1.2 (refined): HardRejection envelope inspection. The post-
    //! commit fallback policy maps russh channel drops to HardRejection
    //! even though the batch retry boundary above can safely reconnect.
    //! Pin the allowlist/denylist explicitly so future kind additions
    //! force a conscious choice.

    use super::*;

    #[test]
    fn hard_rejection_transport_failure_is_transient() {
        let err = RsyncError::HardRejection(
            "native hard rejection (TransportFailure): next_data_frame: remote closed mid file list"
                .to_string(),
        );
        assert!(is_transient_for_reconnect(&err));
    }

    #[test]
    fn hard_rejection_negotiation_failed_is_transient() {
        let err = RsyncError::HardRejection(
            "native hard rejection (NegotiationFailed): kex blip".to_string(),
        );
        assert!(is_transient_for_reconnect(&err));
    }

    #[test]
    fn hard_rejection_host_key_rejected_is_not_transient() {
        let err = RsyncError::HardRejection(
            "native hard rejection (HostKeyRejected): fingerprint mismatch".to_string(),
        );
        assert!(!is_transient_for_reconnect(&err));
    }

    #[test]
    fn hard_rejection_invalid_frame_is_not_transient() {
        let err = RsyncError::HardRejection(
            "native hard rejection (InvalidFrame): unexpected opcode 0x42".to_string(),
        );
        assert!(!is_transient_for_reconnect(&err));
    }

    #[test]
    fn hard_rejection_with_unknown_kind_is_not_transient() {
        // Conservative default: an unrecognised kind label inside the
        // envelope should NOT be retried. Adding a new transient kind
        // requires explicit allowlist entry.
        let err = RsyncError::HardRejection(
            "native hard rejection (SomeBrandNewKind): mystery".to_string(),
        );
        assert!(!is_transient_for_reconnect(&err));
    }

    #[test]
    fn hard_rejection_with_host_key_substring_anywhere_is_not_transient() {
        // Even a formatted message that mentions transport BUT also
        // contains a host-key marker must NOT retry: denylist wins.
        let err = RsyncError::HardRejection(
            "native hard rejection (TransportFailure): host key verification failed".to_string(),
        );
        assert!(!is_transient_for_reconnect(&err));
    }

    #[test]
    fn hard_rejection_match_is_case_insensitive() {
        let err = RsyncError::HardRejection(
            "Native Hard Rejection (TransportFailure): Remote Closed".to_string(),
        );
        assert!(is_transient_for_reconnect(&err));
    }

    // --- TransferFailed envelope (pre-commit fallback) ----------------
    //
    // Pre-commit `classify_fallback` returns `AttemptClassicSftpFallback`,
    // which `map_native_error_to_rsync` maps to
    // `TransferFailed { exit: -1, stderr: "native fallback (Kind): detail" }`.
    // The live Z.1.2 lane on 2026-05-14 saw 65/100 files arrive via this
    // envelope (only 1 via HardRejection), so it MUST be classified the
    // same as the HardRejection counterpart.

    #[test]
    fn transfer_failed_native_fallback_transport_failure_is_transient() {
        let err = RsyncError::TransferFailed {
            exit: -1,
            stderr: "native fallback (TransportFailure): next_data_frame: remote closed mid file list"
                .to_string(),
        };
        assert!(is_transient_for_reconnect(&err));
    }

    #[test]
    fn transfer_failed_native_fallback_channel_send_error_is_transient() {
        // Variant observed live: russh channel_open_session error after
        // an iptables REJECT pulse on the SSH port.
        let err = RsyncError::TransferFailed {
            exit: -1,
            stderr: "native fallback (TransportFailure): russh channel_open_session: Channel send error"
                .to_string(),
        };
        assert!(is_transient_for_reconnect(&err));
    }

    #[test]
    fn transfer_failed_native_fallback_negotiation_failed_is_transient() {
        let err = RsyncError::TransferFailed {
            exit: -1,
            stderr: "native fallback (NegotiationFailed): initial handshake blip".to_string(),
        };
        assert!(is_transient_for_reconnect(&err));
    }

    #[test]
    fn transfer_failed_native_fallback_host_key_rejected_is_not_transient() {
        let err = RsyncError::TransferFailed {
            exit: -1,
            stderr: "native fallback (HostKeyRejected): fingerprint mismatch".to_string(),
        };
        assert!(!is_transient_for_reconnect(&err));
    }

    #[test]
    fn transfer_failed_native_fallback_invalid_frame_is_not_transient() {
        let err = RsyncError::TransferFailed {
            exit: -1,
            stderr: "native fallback (InvalidFrame): unexpected opcode 0xff".to_string(),
        };
        assert!(!is_transient_for_reconnect(&err));
    }

    #[test]
    fn transfer_failed_native_fallback_unknown_kind_is_not_transient() {
        // Conservative default: unknown Kind label is not retried.
        let err = RsyncError::TransferFailed {
            exit: -1,
            stderr: "native fallback (SomeBrandNewKind): mystery".to_string(),
        };
        assert!(!is_transient_for_reconnect(&err));
    }

    #[test]
    fn transfer_failed_native_fallback_envelope_match_is_case_insensitive() {
        let err = RsyncError::TransferFailed {
            exit: -1,
            stderr: "Native Fallback (TransportFailure): Channel Send Error".to_string(),
        };
        assert!(is_transient_for_reconnect(&err));
    }
}

#[cfg(test)]
mod is_transient_tests {
    use super::*;

    #[test]
    fn io_broken_pipe_is_transient() {
        let err = RsyncError::Io(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pipe gone"));
        assert!(is_transient_for_reconnect(&err));
    }

    #[test]
    fn io_connection_reset_is_transient() {
        let err = RsyncError::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "RST",
        ));
        assert!(is_transient_for_reconnect(&err));
    }

    #[test]
    fn io_unexpected_eof_is_transient() {
        let err = RsyncError::Io(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "EOF mid-stream",
        ));
        assert!(is_transient_for_reconnect(&err));
    }

    #[test]
    fn io_permission_denied_is_not_transient() {
        let err = RsyncError::Io(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "perm",
        ));
        assert!(!is_transient_for_reconnect(&err));
    }

    #[test]
    fn transfer_failed_with_connection_reset_substring_is_transient() {
        let err = RsyncError::TransferFailed {
            exit: 12,
            stderr: "rsync error: connection reset by peer\n".to_string(),
        };
        assert!(is_transient_for_reconnect(&err));
    }

    #[test]
    fn transfer_failed_with_permission_denied_is_not_transient() {
        let err = RsyncError::TransferFailed {
            exit: 12,
            stderr: "rsync error: Permission denied (publickey)\n".to_string(),
        };
        assert!(!is_transient_for_reconnect(&err));
    }

    #[test]
    fn transfer_failed_with_host_key_mismatch_is_not_transient() {
        let err = RsyncError::TransferFailed {
            exit: 255,
            stderr: "Host key verification failed.\n".to_string(),
        };
        assert!(!is_transient_for_reconnect(&err));
    }

    #[test]
    fn hard_rejection_without_envelope_kind_is_not_transient() {
        // Bare HardRejection without the formatter envelope: conservative
        // default is "do not retry". The richer envelope-aware tests
        // live in the `is_transient_hard_rejection_tests` module below.
        let err = RsyncError::HardRejection("host key mismatch".into());
        assert!(!is_transient_for_reconnect(&err));
    }

    #[test]
    fn cancelled_is_never_transient() {
        assert!(!is_transient_for_reconnect(&RsyncError::Cancelled));
    }

    #[test]
    fn missing_key_is_never_transient() {
        let err = RsyncError::MissingKey("~/.ssh/id_rsa".into());
        assert!(!is_transient_for_reconnect(&err));
    }

    #[test]
    fn empty_transfer_failed_stderr_is_not_transient() {
        // Empty stderr → no needle match → terminal. We don't blindly
        // retry on exit code alone.
        let err = RsyncError::TransferFailed {
            exit: 1,
            stderr: String::new(),
        };
        assert!(!is_transient_for_reconnect(&err));
    }

    #[test]
    fn transient_needle_match_is_case_insensitive() {
        let err = RsyncError::TransferFailed {
            exit: 12,
            stderr: "Connection Reset By Peer".to_string(),
        };
        assert!(is_transient_for_reconnect(&err));
    }
}

// ===== Unix-only orchestration =====
//
// The helpers below drive the system `rsync` binary over an SSH pipe. They
// exist exclusively for the `RsyncBinaryTransport` Unix path; Windows
// delivers delta sync through the cross-platform native prototype transport
// in `aerorsync`, which does not need any of these helpers.

/// Probe the remote for rsync availability and version.
///
/// Runs `rsync --version` over the shared SSH handle via [`ssh_exec_collect`]. Output
/// is capped at 8 KiB to prevent runaway servers. Cached by the caller (typically
/// keyed on session id) to avoid re-probing on every file.
#[cfg(unix)]
pub async fn probe_rsync(handle: SharedSshHandle) -> Result<RsyncCapability, RsyncError> {
    const MAX_OUTPUT: usize = 8 * 1024;

    // `ssh_exec_collect` runs a direct SSH exec command. Use an absolute path
    // so remote PATH differences do not falsely report "rsync unavailable".
    let (stdout, stderr, exit) = ssh_exec_collect(handle, "/usr/bin/rsync --version", MAX_OUTPUT)
        .await
        .map_err(RsyncError::ProbeFailed)?;

    let stdout_len = stdout.len();
    let stderr_len = stderr.len();

    // Some remote/server combinations emit the version banner on stderr when
    // no PTY is allocated. Accept that shape for capability probing.
    let output = if stdout.is_empty() && !stderr.is_empty() {
        stderr
    } else {
        stdout
    };

    if output.is_empty() {
        tracing::warn!(
            "delta probe: remote rsync unavailable (exit={}, stdout_len={}, stderr_len={})",
            exit,
            stdout_len,
            stderr_len
        );
        return Err(RsyncError::RemoteNotAvailable);
    }

    if exit != 0 {
        tracing::debug!(
            "delta probe: non-zero exit with non-empty output (exit={}, stdout_len={}, stderr_len={})",
            exit,
            stdout_len,
            stderr_len
        );
    }

    let text = String::from_utf8_lossy(&output);
    // First line is the `command -v rsync` output (path to binary). Subsequent lines
    // are rsync --version banner. We look for something like:
    //   rsync  version 3.2.7  protocol version 31
    let version_line = text
        .lines()
        .find(|l| l.contains("version") && l.contains("protocol"))
        .ok_or_else(|| RsyncError::ProbeFailed(format!("unexpected version output: {:?}", text)))?;

    let version = extract_version(version_line).unwrap_or_else(|| "unknown".to_string());
    let protocol = extract_protocol(version_line)
        .ok_or_else(|| RsyncError::ProbeFailed(format!("no protocol in: {}", version_line)))?;

    if protocol < MIN_RSYNC_PROTOCOL {
        return Err(RsyncError::VersionTooOld {
            remote: version,
            required: MIN_RSYNC_PROTOCOL,
        });
    }

    Ok(RsyncCapability { version, protocol })
}

#[cfg(unix)]
fn extract_version(line: &str) -> Option<String> {
    // "rsync  version 3.2.7  protocol version 31" → "3.2.7"
    let marker = "version ";
    let idx = line.find(marker)?;
    let rest = &line[idx + marker.len()..];
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    Some(rest[..end].to_string())
}

#[cfg(unix)]
fn extract_protocol(line: &str) -> Option<u32> {
    let marker = "protocol version ";
    let idx = line.find(marker)?;
    let rest = &line[idx + marker.len()..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// Verify that `rsync` is on the local PATH. Cheap: synchronous which2 crate would
/// be overkill; we just probe with `rsync --version` through tokio.
#[cfg(unix)]
pub async fn probe_local_rsync() -> Result<String, RsyncError> {
    let output = Command::new("rsync")
        .arg("--version")
        .output()
        .await
        .map_err(|_| RsyncError::LocalNotAvailable)?;

    if !output.status.success() {
        return Err(RsyncError::LocalNotAvailable);
    }

    let banner = String::from_utf8_lossy(&output.stdout);
    let first_line = banner.lines().next().unwrap_or("rsync");
    Ok(first_line.to_string())
}

/// Build the `-e ssh …` argument that instructs local rsync how to open its
/// transport. Covers: port, identity file, known_hosts, StrictHostKeyChecking.
#[cfg(unix)]
fn build_ssh_e_arg(cfg: &RsyncConfig) -> Result<String, RsyncError> {
    let key = cfg
        .ssh_key_path
        .as_ref()
        .ok_or(RsyncError::PasswordAuthUnsupported)?;

    if !key.exists() {
        return Err(RsyncError::MissingKey(format!(
            "{}: not found",
            key.display()
        )));
    }

    let mut parts: Vec<String> = vec!["ssh".to_string()];
    if let Some(port) = cfg.ssh_port {
        parts.push("-p".into());
        parts.push(port.to_string());
    }
    parts.push("-i".into());
    parts.push(shell_escape(&key.display().to_string()));
    parts.push("-o".into());
    parts.push(format!(
        "StrictHostKeyChecking={}",
        cfg.strict_host_key_check
    ));
    if let Some(kh) = &cfg.known_hosts_path {
        parts.push("-o".into());
        parts.push(format!(
            "UserKnownHostsFile={}",
            shell_escape(&kh.display().to_string())
        ));
    }
    parts.push("-o".into());
    parts.push("BatchMode=yes".into()); // never prompt
    Ok(parts.join(" "))
}

#[cfg(unix)]
fn shell_escape(s: &str) -> String {
    if s.chars().all(|c| c.is_alphanumeric() || "/._-".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// Download a remote file into `local_path` using delta sync.
///
/// Pre-conditions enforced: rsync present locally, local file (if any) larger than
/// threshold. Remote presence is the caller's responsibility (probe once per
/// session, cache).
#[cfg(unix)]
pub async fn rsync_download(
    remote_path: &str,
    local_path: &Path,
    config: &RsyncConfig,
) -> Result<RsyncStats, RsyncError> {
    probe_local_rsync().await?;

    // Threshold check: if destination file exists and is smaller than threshold, skip.
    if let Ok(meta) = tokio::fs::metadata(local_path).await {
        let size = meta.len();
        if size < config.min_file_size {
            return Err(RsyncError::TooSmall {
                size,
                threshold: config.min_file_size,
            });
        }
    }

    let ssh_arg = build_ssh_e_arg(config)?;
    let remote_spec = format!("{}@{}:{}", config.ssh_user, config.ssh_host, remote_path);

    let mut cmd = Command::new("rsync");
    cmd.arg("-a"); // archive (includes -t, -p, -r, -l, -g, -o, -D)
    if config.compress {
        cmd.arg("-z");
    }
    if config.progress {
        cmd.arg("--info=progress2");
    }
    cmd.arg("--stats");
    cmd.arg("-e").arg(&ssh_arg);
    cmd.arg(&remote_spec);
    cmd.arg(local_path);

    run_rsync(cmd).await
}

/// Upload `local_path` to `remote_path` using delta sync.
#[cfg(unix)]
pub async fn rsync_upload(
    local_path: &Path,
    remote_path: &str,
    config: &RsyncConfig,
) -> Result<RsyncStats, RsyncError> {
    probe_local_rsync().await?;

    let meta = tokio::fs::metadata(local_path)
        .await
        .map_err(RsyncError::Io)?;
    if meta.len() < config.min_file_size {
        return Err(RsyncError::TooSmall {
            size: meta.len(),
            threshold: config.min_file_size,
        });
    }

    let ssh_arg = build_ssh_e_arg(config)?;
    let remote_spec = format!("{}@{}:{}", config.ssh_user, config.ssh_host, remote_path);

    let mut cmd = Command::new("rsync");
    cmd.arg("-a");
    if config.compress {
        cmd.arg("-z");
    }
    if config.progress {
        cmd.arg("--info=progress2");
    }
    cmd.arg("--stats");
    cmd.arg("-e").arg(&ssh_arg);
    cmd.arg(local_path);
    cmd.arg(&remote_spec);

    run_rsync(cmd).await
}

/// Execute a configured rsync [`Command`], streaming stdout through the parser
/// and collecting stats.
#[cfg(unix)]
async fn run_rsync(mut cmd: Command) -> Result<RsyncStats, RsyncError> {
    // The rsync output parser (`rsync_output`) uses locale-tolerant number
    // parsing (`number_parsing`) that accepts both en_US ("1,048,576" / "1.00")
    // and it_IT / de_DE / fr_FR ("1.048.576" / "1,00") conventions. No LC_*
    // override is needed: the child process inherits the caller's locale and
    // the parser adapts. Keeping locale inheritance means any future error
    // messages from rsync stay in the user's language.
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    let start = Instant::now();
    let mut child = cmd
        .spawn()
        .map_err(|e| RsyncError::SpawnFailed(e.to_string()))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| RsyncError::SpawnFailed("no stdout pipe".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| RsyncError::SpawnFailed("no stderr pipe".into()))?;

    let stdout_task = tokio::spawn(async move {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        let mut stats = RsyncStats::default();
        let mut sent_summary: Option<RsyncStats> = None;
        let mut warnings = Vec::<String>::new();

        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => {}
                Err(_) => break,
            }

            if let Some(evt) = parse_line(&line) {
                match evt {
                    RsyncEvent::Progress { .. } => {
                        // TODO (T1.5): forward to UI via tauri event in adapter layer.
                    }
                    RsyncEvent::Summary {
                        sent,
                        received,
                        bytes_per_sec: _,
                        total_size,
                        speedup,
                    } => {
                        // Two summary lines per run: merge partials.
                        if total_size > 0 {
                            stats.total_size = total_size;
                            stats.speedup = speedup;
                        }
                        if sent + received > 0 {
                            stats.bytes_sent = sent;
                            stats.bytes_received = received;
                        }
                        sent_summary = Some(stats.clone());
                    }
                    RsyncEvent::Warning { message } => {
                        warnings.push(message);
                    }
                    RsyncEvent::Error { .. } | RsyncEvent::FileStart { .. } => {
                        // Errors surface via exit status + stderr; FileStart currently no-op.
                    }
                }
            }
        }

        stats.warnings = warnings;
        sent_summary.unwrap_or(stats)
    });

    let stderr_task = tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        let mut buf = String::new();
        let _ = reader.read_to_string(&mut buf).await;
        buf
    });

    let exit = child.wait().await.map_err(RsyncError::Io)?;
    let mut stats = stdout_task.await.unwrap_or_default();
    let stderr_output = stderr_task.await.unwrap_or_default();

    if !exit.success() {
        return Err(RsyncError::TransferFailed {
            exit: exit.code().unwrap_or(-1),
            stderr: stderr_output,
        });
    }

    stats.duration_ms = start.elapsed().as_millis() as u64;
    Ok(stats)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn extract_version_full_banner() {
        let line = "rsync  version 3.2.7  protocol version 31";
        assert_eq!(extract_version(line).as_deref(), Some("3.2.7"));
        assert_eq!(extract_protocol(line), Some(31));
    }

    #[test]
    fn extract_version_minimal() {
        let line = "rsync version 3.0.9 protocol version 30";
        assert_eq!(extract_version(line).as_deref(), Some("3.0.9"));
        assert_eq!(extract_protocol(line), Some(30));
    }

    #[test]
    fn extract_protocol_rejects_garbage() {
        assert_eq!(extract_protocol("no protocol here"), None);
    }

    #[test]
    fn shell_escape_passes_safe_chars() {
        assert_eq!(
            shell_escape("/home/user/.ssh/id_ed25519"),
            "/home/user/.ssh/id_ed25519"
        );
    }

    #[test]
    fn shell_escape_quotes_spaces() {
        assert_eq!(shell_escape("/path with space"), "'/path with space'");
    }

    #[test]
    fn shell_escape_escapes_single_quote() {
        let escaped = shell_escape("it's");
        assert!(escaped.starts_with('\''));
        assert!(escaped.ends_with('\''));
    }

    #[test]
    fn ssh_e_arg_requires_key() {
        let cfg = RsyncConfig {
            ssh_key_path: None,
            ..Default::default()
        };
        match build_ssh_e_arg(&cfg) {
            Err(RsyncError::PasswordAuthUnsupported) => {}
            other => panic!("expected PasswordAuthUnsupported, got {:?}", other),
        }
    }

    #[test]
    fn ssh_e_arg_rejects_missing_key() {
        let cfg = RsyncConfig {
            ssh_key_path: Some(PathBuf::from("/definitely/not/a/key")),
            ..Default::default()
        };
        match build_ssh_e_arg(&cfg) {
            Err(RsyncError::MissingKey(_)) => {}
            other => panic!("expected MissingKey, got {:?}", other),
        }
    }

    #[test]
    fn ssh_e_arg_shape() {
        // Use /etc/hostname which is guaranteed to exist and readable on Linux CI.
        let key = PathBuf::from("/etc/hostname");
        if !key.exists() {
            return; // Skip on non-Linux CI
        }
        let cfg = RsyncConfig {
            ssh_key_path: Some(key),
            ssh_port: Some(2222),
            strict_host_key_check: "accept-new".into(),
            ..Default::default()
        };
        let arg = build_ssh_e_arg(&cfg).expect("ssh arg");
        assert!(arg.starts_with("ssh"));
        assert!(arg.contains("-p 2222"));
        assert!(arg.contains("-i "));
        assert!(arg.contains("StrictHostKeyChecking=accept-new"));
        assert!(arg.contains("BatchMode=yes"));
    }
}
