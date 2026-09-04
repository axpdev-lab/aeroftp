//! Z.2.1: Local-to-local delta transport.
//!
//! The local case: both endpoints are file system paths on the same host. Bypasses the wire protocol 31 entirely:
//! the delta engine runs in-process against two local files, with no SSH,
//! no multiplex framing, no file-list encoding.
//!
//! Design (see `BACKUP-APPENDIX/APPENDIX-L_Local-Transport-aerorsync.md`):
//! - reuse [`CurrentDeltaSyncBridge`] for signature + plan computation
//! - reuse [`write_atomic_chunked`] (from `streaming_writer`) for kill-9-safe
//!   rename-last writes
//! - memory cap at 256 MiB (`LOCAL_DELTA_MAX_IN_MEMORY_BYTES`): files larger
//!   than this return a soft failure and the caller falls back to a
//!   plain `std::fs::copy`
//! - min_file_size guard returns `TransferError::TooSmall` so the caller
//!   can bypass the delta overhead for small files
//!
//! No SSH, no remote: this transport's `name()` is `"aerorsync-local"` and
//! its `probe_remote` returns a synthetic capability flag (`protocol: 31`)
//! so the rest of the delta dispatch keeps working unchanged. The trait
//! implementation that carries these to the application lives in
//! `aerorsync_adapter::local`.

// SPDX-License-Identifier: MPL-2.0 OR GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

#![cfg(feature = "aerorsync")]

use std::path::Path;
use std::time::Instant;

use tokio::fs;

use crate::aerorsync::delta_engine;
use crate::aerorsync::streaming_writer::{write_atomic_chunked, write_atomic_chunked_sparse};
use crate::aerorsync::transport::TransportProbe;
use crate::aerorsync::types::{ProtocolVersion, SessionStats, TransferError, TransferReport};

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
    /// When true, reconstructed output is written with hole punching
    /// (rsync `--sparse` analogue): all-zero chunks become filesystem
    /// holes instead of allocated zero blocks. Opt-in; the default
    /// preserves the historical dense write so existing callers and the
    /// kill-9 atomicity tests are unaffected.
    sparse: bool,
}

impl LocalDeltaTransport {
    /// Construct with a minimum file size below which `TooSmall` is returned.
    /// The caller passes its own minimum size gate; AeroFTP passes its
    /// 1 MiB default.
    pub fn new(min_file_size: u64) -> Self {
        Self {
            min_file_size,
            sparse: false,
        }
    }

    /// Builder: enable sparse (hole-punched) destination writes. Use for
    /// workloads with large zero regions (VM images, pre-allocated DB
    /// files, core dumps) where the dense representation wastes blocks.
    pub fn with_sparse(mut self, sparse: bool) -> Self {
        self.sparse = sparse;
        self
    }

