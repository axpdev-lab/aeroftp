//! Z.2.1: Local-to-local delta transport.
//!
//! Implements [`DeltaTransport`] for the case where both endpoints are file
//! system paths on the same host. Bypasses the wire protocol 31 entirely:
//! the delta engine runs in-process against two local files, with no SSH,
//! no multiplex framing, no file-list encoding.
//!
//! Design (see `BACKUP-APPENDIX/APPENDIX-L_Local-Transport-aerorsync.md`):
//! - reuse [`CurrentDeltaSyncBridge`] for signature + plan computation
//! - reuse [`write_atomic_chunked`] for kill-9-safe rename-last writes
//! - memory cap at 256 MiB (`LOCAL_DELTA_MAX_IN_MEMORY_BYTES`): files larger
//!   than this return `RsyncError::TransferFailed` and the caller falls back
//!   to a plain `std::fs::copy`
//! - min_file_size guard returns `RsyncError::TooSmall` so the caller can
//!   bypass the delta overhead for small files
//!
//! No SSH, no remote: this transport's `name()` is `"aerorsync-local"` and
//! its `probe_remote` returns a synthetic capability flag (`protocol: 31`)
//! so the rest of the delta dispatch keeps working unchanged.

#![cfg(feature = "aerorsync")]

use std::path::Path;
use std::time::Instant;

use async_trait::async_trait;
use tokio::fs;

use crate::aerorsync::delta_transport_impl::write_atomic_chunked;
use crate::delta_sync;
use crate::delta_transport::DeltaTransport;
use crate::rsync_over_ssh::{RsyncCapability, RsyncError, RsyncStats};

/// Memory cap for the local delta path. Source + baseline + reconstructed
/// buffers are all held in memory; with 256 MiB we keep peak RSS bounded by
/// ~768 MiB worst case (3x). Files above this size fall back to plain copy.
pub const LOCAL_DELTA_MAX_IN_MEMORY_BYTES: u64 = 256 * 1024 * 1024;

/// Atomic write chunk size, matches `ATOMIC_WRITE_CHUNK_SIZE` in
/// `delta_transport_impl.rs`. Local copy to avoid leaking a private symbol.
const ATOMIC_WRITE_CHUNK_SIZE: usize = 64 * 1024;

/// Local-to-local delta transport. Bypasses SSH / wire protocol entirely.
pub struct LocalDeltaTransport {
    min_file_size: u64,
}

impl LocalDeltaTransport {
    /// Construct with a minimum file size below which `TooSmall` is returned.
    /// Caller typically uses [`crate::rsync_over_ssh::DEFAULT_MIN_FILE_SIZE`].
    pub fn new(min_file_size: u64) -> Self {
        Self { min_file_size }
    }

    async fn transfer_inner(&self, src: &Path, dst: &Path) -> Result<RsyncStats, RsyncError> {
        let start = Instant::now();

        let src_meta = fs::metadata(src).await.map_err(RsyncError::Io)?;
        let src_size = src_meta.len();

        if src_size < self.min_file_size {
            return Err(RsyncError::TooSmall {
                size: src_size,
                threshold: self.min_file_size,
            });
        }
        if src_size > LOCAL_DELTA_MAX_IN_MEMORY_BYTES {
            return Err(RsyncError::TransferFailed {
                exit: 0,
                stderr: format!(
                    "local delta size {src_size} exceeds in-memory cap {LOCAL_DELTA_MAX_IN_MEMORY_BYTES}; fallback to classic copy"
                ),
            });
        }

        // Baseline: existing destination, empty if absent.
        let baseline = match fs::read(dst).await {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(RsyncError::Io(e)),
        };

        let source = fs::read(src).await.map_err(RsyncError::Io)?;

        // Compute delta against baseline using the same engine the SSH path
        // uses; this keeps the local fast path bit-for-bit consistent with
        // what would happen if the user had pointed at a remote.
        let (ops, _result, block_size) = {
            let block_size = delta_sync::compute_block_size(source.len() as u64);
            let sig_table = delta_sync::compute_signatures(&baseline, block_size);
            let (ops, result) = delta_sync::compute_delta(&source, &sig_table);
            (ops, result, block_size)
        };

        // Apply the plan to the baseline to reconstruct the target bytes.
        // For an identical baseline this is purely CopyBlock ops and the
        // output equals `source`.
        let reconstructed = delta_sync::apply_delta(&baseline, &ops, block_size)
            .map_err(|e| RsyncError::TransferFailed {
                exit: 0,
                stderr: format!("local delta apply failed: {e}"),
            })?;

        // Account literal bytes for the speedup metric: this is the byte
        // count that would have travelled on the wire in a real delta sync.
        let bytes_sent: u64 = ops
            .iter()
            .map(|op| match op {
                delta_sync::DeltaOp::Literal(b) => b.len() as u64,
                delta_sync::DeltaOp::CopyBlock(_) => 0,
            })
            .sum();

        // Preserve mode + mtime when possible. Both fields are best-effort:
        // a failure to extract them shouldn't sink the transfer.
        let preserve_mode: Option<u32> = {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                Some(src_meta.permissions().mode())
            }
            #[cfg(not(unix))]
            {
                None
            }
        };
        let preserve_mtime: Option<(i64, Option<i32>)> = src_meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| (d.as_secs() as i64, Some(d.subsec_nanos() as i32)));

        write_atomic_chunked(
            dst,
            &reconstructed,
            ATOMIC_WRITE_CHUNK_SIZE,
            None,
            preserve_mode,
            preserve_mtime,
        )
        .await
        .map_err(|e| RsyncError::TransferFailed {
            exit: 0,
            stderr: format!("local atomic write failed: {e:?}"),
        })?;

        let total_size = src_size;
        let duration_ms = start.elapsed().as_millis() as u64;
        let speedup = if bytes_sent == 0 {
            f64::INFINITY
        } else {
            total_size as f64 / bytes_sent as f64
        };

        Ok(RsyncStats {
            bytes_sent,
            bytes_received: src_size,
            total_size,
            speedup,
            duration_ms,
            warnings: Vec::new(),
        })
    }
}

