//! PD-SFTP-1 live validation: the shared-engine SFTP connection pool,
//! against the repo's local SFTP Docker fixture (no secrets, reproducible).
//!
//! `#[ignore]` so default `cargo test` skips it. It drives the exact
//! shared code path the GUI orchestrator uses: `SftpProvider::connect()`
//! captures a secure `SftpConnectionSpec`; `clone_for_transfer()` hands
//! out independent, not-yet-connected workers; the first `download()` on
//! each worker dials its **own** SSH connection (`ensure_connected`,
//! host-key pinned re-dial), so N files transfer over N independent
//! connections, exactly like the FTP pool. No SSH handle/channel shared.
//!
//! ```bash
//! cd src-tauri/tests/fixtures/sftp-rsync
//! ./setup.sh && docker compose up -d --build      # key-auth on :2222
//! cd ../../.. && cargo test --release --test integration_sftp_pool \
//!   -- --ignored --nocapture
//! cd tests/fixtures/sftp-rsync && docker compose down -v
//! ```
//!
//! Loopback has no bandwidth/latency bottleneck, so the N=1 vs N=3 vs
//! N=5 wall-clock is expected to be close: this gate proves
//! **correctness** (N real independent connections, byte-identical
//! SHA-256, the new code path executed, pinned re-dial works,
//! non-regression). A WAN throughput delta is latency-bound and noted
//! honestly in the master, not faked here.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command as StdCommand;
use std::time::Instant;

use ftp_client_gui_lib::providers::sftp::SftpProvider;
use ftp_client_gui_lib::providers::types::SftpConfig;
use ftp_client_gui_lib::providers::{ProviderTransferExecutorKind, StorageProvider};
use ftp_client_gui_lib::transfer_dag::Capability;
use futures_util::StreamExt;
use sha2::{Digest, Sha256};

const PORT: u16 = 2222;
const FILES: usize = 8;
const FILE_BYTES: usize = 16 * 1024 * 1024;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sftp-rsync")
}

fn ssh_key_path() -> PathBuf {
    fixture_dir().join("ssh_key")
}

/// Seed the fixture host key into `~/.ssh/known_hosts` so the pooled
/// re-dials (which force `trust_unknown_hosts = false`) verify cleanly.
fn seed_known_host() {
    let home = std::env::var("HOME").expect("HOME");
    let ssh_dir = PathBuf::from(home).join(".ssh");
    std::fs::create_dir_all(&ssh_dir).ok();
    let known_hosts = ssh_dir.join("known_hosts");
    let host = format!("[127.0.0.1]:{}", PORT);
    let _ = StdCommand::new("ssh-keygen")
        .args(["-R", &host, "-f", known_hosts.to_str().unwrap()])
        .output();
    let scan = StdCommand::new("ssh-keyscan")
        .args(["-p", &PORT.to_string(), "127.0.0.1"])
        .output()
        .expect("ssh-keyscan");
    assert!(scan.status.success(), "ssh-keyscan failed");
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&known_hosts)
        .expect("open known_hosts");
    f.write_all(&scan.stdout).expect("append known_hosts");
}

fn fixture_config() -> SftpConfig {
    SftpConfig {
        host: "127.0.0.1".to_string(),
        port: PORT,
        username: "testuser".to_string(),
        password: None,
        private_key_path: Some(ssh_key_path().to_string_lossy().to_string()),
        key_passphrase: None,
        initial_path: Some("/workdir".to_string()),
        timeout_secs: 30,
        trust_unknown_hosts: true,
    }
}

fn sha256_file(path: &std::path::Path) -> String {
    let mut h = Sha256::new();
    h.update(std::fs::read(path).expect("read file"));
    format!("{:x}", h.finalize())
}

