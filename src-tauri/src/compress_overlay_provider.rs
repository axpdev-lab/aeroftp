// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Compress overlay decorator (AeroCompress): a [`StorageProvider`] that wraps
//! an inner provider and applies transparent per-file zstd compression on the
//! content path (names unchanged).
//!
//! Used as the outer layer of the AeroCloud overlay stack (before crypt when
//! both enabled). 1:1 object model for P2/P3/P4: one remote object per logical
//! file. A compact header carries the original plaintext length so decompression
//! can use the bounded primitive.
//!
//! Design: reports_exact_size=false (like legacy crypt defer) so sync uses
//! timestamp primarily and avoids churn from "plain size != wire compressed size".
//! Content round-trips are authoritative.

use std::io::{Read, Write};

use async_trait::async_trait;

use crate::providers::{ProviderError, ProviderType, RemoteEntry, StorageProvider};

use aerovault::v3::chunking::{zstd_compress, zstd_decompress_bounded};

/// Magic + u64 LE plaintext_len prefix written before the zstd payload.
/// 12 bytes overhead per file. Enables safe bounded decompress.
const COMPRESS_HEADER_MAGIC: &[u8; 4] = b"AECP";
const COMPRESS_HEADER_LEN: usize = 4 + 8;

/// Build the on-wire header for a plaintext of `len` bytes.
fn make_header(plain_len: u64) -> Vec<u8> {
    let mut h = Vec::with_capacity(COMPRESS_HEADER_LEN);
    h.extend_from_slice(COMPRESS_HEADER_MAGIC);
    h.extend_from_slice(&plain_len.to_le_bytes());
    h
}

/// Parse header from a downloaded wire blob. Returns (plain_len, compressed_slice).
fn parse_header(wire: &[u8]) -> Result<(u64, &[u8]), String> {
    if wire.len() < COMPRESS_HEADER_LEN {
        return Err("AeroCompress header too short".to_string());
    }
    if &wire[0..4] != COMPRESS_HEADER_MAGIC {
        return Err("AeroCompress bad magic (not an AECP object)".to_string());
    }
    let len_bytes: [u8; 8] = wire[4..12]
        .try_into()
        .map_err(|_| "AeroCompress header length read failed".to_string())?;
    let plain_len = u64::from_le_bytes(len_bytes);
    Ok((plain_len, &wire[COMPRESS_HEADER_LEN..]))
}

/// Lightweight config for the compress layer (parsed from saved profile).
#[derive(Clone, Debug, Default)]
pub struct CompressConfig {
    pub enabled: bool,
    pub level: i32,
}

/// Extract `aeroCompress` from a saved profile JSON (parallel to aeroCryptOverlay).
/// Absent or disabled => enabled:false (level ignored). Default level 3 when enabled without level.
pub fn compress_config_from_profile(profile: &serde_json::Value) -> CompressConfig {
    let Some(c) = profile.get("aeroCompress") else {
        return CompressConfig {
            enabled: false,
            level: 3,
        };
    };
    if !c.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false) {
        return CompressConfig {
            enabled: false,
            level: 3,
        };
    }
    let level = c.get("level").and_then(|v| v.as_i64()).unwrap_or(3) as i32;
    CompressConfig {
        enabled: true,
        level,
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
        // Download the wire (headered-compressed or crypt-compressed) to a temp,
        // then decompress to the final local_path. Progress is on wire bytes.
        let tmp = tempfile::NamedTempFile::new()
            .map_err(|e| ProviderError::TransferFailed(format!("tmp download: {e}")))?;
        let tmp_path = tmp.path().to_path_buf();
        self.inner
            .download(remote_path, tmp_path.to_str().unwrap(), on_progress)
            .await?;
        let wire = std::fs::read(&tmp_path)
            .map_err(|e| ProviderError::TransferFailed(format!("read tmp: {e}")))?;
        let (plain_len, comp) =
            parse_header(&wire).map_err(|e| ProviderError::TransferFailed(e))?;
        let plain = zstd_decompress_bounded(comp, plain_len)
            .map_err(|e| ProviderError::TransferFailed(format!("decompress: {e}")))?;
        std::fs::write(local_path, plain)
            .map_err(|e| ProviderError::TransferFailed(format!("write local: {e}")))?;
        // NamedTempFile drops and cleans the tmp
        Ok(())
    }

    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        let wire = self.inner.download_to_bytes(remote_path).await?;
        let (plain_len, comp) =
            parse_header(&wire).map_err(|e| ProviderError::TransferFailed(e))?;
        zstd_decompress_bounded(comp, plain_len)
            .map_err(|e| ProviderError::TransferFailed(format!("decompress: {e}")))
    }

    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        // P2: use the exact aerovault primitive zstd_compress.
        // Read full (per-file model); prepend header; write to temp; upload.
        // (In-memory materialization of compressed bytes for the overlay layer.)
        let plain = std::fs::read(local_path)
            .map_err(|e| ProviderError::TransferFailed(format!("read local: {e}")))?;
        let plain_len = plain.len() as u64;
        let comp =
            zstd_compress(&plain, self.level).map_err(|e| ProviderError::TransferFailed(e))?;
        let mut wire = make_header(plain_len);
        wire.extend_from_slice(&comp);
        let tmp = tempfile::NamedTempFile::new()
            .map_err(|e| ProviderError::TransferFailed(format!("tmp upload: {e}")))?;
        let tmp_path = tmp.path().to_path_buf();
        std::fs::write(&tmp_path, &wire)
            .map_err(|e| ProviderError::TransferFailed(format!("write tmp: {e}")))?;
        let res = self
            .inner
            .upload(tmp_path.to_str().unwrap(), remote_path, on_progress)
            .await;
        let _ = std::fs::remove_file(&tmp_path);
        res
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

    #[test]
    fn header_roundtrip_and_bounded() {
        let plain = b"hello aerocompress world, this will be compressed a bit";
        let comp = zstd_compress(plain, 3).expect("compress");
        let mut wire = make_header(plain.len() as u64);
        wire.extend_from_slice(&comp);
        let (len, cslice) = parse_header(&wire).expect("parse");
        assert_eq!(len, plain.len() as u64);
        let out = zstd_decompress_bounded(cslice, len).expect("decompress");
        assert_eq!(out, plain);
    }

    #[test]
    fn bad_header_rejected() {
        assert!(parse_header(b"NOPE").is_err());
        let mut bad = make_header(10);
        bad[0] = b'X';
        assert!(parse_header(&bad).is_err());
    }

    #[test]
    fn p3_a2_compress_only_config_from_profile() {
        // A2: crypt flag OFF (absent or disabled) + aeroCompress enabled = compress-only transparent overlay.
        // Builder produces a stack with compress outer, no crypt layer.
        let prof = serde_json::json!({
            "name": "dev-a2",
            "aeroCompress": { "enabled": true, "level": 5 }
            // no aeroCryptOverlay -> A2
        });
        let c = compress_config_from_profile(&prof);
        assert!(c.enabled);
        assert_eq!(c.level, 5);

        let prof_off = serde_json::json!({
            "aeroCompress": { "enabled": false }
        });
        assert!(!compress_config_from_profile(&prof_off).enabled);

        // When both present, A1 path: compress wraps crypt (order asserted in builder logic).
        let prof_a1 = serde_json::json!({
            "aeroCryptOverlay": { "enabled": true, "kind": "aerocrypt" },
            "aeroCompress": { "enabled": true, "level": 3 }
        });
        assert!(compress_config_from_profile(&prof_a1).enabled);
    }
}
