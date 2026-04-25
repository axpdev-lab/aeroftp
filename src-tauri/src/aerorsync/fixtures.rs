//! Golden fixtures derived from the real wrapper capture at
//! `capture/artifacts/20260417_154800/` and the Sinergia 8a real-rsync
//! byte-oracle capture under `capture/artifacts_real/frozen/`.
//!
//! These constants exist so the prototype has a deterministic, transport-free
//! reference for what the current rsync wrapper actually does. They are the
//! parity target for the first native session subset.
//!
//! Number parsing is delegated to `crate::number_parsing`, the locale-tolerant
//! helpers introduced by Sinergia 1. Keeping one parser means native parity
//! against rsync output stays correct regardless of the child-process locale.

use std::path::{Path, PathBuf};

use crate::number_parsing::{parse_f64_loose, parse_u64_loose};

/// Observed remote command line for upload (local sends, remote receives).
/// Source: `capture/artifacts/20260417_154800/upload.remote_command.txt`.
pub const UPLOAD_REMOTE_COMMAND: &str =
    "rsync --server -logDtprcze.iLsfxCIvu --stats . /workspace/upload/target.bin";

/// Observed remote command line for download (remote sends, local receives).
/// Source: `capture/artifacts/20260417_154800/download.remote_command.txt`.
pub const DOWNLOAD_REMOTE_COMMAND: &str =
    "rsync --server --sender -logDtprcze.iLsfxCIvu . /workspace/download/target.bin";

/// rsync banner observed in `summary.env`.
pub const OBSERVED_RSYNC_BANNER: &str = "rsync  version 3.2.7  protocol version 31";

/// OpenSSH banner observed in `summary.env`.
pub const OBSERVED_SSH_BANNER: &str =
    "OpenSSH_9.6p1 Ubuntu-3ubuntu13.15, OpenSSL 3.0.13 30 Jan 2024";

/// Observed baseline counters for the 8 MiB single-file delta case.
/// Parsed from `upload_actual.stdout.txt` / `download_actual.stdout.txt`.
pub const BASELINE_TOTAL_FILE_SIZE: u64 = 8_388_608;
pub const BASELINE_LITERAL_BYTES: u64 = 156_384;
pub const BASELINE_MATCHED_BYTES: u64 = 8_232_224;

/// Upload-direction observed summary (local = sender).
pub const BASELINE_UPLOAD_BYTES_SENT: u64 = 156_561;
pub const BASELINE_UPLOAD_BYTES_RECEIVED: u64 = 17_417;

/// Download-direction observed summary (local = receiver).
pub const BASELINE_DOWNLOAD_BYTES_SENT: u64 = 17_425;
pub const BASELINE_DOWNLOAD_BYTES_RECEIVED: u64 = 156_554;

/// Observed speedup in both runs. rsync prints with 2 decimal digits; parity
/// checks tolerate ±0.01 since the ratio is rounded.
pub const BASELINE_SPEEDUP: f64 = 48.22;

/// Minimal parsed view of the rsync `--stats` block.
#[derive(Debug, Clone, PartialEq)]
pub struct BaselineCounters {
    pub total_file_size: u64,
    pub literal_bytes: u64,
    pub matched_bytes: u64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    /// Reported on the `total size is X  speedup is Y` line. `None` if the
    /// line was absent from the block (e.g. rsync run without `--stats`).
    pub speedup: Option<f64>,
}

impl BaselineCounters {
    pub fn observed_upload() -> Self {
        Self {
            total_file_size: BASELINE_TOTAL_FILE_SIZE,
            literal_bytes: BASELINE_LITERAL_BYTES,
            matched_bytes: BASELINE_MATCHED_BYTES,
            bytes_sent: BASELINE_UPLOAD_BYTES_SENT,
            bytes_received: BASELINE_UPLOAD_BYTES_RECEIVED,
            speedup: Some(BASELINE_SPEEDUP),
        }
    }

