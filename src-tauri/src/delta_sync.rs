// AeroFTP Delta Sync Module
// rsync-style rolling checksum + block matching for efficient file transfers
#![allow(dead_code)]
//
// Algorithm:
// 1. Destination: compute block signatures (rolling Adler-32 + strong SHA-256)
// 2. Source: rolling hash over file, match against signature table
// 3. Emit delta: sequence of (CopyBlock(N) | Literal(bytes))
// 4. Destination: reconstruct file from delta instructions
//
// Only applicable for files > 1MB existing on both sides.
// Provider must support read_range() (SFTP via seek).

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

/// Minimum file size for delta sync (1 MB)
pub const DELTA_MIN_FILE_SIZE: u64 = 1_048_576;

/// Maximum block size (8 KB)
const MAX_BLOCK_SIZE: usize = 8192;

/// Minimum block size (512 bytes)
const MIN_BLOCK_SIZE: usize = 512;

/// If delta exceeds this ratio of file size, fall back to full transfer
const DELTA_RATIO_THRESHOLD: f64 = 0.80;

/// Adler-32 modulus constant
const ADLER_MOD: u32 = 65521;

/// Compute adaptive block size based on file size
pub fn compute_block_size(file_size: u64) -> usize {
    let bs = (file_size as f64).sqrt() as usize;
    bs.clamp(MIN_BLOCK_SIZE, MAX_BLOCK_SIZE)
}

/// Rolling Adler-32 checksum for a window
#[derive(Debug, Clone)]
pub struct RollingChecksum {
    a: u32,
    b: u32,
    window_size: usize,
}

impl RollingChecksum {
    /// Initialize from a data block
    pub fn new(data: &[u8]) -> Self {
        let mut a: u32 = 1;
        let mut b: u32 = 0;
        for &byte in data {
            a = (a + byte as u32) % ADLER_MOD;
            b = (b + a) % ADLER_MOD;
        }
        Self {
            a,
            b,
            window_size: data.len(),
        }
    }

    /// Get the current checksum value
    pub fn value(&self) -> u32 {
        (self.b << 16) | self.a
    }

    /// Roll the window: remove old_byte from front, add new_byte to end
    pub fn roll(&mut self, old_byte: u8, new_byte: u8) {
        let old = old_byte as u32;
        let new = new_byte as u32;
        // Update a: add new byte, remove old byte (mod ADLER_MOD)
        self.a = (self.a + ADLER_MOD + new - old) % ADLER_MOD;
        // Update b: remove contribution of old byte * window_size + 1, add new a
        let remove = (self.window_size as u32 * old + 1) % ADLER_MOD;
        self.b = (self.b + ADLER_MOD + self.a - remove) % ADLER_MOD;
    }
}

/// Strong hash (SHA-256) for block verification
pub fn strong_hash(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let result = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&result);
    out
}

/// A block signature: rolling checksum + strong hash + block index
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockSignature {
    pub index: u32,
    pub rolling: u32,
    pub strong: [u8; 32],
    pub size: u32,
}

/// Signature table for a file (computed on destination side)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignatureTable {
    pub block_size: usize,
    pub file_size: u64,
    pub signatures: Vec<BlockSignature>,
}

/// Delta instruction
#[derive(Debug, Clone)]
pub enum DeltaOp {
    /// Copy block N from the destination file
    CopyBlock(u32),
    /// Literal bytes that don't match any block
    Literal(Vec<u8>),
}

/// Delta result containing instructions and stats
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeltaResult {
    pub block_size: usize,
    pub source_size: u64,
    pub dest_size: u64,
    pub copy_blocks: u32,
    pub literal_bytes: u64,
    pub total_delta_bytes: u64,
    pub savings_ratio: f64,
    pub should_use_delta: bool,
}

/// Compute block signatures for a file (destination side)
pub fn compute_signatures(data: &[u8], block_size: usize) -> SignatureTable {
    let mut signatures = Vec::new();
    let mut offset = 0usize;
    let mut index = 0u32;

    while offset < data.len() {
        let end = (offset + block_size).min(data.len());
        let block = &data[offset..end];

        let rolling = RollingChecksum::new(block);
        let strong = strong_hash(block);

        signatures.push(BlockSignature {
            index,
            rolling: rolling.value(),
            strong,
            size: block.len() as u32,
        });

        offset = end;
        index += 1;
    }

    SignatureTable {
        block_size,
        file_size: data.len() as u64,
        signatures,
    }
}

