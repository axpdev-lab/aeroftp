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
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

#![cfg(unix)]
// Foundations module for Fase 1 delta sync. Orchestrator for the local rsync
// process + SSH transport. Consumed by `delta_sync_rsync::transfer_with_delta`.
// Items appear "never used" until T1.5 Part B wires the delta branch in the
// sync loop — remove this allow then.
#![allow(dead_code)]

use crate::providers::sftp::SharedSshHandle;
use crate::rsync_output::{parse_line, RsyncEvent};
use crate::ssh_exec::ssh_exec_collect;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
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
    pub warnings: Vec<String>,
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
            Self::TooSmall { size, threshold } => write!(
                f,
                "file too small for delta ({} < {})",
                size, threshold
            ),
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
        }
    }
}

impl std::error::Error for RsyncError {}

/// Probe the remote for rsync availability and version.
///
/// Runs `rsync --version` over the shared SSH handle via [`ssh_exec_collect`]. Output
/// is capped at 8 KiB to prevent runaway servers. Cached by the caller (typically
/// keyed on session id) to avoid re-probing on every file.
pub async fn probe_rsync(handle: SharedSshHandle) -> Result<RsyncCapability, RsyncError> {
    const MAX_OUTPUT: usize = 8 * 1024;

    let (stdout, _stderr, exit) =
        ssh_exec_collect(handle, "command -v rsync && rsync --version", MAX_OUTPUT)
            .await
            .map_err(RsyncError::ProbeFailed)?;

    if exit != 0 || stdout.is_empty() {
        return Err(RsyncError::RemoteNotAvailable);
    }

    let text = String::from_utf8_lossy(&stdout);
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

fn extract_version(line: &str) -> Option<String> {
    // "rsync  version 3.2.7  protocol version 31" → "3.2.7"
    let marker = "version ";
    let idx = line.find(marker)?;
    let rest = &line[idx + marker.len()..];
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    Some(rest[..end].to_string())
}

fn extract_protocol(line: &str) -> Option<u32> {
    let marker = "protocol version ";
    let idx = line.find(marker)?;
    let rest = &line[idx + marker.len()..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// Verify that `rsync` is on the local PATH. Cheap — synchronous which2 crate would
/// be overkill; we just probe with `rsync --version` through tokio.
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
    parts.push(format!("StrictHostKeyChecking={}", cfg.strict_host_key_check));
    if let Some(kh) = &cfg.known_hosts_path {
        parts.push("-o".into());
        parts.push(format!("UserKnownHostsFile={}", shell_escape(&kh.display().to_string())));
    }
    parts.push("-o".into());
    parts.push("BatchMode=yes".into()); // never prompt
    Ok(parts.join(" "))
}

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
async fn run_rsync(mut cmd: Command) -> Result<RsyncStats, RsyncError> {
    // The rsync output parser (`rsync_output`) uses locale-tolerant number
    // parsing (`number_parsing`) that accepts both en_US ("1,048,576" / "1.00")
    // and it_IT / de_DE / fr_FR ("1.048.576" / "1,00") conventions. No LC_*
    // override is needed: the child process inherits the caller's locale and
    // the parser adapts. Keeping locale inheritance means any future error
    // messages from rsync stay in the user's language.
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    let start = Instant::now();
    let mut child = cmd.spawn().map_err(|e| RsyncError::SpawnFailed(e.to_string()))?;

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
                        // Two summary lines per run — merge partials.
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

#[cfg(test)]
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
        assert_eq!(shell_escape("/home/user/.ssh/id_ed25519"), "/home/user/.ssh/id_ed25519");
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
