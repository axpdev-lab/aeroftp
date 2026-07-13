// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Compress overlay decorator (AeroCompress): a [`StorageProvider`] that wraps
//! an inner provider and applies transparent per-file zstd compression on the
//! content path (names unchanged).
//!
//! Used as the outer layer of the AeroCloud overlay stack (before crypt when
//! both enabled). 1:1 object model for P2/P3/P4: one remote object per logical
//! file. A compact header carries the mode + original plaintext length.
//!
//! Robustness (post-review fixes):
//! - **Streaming** upload/download so memory stays bounded regardless of file
//!   size (no whole-file-in-RAM materialization). download_to_bytes stays
//!   buffer-based by contract (small-file / compare callers only).
//! - **Stored mode**: if zstd does not shrink the file (already-compressed media,
//!   archives), the raw bytes are stored with a `stored` header flag instead of a
//!   larger zstd frame, so the overlay never enlarges an incompressible file
//!   beyond the tiny header.
//! - **Legacy passthrough**: an object WITHOUT the AECP magic (e.g. written
//!   before compression was enabled on the pair, or by another client) is passed
//!   through unchanged on download instead of failing, so enabling compression on
//!   an already-populated remote does not break reads.
//!
//! Design: reports_exact_size=false (like legacy crypt defer) so sync uses
//! timestamp primarily and avoids churn from "plain size != wire compressed size".

use async_trait::async_trait;
use std::io::{Read, Write};

use crate::providers::{ProviderError, ProviderType, RemoteEntry, StorageProvider};

use aerovault::v3::chunking::zstd_decompress_bounded;

/// On-wire header: magic(4) + mode(1) + u64 LE plaintext_len(8) = 13 bytes.
const COMPRESS_HEADER_MAGIC: &[u8; 4] = b"AECP";
const COMPRESS_HEADER_LEN: usize = 4 + 1 + 8;
/// zstd-compressed payload follows the header.
const MODE_ZSTD: u8 = 0;
/// raw (stored) payload follows the header (zstd did not shrink it).
const MODE_STORED: u8 = 1;

/// Build the on-wire header.
fn make_header(mode: u8, plain_len: u64) -> Vec<u8> {
    let mut h = Vec::with_capacity(COMPRESS_HEADER_LEN);
    h.extend_from_slice(COMPRESS_HEADER_MAGIC);
    h.push(mode);
    h.extend_from_slice(&plain_len.to_le_bytes());
    h
}

/// How a downloaded wire blob is laid out.
#[derive(Debug, PartialEq, Eq)]
enum WirePlan {
    /// No AECP magic: the whole blob is already plaintext, pass it through.
    Legacy,
    /// AECP stored: payload after the header is raw plaintext of `plain_len`.
    Stored(u64),
    /// AECP zstd: payload after the header is a zstd frame decompressing to `plain_len`.
    Zstd(u64),
}

/// Classify a wire prefix (at least the header, or the whole small blob).
fn parse_header_prefix(prefix: &[u8]) -> WirePlan {
    if prefix.len() < COMPRESS_HEADER_LEN || &prefix[0..4] != COMPRESS_HEADER_MAGIC {
        return WirePlan::Legacy;
    }
    let mode = prefix[4];
    let plain_len = u64::from_le_bytes(
        prefix[5..COMPRESS_HEADER_LEN]
            .try_into()
            .expect("13-byte prefix guaranteed above"),
    );
    match mode {
        MODE_STORED => WirePlan::Stored(plain_len),
        _ => WirePlan::Zstd(plain_len),
    }
}

/// The decorator.
pub struct CompressOverlayProvider {
    inner: Box<dyn StorageProvider>,
    level: i32,
}

impl CompressOverlayProvider {
    /// Wrap `inner` with zstd compression at `level` (recommended 1..=22; 3 balanced, >=19 archive).
    pub fn new(inner: Box<dyn StorageProvider>, level: i32) -> Self {
        Self { inner, level }
    }
}

