//! Shared AeroVault telemetry: a version-agnostic technical "receipt" of what
//! happened behind the scenes during a vault operation (compression,
//! encryption, chunking, dedup), plus a human-readable step log.
//!
//! Designed to be global across AeroVault v1 / v2 / v3: each backend builds the
//! same `VaultReport`. v3 fills the chunk/dedup/CDC fields; v1/v2 leave them at
//! zero/None and still report algorithm chain, byte accounting, timing and the
//! step log. The struct is serde-serializable so it is both the in-app receipt
//! and the exportable artifact (CLI `--receipt`, GUI download).

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use serde::{Deserialize, Serialize};

/// Academic attribution. The wrapper / step taxonomy and the
/// compression -> chunking -> crypt -> error-correction pipeline that this
/// report describes were formalized as a design contribution by Ehud Kirsh
/// (GitHub @EhudKirsh) in AeroFTP issue #162. Carried in every receipt so the
/// exported artifact credits the model, not just the implementation.
pub const WRAPPER_MODEL_ATTRIBUTION: &str =
    "Wrapper-stack pipeline model: design contribution by Ehud Kirsh \
     (E. Kirsh), AeroFTP issue #162, 2026";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultReport {
    /// "create" | "add_files" | "extract" | ...
    pub operation: String,
    /// AeroVault on-disk format: 1, 2 or 3.
    pub vault_format: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Ordered wrapper chain, e.g. "packing:small-file-batching v1".
    pub algorithms: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cdc_min: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cdc_avg: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cdc_max: Option<usize>,
    pub files: u64,
    pub packed_files: u64,
    pub packs: u64,
    pub logical_chunks: u64,
    pub new_physical_chunks: u64,
    pub dedup_hits: u64,
    pub plaintext_bytes: u64,
    pub compressed_bytes: u64,
    pub encrypted_bytes: u64,
    /// Space saved by compression over plaintext, percent (negative if it grew).
    pub compression_ratio_pct: f64,
    pub ms_total: u64,
    /// Behind-the-scenes log, one line per meaningful action. Drives the
    /// in-modal "mini terminal" and the exportable receipt body.
    pub steps: Vec<String>,
    /// Academic attribution of the wrapper-stack model (see
    /// [`WRAPPER_MODEL_ATTRIBUTION`]). Always present in the receipt/export.
    pub attribution: String,
}

impl VaultReport {
    pub fn new(operation: &str, vault_format: u8) -> Self {
        Self {
            operation: operation.to_string(),
            vault_format,
            profile: None,
            algorithms: Vec::new(),
            cdc_min: None,
            cdc_avg: None,
            cdc_max: None,
            files: 0,
            packed_files: 0,
            packs: 0,
            logical_chunks: 0,
            new_physical_chunks: 0,
            dedup_hits: 0,
            plaintext_bytes: 0,
            compressed_bytes: 0,
            encrypted_bytes: 0,
            compression_ratio_pct: 0.0,
            ms_total: 0,
            steps: Vec::new(),
            attribution: WRAPPER_MODEL_ATTRIBUTION.to_string(),
        }
    }

    pub fn set_profile(&mut self, profile: &str) {
        self.profile = Some(profile.to_string());
    }

    pub fn set_algorithms(&mut self, algos: Vec<String>) {
        self.algorithms = algos;
    }

    pub fn set_cdc(&mut self, min: usize, avg: usize, max: usize) {
        self.cdc_min = Some(min);
        self.cdc_avg = Some(avg);
        self.cdc_max = Some(max);
    }

    pub fn step<S: Into<String>>(&mut self, line: S) {
        self.steps.push(line.into());
    }

    /// One chunk passed through compress + encrypt (or was deduplicated).
    pub fn on_chunk(
        &mut self,
        is_new_physical: bool,
        plaintext: u64,
        compressed: u64,
        encrypted: u64,
    ) {
        self.logical_chunks += 1;
        if is_new_physical {
            self.new_physical_chunks += 1;
            self.plaintext_bytes += plaintext;
            self.compressed_bytes += compressed;
            self.encrypted_bytes += encrypted;
        } else {
            self.dedup_hits += 1;
        }
    }

    pub fn on_file(&mut self, packed: bool) {
        self.files += 1;
        if packed {
            self.packed_files += 1;
        }
    }

    pub fn on_pack(&mut self) {
        self.packs += 1;
    }

    /// Finalize derived metrics and total elapsed time.
    pub fn finish(&mut self, ms_total: u64) {
        self.ms_total = ms_total;
        // Only meaningful when a compression stage actually ran and recorded
        // its output (v3). v2/v1 leave compressed_bytes at 0 and report 0.0
        // rather than a misleading 100%.
        self.compression_ratio_pct = if self.plaintext_bytes > 0 && self.compressed_bytes > 0 {
            (1.0 - (self.compressed_bytes as f64 / self.plaintext_bytes as f64)) * 100.0
        } else {
            0.0
        };
    }

    /// Plain-text rendering for CLI stderr / a downloadable `.txt` receipt.
    pub fn render_text(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "AeroVault technical receipt | op={} format=v{}",
            self.operation, self.vault_format
        ));
        if let Some(p) = &self.profile {
            out.push_str(&format!(" profile={p}"));
        }
        out.push('\n');
        if !self.algorithms.is_empty() {
            out.push_str(&format!("pipeline: {}\n", self.algorithms.join(" -> ")));
        }
        if let (Some(mn), Some(av), Some(mx)) = (self.cdc_min, self.cdc_avg, self.cdc_max) {
            out.push_str(&format!("cdc bounds: min={mn} avg={av} max={mx}\n"));
        }
        out.push_str(&format!(
            "files={} (packed={}, packs={}) chunks: logical={} new={} dedup={}\n",
            self.files,
            self.packed_files,
            self.packs,
            self.logical_chunks,
            self.new_physical_chunks,
            self.dedup_hits
        ));
        out.push_str(&format!(
            "bytes: plaintext={} compressed={} encrypted={} ratio={:.1}%\n",
            self.plaintext_bytes,
            self.compressed_bytes,
            self.encrypted_bytes,
            self.compression_ratio_pct
        ));
        out.push_str(&format!("elapsed: {} ms\n", self.ms_total));
        out.push_str("steps:\n");
        for s in &self.steps {
            out.push_str(&format!("  {s}\n"));
        }
        out.push_str(&format!("\n{}\n", self.attribution));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_accounting_and_ratio() {
        let mut r = VaultReport::new("add_files", 3);
        r.set_profile("balanced");
        r.set_algorithms(vec!["compression:zstd v1".into()]);
        r.set_cdc(262144, 1048576, 4194304);
        r.on_file(true);
        r.on_pack();
        r.on_chunk(true, 1000, 400, 430);
        r.on_chunk(false, 1000, 400, 430); // dedup hit
        r.finish(12);
        assert_eq!(r.files, 1);
        assert_eq!(r.packs, 1);
        assert_eq!(r.logical_chunks, 2);
        assert_eq!(r.new_physical_chunks, 1);
        assert_eq!(r.dedup_hits, 1);
        assert_eq!(r.plaintext_bytes, 1000);
        assert!((r.compression_ratio_pct - 60.0).abs() < 1e-9);
        assert!(r.render_text().contains("pipeline: compression:zstd v1"));
    }
}