#[tokio::test]
#[ignore = "live: requires the sftp-rsync Docker fixture up on :2222"]
async fn sftp_connection_pool_parallel_download_is_real_and_byte_identical() {
    if !ssh_key_path().exists() {
        eprintln!("SKIP: fixture ssh_key missing (run fixtures/sftp-rsync/setup.sh)");
        return;
    }
    seed_known_host();

    // Local source dataset.
    let src_dir = std::env::temp_dir().join("pd-sftp1-src-it");
    let _ = std::fs::remove_dir_all(&src_dir);
    std::fs::create_dir_all(&src_dir).unwrap();
    let mut src_hashes = std::collections::HashMap::new();
    for i in 0..FILES {
        let name = format!("f{i}.bin");
        let p = src_dir.join(&name);
        let mut buf = vec![0u8; FILE_BYTES];
        for (j, b) in buf.iter_mut().enumerate() {
            *b = ((i * 131 + j * 7) % 251) as u8;
        }
        std::fs::write(&p, &buf).unwrap();
        src_hashes.insert(name, sha256_file(&p));
    }

    // Base provider: connect once, capture the secure spec, seed remote.
    let mut base = SftpProvider::new(fixture_config());
    base.connect().await.expect("base SFTP connect");
    assert!(
        base.connection_spec().is_some(),
        "connect() must capture a secure SftpConnectionSpec"
    );
    assert_eq!(
        base.transfer_executor_kind(),
        ProviderTransferExecutorKind::SftpConnectionPool,
        "a connected SFTP provider must advertise the connection pool"
    );

    for i in 0..FILES {
        let name = format!("f{i}.bin");
        base.upload(
            src_dir.join(&name).to_str().unwrap(),
            &format!("/workdir/{name}"),
            None,
        )
        .await
        .unwrap_or_else(|e| panic!("seed upload {name} failed: {e}"));
    }

    let files: Vec<String> = (0..FILES).map(|i| format!("/workdir/f{i}.bin")).collect();
    let total_mib = (FILES * FILE_BYTES) as f64 / 1_048_576.0;

    let tmp_root = std::env::temp_dir().join("pd-sftp1-it");
    let _ = std::fs::remove_dir_all(&tmp_root);

    for concurrency in [1usize, 3, 5] {
        let run_dir = tmp_root.join(format!("c{concurrency}"));
        std::fs::create_dir_all(&run_dir).unwrap();

        let started = Instant::now();
        let results: Vec<(String, PathBuf)> =
            futures_util::stream::iter(files.clone().into_iter().map(|remote| {
                let base = &base;
                let run_dir = run_dir.clone();
                async move {
                    let name = remote.rsplit('/').next().unwrap().to_string();
                    let local = run_dir.join(&name);
                    // Independent worker -> its own SSH connection.
                    let mut worker = base
                        .clone_for_transfer()
                        .expect("clone_for_transfer must succeed for connected SFTP");
                    worker
                        .download(&remote, local.to_str().unwrap(), None)
                        .await
                        .unwrap_or_else(|e| panic!("download {remote} failed: {e}"));
                    (name, local)
                }
            }))
            .buffer_unordered(concurrency)
            .collect()
            .await;

        let elapsed = started.elapsed();
        eprintln!(
            "C={concurrency:<2} elapsed={:>6.2}s  {:>6.1} MiB/s  ({} files)",
            elapsed.as_secs_f64(),
            total_mib / elapsed.as_secs_f64(),
            results.len()
        );

        assert_eq!(results.len(), FILES, "all files must download at C={concurrency}");
        for (name, local) in &results {
            assert_eq!(
                src_hashes.get(name),
                Some(&sha256_file(local)),
                "byte mismatch for {name} at C={concurrency}"
            );
        }
    }

    let _ = base.disconnect().await;
    let _ = std::fs::remove_dir_all(&tmp_root);
    let _ = std::fs::remove_dir_all(&src_dir);
    eprintln!("PD-SFTP-1: byte-identical across C=1/3/5; independent-connection pool path exercised.");
}

/// PD-SFTP-2 live validation: intra-file parallelism. ONE large file is
/// downloaded single-stream (N=1) and then split into N=4 concurrent range
/// windows, each over its own independent SSH connection, and re-assembled.
/// The gate is **correctness**: byte-identical SHA-256 across source / N=1 /
/// N=4 proves the range plan + strict-length per-window writer + offset
/// assembly are correct, and zero `.aerotmp` residue proves the
/// TempFileGuard + atomic rename. The intra-file path is taken
/// deterministically (cutoff 8 MiB < file size, streams = 4) and the
/// capability flip is asserted, so a silent single-stream fallback fails
/// the run.
#[tokio::test]
#[ignore = "live: requires the sftp-rsync Docker fixture up on :2222"]
async fn sftp_intra_file_concurrent_range_is_byte_identical() {
    if !ssh_key_path().exists() {
        eprintln!("SKIP: fixture ssh_key missing (run fixtures/sftp-rsync/setup.sh)");
        return;
    }
    seed_known_host();

    // ~64 MiB so 4 streams produce 4 x 16 MiB windows. Pseudo-random,
    // non-repeating bytes so a mis-ordered window cannot accidentally match.
    const BIG_BYTES: usize = 64 * 1024 * 1024;
    const CUTOFF: u64 = 8 * 1024 * 1024;
    const STREAMS: usize = 4;

    let src_dir = std::env::temp_dir().join("pd-sftp2-src-it");
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

    let mut base = SftpProvider::new(fixture_config());
    base.connect().await.expect("base SFTP connect");
    assert!(
        base.connection_spec().is_some(),
        "connect() must capture a secure SftpConnectionSpec"
    );
    base.upload(src.to_str().unwrap(), "/workdir/big.bin", None)
        .await
        .expect("seed upload big.bin");

    let tmp_root = std::env::temp_dir().join("pd-sftp2-it");
    let _ = std::fs::remove_dir_all(&tmp_root);
    std::fs::create_dir_all(&tmp_root).unwrap();
    let total_mib = BIG_BYTES as f64 / 1_048_576.0;

    // --- N=1 single-stream (cutoff above size: intra-file disabled) -------
    let n1 = tmp_root.join("n1.bin");
    let mut w1 = base
        .clone_for_transfer()
        .expect("clone_for_transfer (single-stream)");
    let t1 = Instant::now();
    w1.download("/workdir/big.bin", n1.to_str().unwrap(), None)
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
        "a pool-backed SFTP worker must advertise strict concurrent range"
    );
    let t4 = Instant::now();
    w4.download("/workdir/big.bin", n4.to_str().unwrap(), None)
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

    eprintln!(
        "PD-SFTP-2 intra-file: file={:.0} MiB  N=1 {:.2}s ({:.1} MiB/s)  N={} {:.2}s ({:.1} MiB/s)  SHA-256 identical",
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