#[async_trait]
impl StorageProvider for CompressOverlayProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn provider_type(&self) -> ProviderType {
        self.inner.provider_type()
    }

    fn display_name(&self) -> String {
        format!("{}+compress", self.inner.display_name())
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        self.inner.connect().await
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.inner.disconnect().await
    }

    fn is_connected(&self) -> bool {
        self.inner.is_connected()
    }

    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        // Names unchanged; sizes are wire (deferred) sizes. Sync will see
        // reports_exact_size=false and drop size-compare.
        self.inner.list(path).await
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> {
        self.inner.pwd().await
    }

    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        self.inner.cd(path).await
    }

    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        self.inner.cd_up().await
    }

    async fn download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        // Pull the wire to a temp, classify the header, then stream to local_path
        // with bounded memory (no whole-file materialization).
        let tmp = tempfile::NamedTempFile::new()
            .map_err(|e| ProviderError::TransferFailed(format!("tmp download: {e}")))?;
        let tmp_path = tmp.path().to_path_buf();
        self.inner
            .download(remote_path, tmp_path.to_str().unwrap(), on_progress)
            .await?;

        let mut wire = std::fs::File::open(&tmp_path)
            .map_err(|e| ProviderError::TransferFailed(format!("open tmp: {e}")))?;
        let mut prefix = Vec::with_capacity(COMPRESS_HEADER_LEN);
        std::io::Read::by_ref(&mut wire)
            .take(COMPRESS_HEADER_LEN as u64)
            .read_to_end(&mut prefix)
            .map_err(|e| ProviderError::TransferFailed(format!("read header: {e}")))?;
        let plan = parse_header_prefix(&prefix);

        let mut out = std::fs::File::create(local_path)
            .map_err(|e| ProviderError::TransferFailed(format!("create local: {e}")))?;
        match plan {
            WirePlan::Legacy => {
                // Whole blob is plaintext: emit the bytes we already read, then the rest.
                out.write_all(&prefix)
                    .map_err(|e| ProviderError::TransferFailed(format!("write local: {e}")))?;
                std::io::copy(&mut wire, &mut out)
                    .map_err(|e| ProviderError::TransferFailed(format!("copy local: {e}")))?;
            }
            WirePlan::Stored(plain_len) => {
                // Raw payload after the header, capped at plain_len.
                std::io::copy(&mut wire.take(plain_len), &mut out)
                    .map_err(|e| ProviderError::TransferFailed(format!("copy stored: {e}")))?;
            }
            WirePlan::Zstd(plain_len) => {
                // Streaming decode, output capped at plain_len (bounded, anti-bomb).
                let mut decoder = zstd::Decoder::new(std::io::BufReader::new(wire))
                    .map_err(|e| ProviderError::TransferFailed(format!("zstd decoder: {e}")))?;
                std::io::copy(&mut (&mut decoder).take(plain_len), &mut out)
                    .map_err(|e| ProviderError::TransferFailed(format!("decompress: {e}")))?;
            }
        }
        out.flush()
            .map_err(|e| ProviderError::TransferFailed(format!("flush local: {e}")))?;
        Ok(())
    }

    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        let wire = self.inner.download_to_bytes(remote_path).await?;
        match parse_header_prefix(&wire) {
            WirePlan::Legacy => Ok(wire),
            WirePlan::Stored(plain_len) => {
                let start = COMPRESS_HEADER_LEN;
                let end = start.saturating_add(plain_len as usize).min(wire.len());
                Ok(wire[start.min(wire.len())..end].to_vec())
            }
            WirePlan::Zstd(plain_len) => {
                zstd_decompress_bounded(&wire[COMPRESS_HEADER_LEN..], plain_len)
                    .map_err(|e| ProviderError::TransferFailed(format!("decompress: {e}")))
            }
        }
    }

    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        // Streaming: compress local -> temp, keep whichever is smaller (zstd vs
        // stored), prepend the header, upload. Bounded memory throughout.
        let plain_len = std::fs::metadata(local_path)
            .map_err(|e| ProviderError::TransferFailed(format!("stat local: {e}")))?
            .len();

        // Pass 1: stream-compress to a temp.
        let comp_tmp = tempfile::NamedTempFile::new()
            .map_err(|e| ProviderError::TransferFailed(format!("tmp comp: {e}")))?;
        {
            let mut reader = std::fs::File::open(local_path)
                .map_err(|e| ProviderError::TransferFailed(format!("open local: {e}")))?;
            let mut writer = comp_tmp
                .reopen()
                .map_err(|e| ProviderError::TransferFailed(format!("reopen comp: {e}")))?;
            zstd::stream::copy_encode(&mut reader, &mut writer, self.level)
                .map_err(|e| ProviderError::TransferFailed(format!("compress: {e}")))?;
            writer
                .flush()
                .map_err(|e| ProviderError::TransferFailed(format!("flush comp: {e}")))?;
        }
        let comp_len = comp_tmp
            .path()
            .metadata()
            .map_err(|e| ProviderError::TransferFailed(format!("stat comp: {e}")))?
            .len();

        // Build the final wire = header + smaller payload.
        let wire_tmp = tempfile::NamedTempFile::new()
            .map_err(|e| ProviderError::TransferFailed(format!("tmp wire: {e}")))?;
        {
            let mut w = std::io::BufWriter::new(
                wire_tmp
                    .reopen()
                    .map_err(|e| ProviderError::TransferFailed(format!("reopen wire: {e}")))?,
            );
            if comp_len < plain_len {
                w.write_all(&make_header(MODE_ZSTD, plain_len))
                    .map_err(|e| ProviderError::TransferFailed(format!("write header: {e}")))?;
                let mut r = std::fs::File::open(comp_tmp.path())
                    .map_err(|e| ProviderError::TransferFailed(format!("open comp: {e}")))?;
                std::io::copy(&mut r, &mut w)
                    .map_err(|e| ProviderError::TransferFailed(format!("copy comp: {e}")))?;
            } else {
                // zstd did not help: store raw so the object never grows beyond the header.
                w.write_all(&make_header(MODE_STORED, plain_len))
                    .map_err(|e| ProviderError::TransferFailed(format!("write header: {e}")))?;
                let mut r = std::fs::File::open(local_path)
                    .map_err(|e| ProviderError::TransferFailed(format!("reopen local: {e}")))?;
                std::io::copy(&mut r, &mut w)
                    .map_err(|e| ProviderError::TransferFailed(format!("copy raw: {e}")))?;
            }
            w.flush()
                .map_err(|e| ProviderError::TransferFailed(format!("flush wire: {e}")))?;
        }

        self.inner
            .upload(wire_tmp.path().to_str().unwrap(), remote_path, on_progress)
            .await
    }

    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        self.inner.mkdir(path).await
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        self.inner.delete(path).await
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        self.inner.rmdir(path).await
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        self.inner.rmdir_recursive(path).await
    }

    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        self.inner.rename(from, to).await
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        // Return wire size (deferred semantics). Caller sees logical name.
        self.inner.stat(path).await
    }

    async fn size(&mut self, path: &str) -> Result<u64, ProviderError> {
        self.inner.size(path).await
    }

    async fn exists(&mut self, path: &str) -> Result<bool, ProviderError> {
        self.inner.exists(path).await
    }

    async fn keep_alive(&mut self) -> Result<(), ProviderError> {
        self.inner.keep_alive().await
    }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        self.inner.server_info().await
    }

    fn reports_exact_size(&self) -> bool {
        // Compress changes size; we do not map wire->plain without I/O.
        // Sync honors this and drops size-compare (timestamp-driven).
        false
    }

    fn supports_checksum(&self) -> bool {
        // Wire checksum would be over compressed bytes; do not advertise.
        false
    }

    // Delegate the rest via defaults or explicit for the ones used in cloud/sync.
    // Add more explicit forwards here if a concrete provider path hits them.
}