/// Build a lookup table from rolling checksums to signature indices
fn build_rolling_lookup(sigs: &[BlockSignature]) -> HashMap<u32, Vec<usize>> {
    let mut map: HashMap<u32, Vec<usize>> = HashMap::new();
    for (i, sig) in sigs.iter().enumerate() {
        map.entry(sig.rolling).or_default().push(i);
    }
    map
}

/// Compute delta between source data and destination signatures
pub fn compute_delta(source_data: &[u8], sig_table: &SignatureTable) -> (Vec<DeltaOp>, DeltaResult) {
    let block_size = sig_table.block_size;
    let lookup = build_rolling_lookup(&sig_table.signatures);

    let mut ops: Vec<DeltaOp> = Vec::new();
    let mut literal_buf: Vec<u8> = Vec::new();
    let mut copy_blocks: u32 = 0;
    let mut literal_total: u64 = 0;

    if source_data.len() < block_size {
        // File smaller than block size — send as literal
        return (
            vec![DeltaOp::Literal(source_data.to_vec())],
            DeltaResult {
                block_size,
                source_size: source_data.len() as u64,
                dest_size: sig_table.file_size,
                copy_blocks: 0,
                literal_bytes: source_data.len() as u64,
                total_delta_bytes: source_data.len() as u64,
                savings_ratio: 0.0,
                should_use_delta: false,
            },
        );
    }

    let mut pos = 0usize;

    // Initialize rolling checksum for first window
    let first_window = &source_data[..block_size.min(source_data.len())];
    let mut rolling = RollingChecksum::new(first_window);

    loop {
        if pos + block_size > source_data.len() {
            // Remaining bytes smaller than block_size — emit as literal
            literal_buf.extend_from_slice(&source_data[pos..]);
            break;
        }

        let rolling_val = rolling.value();

        // Check if rolling checksum matches any signature
        let mut matched = false;
        if let Some(candidates) = lookup.get(&rolling_val) {
            let window = &source_data[pos..pos + block_size];
            let strong = strong_hash(window);

            for &idx in candidates {
                if sig_table.signatures[idx].strong == strong {
                    // Match found! Flush literal buffer first
                    if !literal_buf.is_empty() {
                        literal_total += literal_buf.len() as u64;
                        ops.push(DeltaOp::Literal(std::mem::take(&mut literal_buf)));
                    }
                    ops.push(DeltaOp::CopyBlock(sig_table.signatures[idx].index));
                    copy_blocks += 1;
                    pos += block_size;
                    matched = true;

                    // Re-initialize rolling checksum for next window
                    if pos + block_size <= source_data.len() {
                        rolling = RollingChecksum::new(&source_data[pos..pos + block_size]);
                    }
                    break;
                }
            }
        }

        if !matched {
            // No match — add byte to literal buffer and roll window by 1
            literal_buf.push(source_data[pos]);
            if pos + block_size < source_data.len() {
                rolling.roll(source_data[pos], source_data[pos + block_size]);
            }
            pos += 1;
        }
    }

    // Flush remaining literal
    if !literal_buf.is_empty() {
        literal_total += literal_buf.len() as u64;
        ops.push(DeltaOp::Literal(literal_buf));
    }

    let total_delta = literal_total + (copy_blocks as u64 * 8); // 8 bytes per copy instruction
    let savings = if sig_table.file_size > 0 {
        1.0 - (total_delta as f64 / sig_table.file_size as f64)
    } else {
        0.0
    };

    let result = DeltaResult {
        block_size,
        source_size: source_data.len() as u64,
        dest_size: sig_table.file_size,
        copy_blocks,
        literal_bytes: literal_total,
        total_delta_bytes: total_delta,
        savings_ratio: savings,
        should_use_delta: savings > (1.0 - DELTA_RATIO_THRESHOLD),
    };

    (ops, result)
}

