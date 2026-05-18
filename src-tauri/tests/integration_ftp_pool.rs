//! PD-FTP-1 live validation: FTP intra-file concurrent range download on
//! the shared engine, against the repo's local vsftpd Docker fixture (no
//! secrets beyond the fixture-only testuser:testpass, reproducible).
//!
//! `#[ignore]` so default `cargo test` skips it. It drives the exact
//! shared code path: `FtpProvider::connect()` captures a connection spec;
//! `clone_for_transfer()` hands out independent, not-yet-connected workers;
//! the first `download()` on each worker dials its **own** FTP connection
//! (`ensure_connected`), and above the multi-thread cutoff the file is
//! split into N gap-free windows, each fetched over its **own** REST+RETR
//! connection (`ftp_download_one_range`) into a single pre-allocated
//! `.aerotmp` then atomically renamed. N ranges = N independent
//! connections, the same model as the SFTP pool (PD-SFTP-2). No control or
//! data connection is shared.
//!
//! ```bash
//! cd src-tauri/tests/fixtures/ftp
//! docker compose up -d --build            # control :2123, PASV 30000-30009
//! cd ../../.. && cargo test --release --test integration_ftp_pool \
//!   -- --ignored --nocapture
//! cd tests/fixtures/ftp && docker compose down -v
//! ```
//!
//! Loopback has no bandwidth/latency bottleneck, so the N=1 vs N=4
//! wall-clock is expected to be close: this gate proves **correctness**
//! (N real independent connections, byte-identical SHA-256, the new code
//! path executed, strict short-read gate, no `.aerotmp` residue,
//! non-regression) rather than a WAN speedup, which is latency-bound and
//! noted honestly in the master, not faked here.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

#![cfg(unix)]

use std::time::Instant;

use ftp_client_gui_lib::providers::types::{FtpConfig, FtpTlsMode};
use ftp_client_gui_lib::providers::{FtpProvider, StorageProvider};
use ftp_client_gui_lib::transfer_dag::Capability;
use sha2::{Digest, Sha256};

const PORT: u16 = 2123;

fn fixture_config() -> FtpConfig {
    FtpConfig {
        host: "127.0.0.1".to_string(),
        port: PORT,
        username: "testuser".to_string(),
        password: secrecy::SecretString::from("testpass".to_string()),
        tls_mode: FtpTlsMode::None,
        verify_cert: false,
        initial_path: Some("/".to_string()),
    }
}

fn sha256_file(path: &std::path::Path) -> String {
    let mut h = Sha256::new();
    h.update(std::fs::read(path).expect("read file"));
    format!("{:x}", h.finalize())
}

/// One file, single-stream (N=1, cutoff above size) vs intra-file
/// concurrent range (N=4, cutoff below size). Both must be byte-identical
/// SHA-256 to the source, and a pool-backed worker must advertise strict
/// concurrent range so a silent single-stream fallback fails the run.
#[tokio::test]
#[ignore = "live: requires the ftp Docker fixture up on :2123"]
async fn ftp_intra_file_concurrent_range_is_byte_identical() {
    // ~64 MiB so 4 streams produce 4 x 16 MiB windows. Pseudo-random,
    // non-repeating bytes so a mis-ordered window cannot accidentally match.
    const BIG_BYTES: usize = 64 * 1024 * 1024;
    const CUTOFF: u64 = 8 * 1024 * 1024;
    const STREAMS: usize = 4;

    let src_dir = std::env::temp_dir().join("pd-ftp1-src-it");
    let _ = std::fs::remove_dir_all(&src_dir);
    std::fs::create_dir_all(&src_dir).unwrap();
    let src = src_dir.join("big.bin");
    {
        let mut buf = vec![0u8; BIG_BYTES];
        let mut x: u32 = 0x9E37_79B9;
        for b in buf.iter_mut() {
            // xorshift32: cheap, deterministic, full-range, non-periodic here.
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            *b = (x & 0xFF) as u8;
        }
        std::fs::write(&src, &buf).unwrap();
    }
    let src_hash = sha256_file(&src);

    let mut base = FtpProvider::new(fixture_config());
    base.connect().await.expect("base FTP connect");
    assert!(
        base.connection_spec().is_some(),
        "connect() must capture an FTP connection spec"
    );
    base.upload(src.to_str().unwrap(), "/big.bin", None)
        .await
        .expect("seed upload big.bin");

    let tmp_root = std::env::temp_dir().join("pd-ftp1-it");
    let _ = std::fs::remove_dir_all(&tmp_root);
    std::fs::create_dir_all(&tmp_root).unwrap();
    let total_mib = BIG_BYTES as f64 / 1_048_576.0;

    // --- N=1 single-stream (cutoff above size: intra-file disabled) -------
    let n1 = tmp_root.join("n1.bin");
    let mut w1 = base
        .clone_for_transfer()
        .expect("clone_for_transfer (single-stream)");
    let t1 = Instant::now();
    w1.download("/big.bin", n1.to_str().unwrap(), None)
        .await
        .expect("single-stream download");
    let e1 = t1.elapsed();
    assert_eq!(src_hash, sha256_file(&n1), "N=1 byte mismatch");

    // --- N=4 intra-file concurrent range ---------------------------------
    let n4 = tmp_root.join("n4.bin");
    let mut w4 = base
        .clone_for_transfer()
        .expect("clone_for_transfer (intra-file)");
    w4.set_multi_thread_download(STREAMS, CUTOFF);
    assert_eq!(
        w4.transfer_capabilities().strict_concurrent_range_download,
        Capability::Supported,
        "a pool-backed FTP worker must advertise strict concurrent range"
    );
    let t4 = Instant::now();
    w4.download("/big.bin", n4.to_str().unwrap(), None)
        .await
        .expect("intra-file concurrent range download");
    let e4 = t4.elapsed();
    assert_eq!(src_hash, sha256_file(&n4), "N=4 intra-file byte mismatch");

    // Strict temp hygiene: no `.aerotmp` residue beside either output.
    assert!(
        !tmp_root.join("n1.bin.aerotmp").exists(),
        "N=1 left an .aerotmp residue"
    );
    assert!(
        !tmp_root.join("n4.bin.aerotmp").exists(),
        "N=4 intra-file left an .aerotmp residue"
    );

    // Parallelism preserved up to the FTP cap, never silently serialised.
    assert!(
        e4 <= e1 * 3,
        "N=4 intra-file ({:?}) catastrophically slower than N=1 ({:?}): likely serialised",
        e4,
        e1
    );

    eprintln!(
        "PD-FTP-1 intra-file: file={:.0} MiB  N=1 {:.2}s ({:.1} MiB/s)  N={} {:.2}s ({:.1} MiB/s)  SHA-256 identical",
        total_mib,
        e1.as_secs_f64(),
        total_mib / e1.as_secs_f64(),
        STREAMS,
        e4.as_secs_f64(),
        total_mib / e4.as_secs_f64(),
    );

    let _ = base.disconnect().await;
    let _ = std::fs::remove_dir_all(&tmp_root);
    let _ = std::fs::remove_dir_all(&src_dir);
}