#[cfg(test)]
mod tests {
    use super::*;
    use aerovault::v3::chunking::zstd_compress;

    #[test]
    fn header_zstd_roundtrip_and_bounded() {
        let plain = b"hello aerocompress world, this will be compressed a bit";
        let comp = zstd_compress(plain, 3).expect("compress");
        let mut wire = make_header(MODE_ZSTD, plain.len() as u64);
        wire.extend_from_slice(&comp);
        match parse_header_prefix(&wire) {
            WirePlan::Zstd(len) => {
                assert_eq!(len, plain.len() as u64);
                let out =
                    zstd_decompress_bounded(&wire[COMPRESS_HEADER_LEN..], len).expect("decompress");
                assert_eq!(out, plain);
            }
            other => panic!("expected Zstd, got {other:?}"),
        }
    }

    #[test]
    fn header_stored_classified() {
        let plain = b"already-compressed-ish bytes";
        let mut wire = make_header(MODE_STORED, plain.len() as u64);
        wire.extend_from_slice(plain);
        match parse_header_prefix(&wire) {
            WirePlan::Stored(len) => {
                assert_eq!(len, plain.len() as u64);
                assert_eq!(&wire[COMPRESS_HEADER_LEN..], plain);
            }
            other => panic!("expected Stored, got {other:?}"),
        }
    }

    #[test]
    fn legacy_passthrough_on_absent_magic() {
        // F1: a blob without the AECP magic (pre-compression object, foreign client)
        // is classified Legacy and passed through, not rejected.
        assert_eq!(parse_header_prefix(b"NOPE"), WirePlan::Legacy);
        assert_eq!(parse_header_prefix(b""), WirePlan::Legacy);
        assert_eq!(
            parse_header_prefix(b"plain text file contents with no header"),
            WirePlan::Legacy
        );
        // A short blob that happens to start with the magic but lacks a full header
        // is Legacy (we require the full 13-byte header).
        assert_eq!(parse_header_prefix(b"AECP"), WirePlan::Legacy);
    }
}