    pub fn observed_download() -> Self {
        Self {
            total_file_size: BASELINE_TOTAL_FILE_SIZE,
            literal_bytes: BASELINE_LITERAL_BYTES,
            matched_bytes: BASELINE_MATCHED_BYTES,
            bytes_sent: BASELINE_DOWNLOAD_BYTES_SENT,
            bytes_received: BASELINE_DOWNLOAD_BYTES_RECEIVED,
            speedup: Some(BASELINE_SPEEDUP),
        }
    }

    /// Invariants we want the native path to preserve:
    ///   1. total_file_size == literal + matched
    ///   2. if speedup is present, it matches total / (sent + received) within
    ///      rsync's 2-decimal rounding tolerance (±0.01)
    pub fn invariants_hold(&self) -> bool {
        if self.literal_bytes + self.matched_bytes != self.total_file_size {
            return false;
        }
        if let Some(s) = self.speedup {
            let total_bytes = self.bytes_sent.saturating_add(self.bytes_received);
            if total_bytes == 0 {
                return false;
            }
            let computed = self.total_file_size as f64 / total_bytes as f64;
            // rsync prints speedup truncated/rounded; allow ±0.01
            if (computed - s).abs() > 0.01 {
                return false;
            }
        }
        true
    }
}

/// Parse the raw counters out of a rsync `--stats` block like the one in
/// `upload_actual.stdout.txt`. Keys are matched by prefix after trimming.
/// Locale-tolerant: handles both en_US (`156,561`) and it_IT (`156.561`)
/// number shapes through the shared `number_parsing` helpers.
///
/// Unknown lines are skipped. Missing counter fields default to 0. A missing
/// "speedup" line leaves `speedup = None`.
pub fn parse_stats_block(stdout: &str) -> BaselineCounters {
    let mut out = BaselineCounters {
        total_file_size: 0,
        literal_bytes: 0,
        matched_bytes: 0,
        bytes_sent: 0,
        bytes_received: 0,
        speedup: None,
    };
    for line in stdout.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Total file size:") {
            out.total_file_size = parse_u64_loose(rest).unwrap_or(0);
        } else if let Some(rest) = trimmed.strip_prefix("Literal data:") {
            out.literal_bytes = parse_u64_loose(rest).unwrap_or(0);
        } else if let Some(rest) = trimmed.strip_prefix("Matched data:") {
            out.matched_bytes = parse_u64_loose(rest).unwrap_or(0);
        } else if let Some(rest) = trimmed.strip_prefix("Total bytes sent:") {
            out.bytes_sent = parse_u64_loose(rest).unwrap_or(0);
        } else if let Some(rest) = trimmed.strip_prefix("Total bytes received:") {
            out.bytes_received = parse_u64_loose(rest).unwrap_or(0);
        } else if let Some(rest) = trimmed.strip_prefix("total size is") {
            // Shape: "total size is 8.388.608  speedup is 48,22"
            if let Some(speedup_part) = rest.split("speedup is").nth(1) {
                out.speedup = parse_f64_loose(speedup_part);
            }
        }
    }
    out
}

#[cfg(test)]
mod parser_tests {
    use super::*;

    const SAMPLE_UPLOAD_STDOUT_IT: &str = r#"
Number of files: 1 (reg: 1)
Number of created files: 0
Number of deleted files: 0
Number of regular files transferred: 1
Total file size: 8.388.608 bytes
Total transferred file size: 8.388.608 bytes
Literal data: 156.384 bytes
Matched data: 8.232.224 bytes
File list size: 0
File list generation time: 0,001 seconds
File list transfer time: 0,000 seconds
Total bytes sent: 156.561
Total bytes received: 17.417

sent 156.561 bytes  received 17.417 bytes  347.956,00 bytes/sec
total size is 8.388.608  speedup is 48,22
"#;