    /// Run the local delta and report the outcome in the crate's own
    /// types. The application adapter renders it: this transport's
    /// numbers are not the remote one's, and the rendering differs with
    /// them (see `aerorsync_adapter::local`).
    pub(crate) async fn transfer(
        &self,
        src: &Path,
        dst: &Path,
    ) -> Result<TransferReport, TransferError> {
        let start = Instant::now();

        let src_meta = fs::metadata(src).await.map_err(TransferError::Io)?;
        let src_size = src_meta.len();

        if src_size < self.min_file_size {
            return Err(TransferError::TooSmall {
                size: src_size,
                threshold: self.min_file_size,
            });
        }
        if src_size > LOCAL_DELTA_MAX_IN_MEMORY_BYTES {
            return Err(TransferError::Soft {
                detail: format!(
                    "local delta size {src_size} exceeds in-memory cap {LOCAL_DELTA_MAX_IN_MEMORY_BYTES}; fallback to classic copy"
                ),
            });
        }

        // Baseline: existing destination, empty if absent.
        let baseline = match fs::read(dst).await {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(TransferError::Io(e)),
        };

        let source = fs::read(src).await.map_err(TransferError::Io)?;

        // Compute delta against baseline using the same engine the SSH path
        // uses; this keeps the local fast path bit-for-bit consistent with
        // what would happen if the user had pointed at a remote.
        let (ops, _result, block_size) = {
            let block_size = delta_engine::compute_block_size(source.len() as u64);
            let sig_table = delta_engine::compute_signatures(&baseline, block_size);
            let (ops, result) = delta_engine::compute_delta(&source, &sig_table);
            (ops, result, block_size)
        };

        // Apply the plan to the baseline to reconstruct the target bytes.
        // For an identical baseline this is purely CopyBlock ops and the
        // output equals `source`.
        let reconstructed =
            delta_engine::apply_delta(&baseline, &ops, block_size).map_err(|e| {
                TransferError::Soft {
                    detail: format!("local delta apply failed: {e}"),
                }
            })?;

        // Account literal bytes for the speedup metric: this is the byte
        // count that would have travelled on the wire in a real delta sync.
        let bytes_sent: u64 = ops
            .iter()
            .map(|op| match op {
                delta_engine::DeltaOp::Literal(b) => b.len() as u64,
                delta_engine::DeltaOp::CopyBlock(_) => 0,
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

        if self.sparse {
            write_atomic_chunked_sparse(
                dst,
                &reconstructed,
                ATOMIC_WRITE_CHUNK_SIZE,
                None,
                preserve_mode,
                preserve_mtime,
            )
            .await
        } else {
            write_atomic_chunked(
                dst,
                &reconstructed,
                ATOMIC_WRITE_CHUNK_SIZE,
                None,
                preserve_mode,
                preserve_mtime,
            )
            .await
        }
        .map_err(|e| TransferError::Soft {
            detail: format!("local atomic write failed: {e:?}"),
        })?;

        let total_size = src_size;
        let duration_ms = start.elapsed().as_millis() as u64;

        // `bytes_received` is the whole source: locally nothing travels,
        // so the reconstructed size is what the destination received.
        // `copy_blocks` stays 0 as it always did on this path. The
        // speedup is not here on purpose: the local adapter computes it,
        // because this transport reports an infinite speedup when
        // nothing was sent and the remote one reports 1.0.
        Ok(TransferReport {
            session: SessionStats {
                bytes_sent,
                bytes_received: src_size,
                copy_blocks: 0,
                ..SessionStats::default()
            },
            total_size,
            duration_ms,
            warnings: Vec::new(),
        })
    }
}

/// Name this transport reports. The application adapter copies it into
/// the trait implementation and a test pins the two together.
pub const LOCAL_TRANSPORT_NAME: &str = "aerorsync-local";

/// The synthetic probe of a transport that has no remote: the delta
/// dispatch expects a capability record, and this one says "protocol 31,
/// in process". The adapter renders it like any other probe.
pub(crate) fn local_probe() -> TransportProbe {
    TransportProbe {
        remote_banner: "aerorsync local in-process".to_string(),
        protocol: ProtocolVersion(31),
        supports_remote_shell: false,
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
        let report = transport
            .transfer(src.as_path(), dst.as_path())
            .await
            .expect("happy path");

        let written = tokio::fs::read(&dst).await.unwrap();
        assert_eq!(written, payload);
        assert_eq!(report.total_size, payload.len() as u64);
        // No baseline -> all bytes travelled as literal.
        assert_eq!(report.session.bytes_sent, payload.len() as u64);
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
        let report = transport
            .transfer(src.as_path(), dst.as_path())
            .await
            .expect("identical baseline path");

        let written = tokio::fs::read(&dst).await.unwrap();
        assert_eq!(written, payload);
        // Tail block (last partial block_size) may always travel as literal
        // because compute_signatures pads it; the savings ratio is the
        // metric that matters. Assert << 0.1% of total.
        assert!(
            report.session.bytes_sent < payload.len() as u64 / 1000,
            "identical baseline should emit << 0.1% literal bytes, got {}",
            report.session.bytes_sent
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
        let report = transport
            .transfer(src.as_path(), dst.as_path())
            .await
            .expect("localized change path");

        let written = tokio::fs::read(&dst).await.unwrap();
        assert_eq!(written, payload);
        assert!(
            report.session.bytes_sent < payload.len() as u64 / 4,
            "localized change should match most blocks: sent {}",
            report.session.bytes_sent
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
            .transfer(src.as_path(), dst.as_path())
            .await
            .expect_err("too small");
        match err {
            TransferError::TooSmall { size, threshold } => {
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
            .transfer(src.as_path(), dst.as_path())
            .await
            .expect_err("missing source");
        match err {
            TransferError::Io(io) => {
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
            .transfer(src.as_path(), dst.as_path())
            .await
            .expect_err("oversized");
        match err {
            TransferError::Soft { detail } => {
                assert!(detail.contains("exceeds in-memory cap"));
            }
            other => panic!("expected a soft error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn sparse_transport_output_is_byte_identical() {
        let dir = tmp_dir();
        let src = dir.path().join("src.bin");
        let dst = dir.path().join("dst.bin");
        // 2 MiB with a large interior zero region; dense markers at the
        // ends so the file is not trivially empty.
        let mut payload = vec![0u8; 2 * 1024 * 1024];
        payload[..1024].fill(0x33);
        let n = payload.len();
        payload[n - 1024..].fill(0x44);
        tokio::fs::write(&src, &payload).await.unwrap();

        let transport = LocalDeltaTransport::new(1024).with_sparse(true);
        transport
            .transfer(src.as_path(), dst.as_path())
            .await
            .expect("sparse transfer");

        assert_eq!(
            tokio::fs::read(&dst).await.unwrap(),
            payload,
            "sparse transport must reconstruct byte-identical content"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn sparse_transport_allocates_fewer_blocks_than_dense() {
        use std::os::unix::fs::MetadataExt;
        let dir = tmp_dir();
        let src = dir.path().join("src.bin");
        let dense_dst = dir.path().join("dense.bin");
        let sparse_dst = dir.path().join("sparse.bin");
        let mut payload = vec![0u8; 2 * 1024 * 1024];
        payload[..16].fill(0x55);
        let n = payload.len();
        payload[n - 16..].fill(0x66);
        tokio::fs::write(&src, &payload).await.unwrap();

        LocalDeltaTransport::new(1024)
            .transfer(src.as_path(), dense_dst.as_path())
            .await
            .expect("dense transfer");
        LocalDeltaTransport::new(1024)
            .with_sparse(true)
            .transfer(src.as_path(), sparse_dst.as_path())
            .await
            .expect("sparse transfer");

        assert_eq!(
            tokio::fs::read(&dense_dst).await.unwrap(),
            tokio::fs::read(&sparse_dst).await.unwrap()
        );
        let dense_blocks = std::fs::metadata(&dense_dst).unwrap().blocks();
        let sparse_blocks = std::fs::metadata(&sparse_dst).unwrap().blocks();
        assert!(
            sparse_blocks < dense_blocks,
            "sparse transport must allocate fewer blocks: sparse={sparse_blocks} dense={dense_blocks}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn preserves_mode_on_unix() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tmp_dir();
        let src = dir.path().join("src.bin");
        let dst = dir.path().join("dst.bin");
        tokio::fs::write(&src, vec![0u8; 1024 * 1024 + 1])
            .await
            .unwrap();
        let perms = std::fs::Permissions::from_mode(0o640);
        std::fs::set_permissions(&src, perms).unwrap();

        let transport = LocalDeltaTransport::new(1024);
        transport
            .transfer(src.as_path(), dst.as_path())
            .await
            .expect("transfer");

        let dst_mode = std::fs::metadata(&dst).unwrap().permissions().mode() & 0o777;
        assert_eq!(dst_mode, 0o640);
    }
}