#[async_trait]
impl DeltaTransport for LocalDeltaTransport {
    fn name(&self) -> &'static str {
        "aerorsync-local"
    }

    async fn probe_remote(&self) -> Result<RsyncCapability, RsyncError> {
        Ok(RsyncCapability {
            version: "aerorsync local in-process".to_string(),
            protocol: 31,
        })
    }

    async fn probe_local(&self) -> Result<(), RsyncError> {
        Ok(())
    }

    async fn upload(
        &self,
        local_path: &Path,
        remote_path: &str,
    ) -> Result<RsyncStats, RsyncError> {
        // `remote_path` is interpreted as a local filesystem path.
        self.transfer_inner(local_path, Path::new(remote_path)).await
    }

    async fn download(
        &self,
        remote_path: &str,
        local_path: &Path,
    ) -> Result<RsyncStats, RsyncError> {
        // Inverted direction: `remote_path` is the source on the local fs.
        self.transfer_inner(Path::new(remote_path), local_path).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[tokio::test]
    async fn happy_path_full_copy_when_destination_missing() {
        let dir = tmp_dir();
        let src = dir.path().join("src.bin");
        let dst = dir.path().join("dst.bin");
        let payload = vec![0xABu8; 2 * 1024 * 1024];
        tokio::fs::write(&src, &payload).await.unwrap();

        let transport = LocalDeltaTransport::new(1024);
        let stats = transport
            .transfer_inner(src.as_path(), dst.as_path())
            .await
            .expect("happy path");

        let written = tokio::fs::read(&dst).await.unwrap();
        assert_eq!(written, payload);
        assert_eq!(stats.total_size, payload.len() as u64);
        // No baseline -> all bytes travelled as literal.
        assert_eq!(stats.bytes_sent, payload.len() as u64);
    }

    #[tokio::test]
    async fn identical_baseline_produces_zero_literal_bytes() {
        let dir = tmp_dir();
        let src = dir.path().join("src.bin");
        let dst = dir.path().join("dst.bin");
        let payload = vec![0x42u8; 2 * 1024 * 1024];
        tokio::fs::write(&src, &payload).await.unwrap();
        tokio::fs::write(&dst, &payload).await.unwrap();

        let transport = LocalDeltaTransport::new(1024);
        let stats = transport
            .transfer_inner(src.as_path(), dst.as_path())
            .await
            .expect("identical baseline path");

        let written = tokio::fs::read(&dst).await.unwrap();
        assert_eq!(written, payload);
        // Tail block (last partial block_size) may always travel as literal
        // because compute_signatures pads it; the savings ratio is the
        // metric that matters. Assert << 0.1% of total.
        assert!(
            stats.bytes_sent < payload.len() as u64 / 1000,
            "identical baseline should emit << 0.1% literal bytes, got {}",
            stats.bytes_sent
        );
    }

    #[tokio::test]
    async fn localized_change_keeps_most_bytes_as_match() {
        let dir = tmp_dir();
        let src = dir.path().join("src.bin");
        let dst = dir.path().join("dst.bin");
        let mut payload = vec![0x11u8; 2 * 1024 * 1024];
        tokio::fs::write(&dst, &payload).await.unwrap();
        // 1 KiB change in the middle.
        for byte in &mut payload[1_000_000..1_001_024] {
            *byte = 0xFF;
        }
        tokio::fs::write(&src, &payload).await.unwrap();

        let transport = LocalDeltaTransport::new(1024);
        let stats = transport
            .transfer_inner(src.as_path(), dst.as_path())
            .await
            .expect("localized change path");

        let written = tokio::fs::read(&dst).await.unwrap();
        assert_eq!(written, payload);
        assert!(
            stats.bytes_sent < payload.len() as u64 / 4,
            "localized change should match most blocks: sent {}",
            stats.bytes_sent
        );
    }

    #[tokio::test]
    async fn too_small_returns_too_small_error() {
        let dir = tmp_dir();
        let src = dir.path().join("src.bin");
        let dst = dir.path().join("dst.bin");
        tokio::fs::write(&src, b"short").await.unwrap();

        let transport = LocalDeltaTransport::new(1024 * 1024);
        let err = transport
            .transfer_inner(src.as_path(), dst.as_path())
            .await
            .expect_err("too small");
        match err {
            RsyncError::TooSmall { size, threshold } => {
                assert_eq!(size, 5);
                assert_eq!(threshold, 1024 * 1024);
            }
            other => panic!("expected TooSmall, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn source_not_found_returns_io_error() {
        let dir = tmp_dir();
        let src = dir.path().join("absent.bin");
        let dst = dir.path().join("dst.bin");

        let transport = LocalDeltaTransport::new(1024);
        let err = transport
            .transfer_inner(src.as_path(), dst.as_path())
            .await
            .expect_err("missing source");
        match err {
            RsyncError::Io(io) => {
                assert_eq!(io.kind(), std::io::ErrorKind::NotFound);
            }
            other => panic!("expected Io NotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn oversized_source_falls_back_to_classic() {
        let dir = tmp_dir();
        let src = dir.path().join("big.bin");
        let dst = dir.path().join("dst.bin");
        // Create a sparse-ish file just over the cap. Use set_len so we
        // don't pay the cost of writing 256 MiB of real bytes.
        let file = std::fs::File::create(&src).unwrap();
        file.set_len(LOCAL_DELTA_MAX_IN_MEMORY_BYTES + 1).unwrap();
        drop(file);

        let transport = LocalDeltaTransport::new(1024);
        let err = transport
            .transfer_inner(src.as_path(), dst.as_path())
            .await
            .expect_err("oversized");
        match err {
            RsyncError::TransferFailed { stderr, .. } => {
                assert!(stderr.contains("exceeds in-memory cap"));
            }
            other => panic!("expected TransferFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn upload_and_download_are_symmetric() {
        let dir = tmp_dir();
        let src = dir.path().join("a.bin");
        let dst = dir.path().join("b.bin");
        let payload = vec![0x55u8; 1_500_000];
        tokio::fs::write(&src, &payload).await.unwrap();

        let transport = LocalDeltaTransport::new(1024);
        // upload semantics: local -> remote (interpreted as local fs path)
        transport
            .upload(&src, dst.to_string_lossy().as_ref())
            .await
            .expect("upload");
        assert_eq!(tokio::fs::read(&dst).await.unwrap(), payload);

        // download semantics: remote -> local
        let dst2 = dir.path().join("c.bin");
        transport
            .download(src.to_string_lossy().as_ref(), &dst2)
            .await
            .expect("download");
        assert_eq!(tokio::fs::read(&dst2).await.unwrap(), payload);
    }

    #[tokio::test]
    async fn probe_remote_reports_local_capability() {
        let transport = LocalDeltaTransport::new(1024);
        let cap = transport.probe_remote().await.expect("probe");
        assert_eq!(cap.protocol, 31);
        assert!(cap.version.contains("local"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn preserves_mode_on_unix() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tmp_dir();
        let src = dir.path().join("src.bin");
        let dst = dir.path().join("dst.bin");
        tokio::fs::write(&src, vec![0u8; 1024 * 1024 + 1]).await.unwrap();
        let perms = std::fs::Permissions::from_mode(0o640);
        std::fs::set_permissions(&src, perms).unwrap();

        let transport = LocalDeltaTransport::new(1024);
        transport
            .transfer_inner(src.as_path(), dst.as_path())
            .await
            .expect("transfer");

        let dst_mode = std::fs::metadata(&dst).unwrap().permissions().mode() & 0o777;
        assert_eq!(dst_mode, 0o640);
    }
}