    // Same run, but in en_US locale (rsync emits thousands with ',' and
    // speedup with '.'). We must be insensitive to locale.
    const SAMPLE_UPLOAD_STDOUT_EN: &str = r#"
Total file size: 8,388,608 bytes
Literal data: 156,384 bytes
Matched data: 8,232,224 bytes
Total bytes sent: 156,561
Total bytes received: 17,417

total size is 8,388,608  speedup is 48.22
"#;

    #[test]
    fn parses_observed_upload_counters_it_locale() {
        let parsed = parse_stats_block(SAMPLE_UPLOAD_STDOUT_IT);
        assert_eq!(parsed, BaselineCounters::observed_upload());
        assert!(parsed.invariants_hold());
    }

    #[test]
    fn parses_observed_upload_counters_en_locale() {
        let parsed = parse_stats_block(SAMPLE_UPLOAD_STDOUT_EN);
        assert_eq!(parsed, BaselineCounters::observed_upload());
        assert!(parsed.invariants_hold());
    }

    #[test]
    fn invariants_reject_mismatched_delta_split() {
        let mut bad = BaselineCounters::observed_upload();
        bad.literal_bytes = 999;
        assert!(!bad.invariants_hold());
    }

    #[test]
    fn invariants_reject_mismatched_speedup() {
        let mut bad = BaselineCounters::observed_upload();
        bad.speedup = Some(1.0);
        assert!(!bad.invariants_hold());
    }

    #[test]
    fn missing_speedup_does_not_invalidate_invariants() {
        let mut noninv = BaselineCounters::observed_upload();
        noninv.speedup = None;
        assert!(noninv.invariants_hold());
    }
}

// =============================================================================
// Sinergia 8a — real rsync byte-oracle lane
// =============================================================================
//
// The harness `capture/run_real_rsync_capture.sh` produces a full byte-level
// transcript of rsync protocol ~31/32 running over SSH against a Docker
// fixture on port 2224. The first successful run is frozen at
// `capture/artifacts_real/frozen/` and becomes the reference that S8b and
// later sinergie parse against.
//
// Layout (relative to the repo root):
//
//   src-tauri/src/aerorsync/capture/artifacts_real/frozen/
//     ├── summary.env                      (freeze_ts, fingerprints, byte counts)
//     ├── host_rsync_version.txt
//     ├── server_rsync_version.txt
//     ├── upload/
//     │   ├── capture_in.bin               client -> server wire bytes
//     │   ├── capture_out.bin              server -> client wire bytes
//     │   ├── remote_command.txt           rsync --server -... --stats
//     │   ├── client.stdout.txt            rsync stats block
//     │   └── client.stderr.txt
//     └── download/
//         ├── capture_in.bin               (same scheme, --sender)
//         ├── capture_out.bin
//         ├── remote_command.txt
//         ├── client.stdout.txt
//         └── client.stderr.txt
//
// We deliberately do NOT `include_bytes!` these files: (a) the prototype is
// gitignored so the bytes are not part of the tracked source, (b) the bytes
// are not yet parsed — S8b is where the multiplex tag demux will consume them
// and at that point the load happens at test-time, not at compile-time.

/// Frozen subdirectory of the real-rsync lane. Relative to the cargo
/// manifest directory (`src-tauri/`) so `cargo test` can resolve it.
pub const REAL_RSYNC_FROZEN_TRANSCRIPT_REL: &str = "src/aerorsync/capture/artifacts_real/frozen";

/// Container-side workspace root for the real-rsync lane.
pub const REAL_RSYNC_LANE_WORKDIR: &str = "/workspace/real";

/// Port the real-rsync Docker lane listens on (docker-compose.real-rsync.yml).
pub const REAL_RSYNC_LANE_PORT: u16 = 2224;