/// Reconstruct a file from the original (destination) data and delta instructions
pub fn apply_delta(dest_data: &[u8], ops: &[DeltaOp], block_size: usize) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();

    for op in ops {
        match op {
            DeltaOp::CopyBlock(idx) => {
                let offset = *idx as usize * block_size;
                if offset >= dest_data.len() {
                    return Err(format!(
                        "CopyBlock index {} out of range (dest size: {})",
                        idx,
                        dest_data.len()
                    ));
                }
                let end = (offset + block_size).min(dest_data.len());
                output.extend_from_slice(&dest_data[offset..end]);
            }
            DeltaOp::Literal(bytes) => {
                output.extend_from_slice(bytes);
            }
        }
    }

    Ok(output)
}

/// Serialize delta operations to a compact binary format
/// Format: [op_type(1 byte)] [data]
/// CopyBlock: 0x01 + block_index(u32 LE)
/// Literal:   0x02 + length(u32 LE) + bytes
pub fn serialize_delta(ops: &[DeltaOp]) -> Vec<u8> {
    let mut out = Vec::new();
    for op in ops {
        match op {
            DeltaOp::CopyBlock(idx) => {
                out.push(0x01);
                out.extend_from_slice(&idx.to_le_bytes());
            }
            DeltaOp::Literal(bytes) => {
                out.push(0x02);
                out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
                out.extend_from_slice(bytes);
            }
        }
    }
    out
}

