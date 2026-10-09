// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! The files a speed test or a benchmark writes on a remote: their name, one
//! rule for every path that writes them (the GUI speed test, the CLI and
//! AeroAgent `benchmark`, `speed` and `speed-compare`, and AeroAgent's speed
//! tool), and the random content the CLI and the benchmark fill them with.
//!
//! The extension is `.dat`, never `.bin`. Koofr refuses to serve any `.bin`
//! file ("403 FileBlocked: File download restricted due to possible dangerous
//! content"), whatever it holds, a file of zeros included, while the same
//! bytes named `.dat` come back intact (measured 2026-10-05). With `.bin`
//! every download measurement on Koofr failed (#368).

/// Extension of every speed-test and benchmark payload.
pub const EXTENSION: &str = ".dat";

/// `stem` with the payload extension.
pub fn name(stem: &str) -> String {
    format!("{stem}{EXTENSION}")
}

/// Write a non-compressible random payload of `size` bytes to `path` and
/// return its hex SHA-256.
///
/// Uses `rand::thread_rng()` to generate high-entropy bytes (~8 bits/byte),
/// preventing TLS or transport-level compression from skewing the benchmark.
/// This is *high-entropy random* in the benchmarking sense, not a cryptographic
/// secrecy guarantee: the bytes are read back over the wire and hashed.
pub fn write_random(path: &std::path::Path, size: u64) -> Result<String, String> {
    use rand::RngCore;
    use sha2::{Digest, Sha256};
    use std::io::Write;
    let mut file = std::fs::File::create(path)
        .map_err(|e| format!("Cannot create speed test payload: {}", e))?;
    let mut rng = rand::thread_rng();
    let mut hasher = Sha256::new();
    let mut chunk = vec![0u8; 1024 * 1024];
    let mut remaining = size;
    while remaining > 0 {
        let next = remaining.min(chunk.len() as u64) as usize;
        rng.fill_bytes(&mut chunk[..next]);
        file.write_all(&chunk[..next])
            .map_err(|e| format!("Cannot write speed test payload: {}", e))?;
        hasher.update(&chunk[..next]);
        remaining -= next as u64;
    }
    file.flush()
        .map_err(|e| format!("Cannot flush speed test payload: {}", e))?;
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_payload_is_never_named_bin() {
        let name = name(".aeroftp-speedtest-x");
        assert_eq!(name, ".aeroftp-speedtest-x.dat");
        assert!(!name.ends_with(".bin"));
    }
}