/// Entrypoint layout inside the frozen transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RealRsyncTranscriptPaths {
    pub summary_env: PathBuf,
    pub upload_capture_in: PathBuf,
    pub upload_capture_out: PathBuf,
    pub upload_remote_command: PathBuf,
    pub upload_client_stdout: PathBuf,
    pub download_capture_in: PathBuf,
    pub download_capture_out: PathBuf,
    pub download_remote_command: PathBuf,
    pub download_client_stdout: PathBuf,
}

impl RealRsyncTranscriptPaths {
    /// Build an absolute-path view over the frozen transcript rooted at
    /// `manifest_dir`. The caller usually passes
    /// `env!("CARGO_MANIFEST_DIR")`. Paths are not checked for existence
    /// here — the caller decides whether to skip a test if they are
    /// missing (see `try_load_frozen`).
    pub fn rooted_at(manifest_dir: impl AsRef<Path>) -> Self {
        let root = manifest_dir.as_ref().join(REAL_RSYNC_FROZEN_TRANSCRIPT_REL);
        Self {
            summary_env: root.join("summary.env"),
            upload_capture_in: root.join("upload/capture_in.bin"),
            upload_capture_out: root.join("upload/capture_out.bin"),
            upload_remote_command: root.join("upload/remote_command.txt"),
            upload_client_stdout: root.join("upload/client.stdout.txt"),
            download_capture_in: root.join("download/capture_in.bin"),
            download_capture_out: root.join("download/capture_out.bin"),
            download_remote_command: root.join("download/remote_command.txt"),
            download_client_stdout: root.join("download/client.stdout.txt"),
        }
    }

    /// `true` iff every file in the layout exists and is non-empty on the
    /// upload_capture_out side — cheap sanity check the frozen transcript
    /// was produced by a successful run, not a partial one.
    pub fn appears_complete(&self) -> bool {
        let meta_ok =
            |p: &Path| p.exists() && std::fs::metadata(p).map(|m| m.len() > 0).unwrap_or(false);
        meta_ok(&self.summary_env)
            && self.upload_capture_in.exists()
            && meta_ok(&self.upload_capture_out)
            && meta_ok(&self.upload_remote_command)
            && meta_ok(&self.upload_client_stdout)
            && meta_ok(&self.download_capture_out)
            && meta_ok(&self.download_remote_command)
            && meta_ok(&self.download_client_stdout)
    }
}

/// Raw wire bytes from the frozen transcript, loaded at test time.
#[derive(Debug, Clone)]
pub struct RealRsyncBaselineByteTranscript {
    pub upload_client_to_server: Vec<u8>,
    pub upload_server_to_client: Vec<u8>,
    pub download_client_to_server: Vec<u8>,
    pub download_server_to_client: Vec<u8>,
}

impl RealRsyncBaselineByteTranscript {
    /// Load the frozen transcript if it exists. Returns `None` on any
    /// missing file so tests can skip gracefully when the capture has
    /// not been run yet on this machine.
    pub fn try_load_frozen() -> Option<Self> {
        let paths = RealRsyncTranscriptPaths::rooted_at(env!("CARGO_MANIFEST_DIR"));
        if !paths.appears_complete() {
            return None;
        }
        Some(Self {
            upload_client_to_server: std::fs::read(&paths.upload_capture_in).ok()?,
            upload_server_to_client: std::fs::read(&paths.upload_capture_out).ok()?,
            download_client_to_server: std::fs::read(&paths.download_capture_in).ok()?,
            download_server_to_client: std::fs::read(&paths.download_capture_out).ok()?,
        })
    }

    /// The very first 4 bytes of the upload server->client stream are the
    /// protocol version in little-endian. For the frozen fixture this is
    /// 0x1F (31) or 0x20 (32) depending on the server rsync version.
    pub fn upload_greeting_protocol_version_le(&self) -> Option<u32> {
        if self.upload_server_to_client.len() < 4 {
            return None;
        }
        let b = &self.upload_server_to_client[..4];
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
}