/// Deserialize delta operations from binary format
pub fn deserialize_delta(data: &[u8]) -> Result<Vec<DeltaOp>, String> {
    let mut ops = Vec::new();
    let mut pos = 0usize;

    while pos < data.len() {
        match data[pos] {
            0x01 => {
                if pos + 5 > data.len() {
                    return Err("Truncated CopyBlock".to_string());
                }
                let idx = u32::from_le_bytes([data[pos + 1], data[pos + 2], data[pos + 3], data[pos + 4]]);
                ops.push(DeltaOp::CopyBlock(idx));
                pos += 5;
            }
            0x02 => {
                if pos + 5 > data.len() {
                    return Err("Truncated Literal header".to_string());
                }
                let len = u32::from_le_bytes([data[pos + 1], data[pos + 2], data[pos + 3], data[pos + 4]]) as usize;
                const MAX_LITERAL_SIZE: usize = 64 * 1024 * 1024; // 64 MB max per literal block
                if len > MAX_LITERAL_SIZE {
                    return Err(format!(
                        "Literal block too large: {} bytes (max {})",
                        len, MAX_LITERAL_SIZE
                    ));
                }
                pos += 5;
                if pos + len > data.len() {
                    return Err("Truncated Literal data".to_string());
                }
                ops.push(DeltaOp::Literal(data[pos..pos + len].to_vec()));
                pos += len;
            }
            other => {
                return Err(format!("Unknown delta op: 0x{:02x}", other));
            }
        }
    }

    Ok(ops)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_block_size_computation() {
        assert_eq!(compute_block_size(1_048_576), 1024); // 1MB → 1024
        assert_eq!(compute_block_size(100_000_000), MAX_BLOCK_SIZE); // 100MB → capped at 8KB
        assert_eq!(compute_block_size(100_000), MIN_BLOCK_SIZE); // 100KB → min 512
    }

    #[test]
    fn test_rolling_checksum() {
        let data = b"Hello, World!";
        let rc = RollingChecksum::new(data);
        assert_ne!(rc.value(), 0);
    }

    #[test]
    fn test_rolling_checksum_roll() {
        let data = b"abcdef";
        let mut rc = RollingChecksum::new(&data[0..4]); // "abcd"
        let v1 = rc.value();
        rc.roll(b'a', b'e'); // "bcde"
        let v2 = rc.value();
        assert_ne!(v1, v2);

        // Verify: fresh checksum of "bcde" should match
        let rc2 = RollingChecksum::new(&data[1..5]);
        assert_eq!(rc.value(), rc2.value());
    }

    #[test]
    fn test_strong_hash_deterministic() {
        let data = b"test data";
        let h1 = strong_hash(data);
        let h2 = strong_hash(data);
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_compute_signatures() {
        let data = vec![0u8; 2048];
        let sigs = compute_signatures(&data, 512);
        assert_eq!(sigs.signatures.len(), 4);
        assert_eq!(sigs.block_size, 512);
        assert_eq!(sigs.file_size, 2048);
    }

    #[test]
    fn test_identical_file_delta() {
        let data = vec![42u8; 4096];
        let sigs = compute_signatures(&data, 512);
        let (ops, result) = compute_delta(&data, &sigs);

        // All blocks should match
        assert_eq!(result.copy_blocks, 8);
        assert_eq!(result.literal_bytes, 0);
        assert!(result.savings_ratio > 0.9);
        assert!(result.should_use_delta);

        // Reconstruct should be identical
        let reconstructed = apply_delta(&data, &ops, 512).unwrap();
        assert_eq!(reconstructed, data);
    }

    #[test]
    fn test_small_change_delta() {
        let mut original = vec![0u8; 4096];
        for i in 0..4096 {
            original[i] = (i % 256) as u8;
        }

        // Modify a small portion in the middle
        let mut modified = original.clone();
        for i in 1024..1030 {
            modified[i] = 0xFF;
        }

        let sigs = compute_signatures(&original, 512);
        let (ops, result) = compute_delta(&modified, &sigs);

        // Most blocks should match, some literals for the changed section
        assert!(result.copy_blocks > 0);
        assert!(result.literal_bytes < 2048); // Much less than full file

        // Reconstruct should match modified version
        let reconstructed = apply_delta(&original, &ops, 512).unwrap();
        assert_eq!(reconstructed, modified);
    }

    #[test]
    fn test_completely_different_file() {
        let original = vec![0u8; 4096];
        let modified = vec![0xFFu8; 4096];

        let sigs = compute_signatures(&original, 512);
        let (_, result) = compute_delta(&modified, &sigs);

        // No blocks should match
        assert_eq!(result.copy_blocks, 0);
        assert_eq!(result.literal_bytes, 4096);
        assert!(!result.should_use_delta);
    }

    #[test]
    fn test_delta_serialization_roundtrip() {
        let ops = vec![
            DeltaOp::CopyBlock(0),
            DeltaOp::Literal(vec![1, 2, 3]),
            DeltaOp::CopyBlock(2),
            DeltaOp::Literal(vec![4, 5]),
        ];

        let serialized = serialize_delta(&ops);
        let deserialized = deserialize_delta(&serialized).unwrap();

        assert_eq!(deserialized.len(), 4);
        match &deserialized[0] {
            DeltaOp::CopyBlock(0) => {}
            _ => panic!("Expected CopyBlock(0)"),
        }
        match &deserialized[1] {
            DeltaOp::Literal(bytes) => assert_eq!(bytes, &[1, 2, 3]),
            _ => panic!("Expected Literal"),
        }
    }

    #[test]
    fn test_delta_reconstruction_with_serialization() {
        let mut original = vec![0u8; 2048];
        for i in 0..2048 {
            original[i] = (i * 7 % 256) as u8;
        }

        let mut modified = original.clone();
        modified[500] = 0xFF;
        modified[501] = 0xFE;

        let sigs = compute_signatures(&original, 512);
        let (ops, _) = compute_delta(&modified, &sigs);

        // Serialize → deserialize → reconstruct
        let binary = serialize_delta(&ops);
        let ops2 = deserialize_delta(&binary).unwrap();
        let reconstructed = apply_delta(&original, &ops2, 512).unwrap();
        assert_eq!(reconstructed, modified);
    }

    #[test]
    fn test_empty_delta_deserialization() {
        let ops = deserialize_delta(&[]).unwrap();
        assert!(ops.is_empty());
    }

    #[test]
    fn test_truncated_delta_error() {
        assert!(deserialize_delta(&[0x01]).is_err());
        assert!(deserialize_delta(&[0x02, 0x05, 0x00, 0x00, 0x00]).is_err());
        assert!(deserialize_delta(&[0xFF]).is_err());
    }
}
