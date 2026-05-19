//! PD-PIPE-1 live validation: bounded read pipelining on the **one
//! existing** SFTP session (no new connection, no pool), against the repo's
//! local SFTP Docker fixture (no secrets, reproducible).
//!
//! `#[ignore]` so default `cargo test` skips it. It drives the real
//! `SftpProvider::download()` path: with `AEROFTP_SFTP_READ_PIPELINE` unset
//! the serial loop runs (the shipped, diff-0 default); with the flag set a
//! window of concurrent `SSH_FXP_READ` is multiplexed over the single SFTP
//! channel. The gate proves **byte-identical output** (serial == pipelined
//! == source SHA-256) across sizes that exercise the chunk/window/tail
//! boundaries (sub-chunk, exact chunk, multi-batch + partial tail, large),
//! plus F-1: zero `.aerotmp` residue on success and on the error path.
//!
//! Cancellation parity: the plain `download()` serial loop has no
//! cancellation token (it is drop-cancelled, and `AtomicFile`'s temp guard
//! cleans `.aerotmp` on drop). The pipelined path keeps exactly that
//! behaviour (no new token), so "diff-0 cancel" here means the same
//! drop-based semantics; the error-path residue check exercises the guard.
//!
//! ```bash
//! cd src-tauri/tests/fixtures/sftp-rsync
//! ./setup.sh && docker compose up -d --build      # key-auth on :2222
//! cd ../../.. && cargo test --release --test integration_sftp_pipeline \
//!   -- --ignored --nocapture
//! cd tests/fixtures/sftp-rsync && docker compose down -v
//! ```

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::Instant;

use ftp_client_gui_lib::providers::sftp::SftpProvider;
use ftp_client_gui_lib::providers::types::SftpConfig;
use ftp_client_gui_lib::providers::StorageProvider;
use sha2::{Digest, Sha256};

const PORT: u16 = 2222;
const CHUNK: usize = 256 * 1024; // SftpProvider default buffer_size

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sftp-rsync")
}

fn ssh_key_path() -> PathBuf {
    fixture_dir().join("ssh_key")
}

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

fn sha256_file(path: &Path) -> String {
    let mut h = Sha256::new();
    h.update(std::fs::read(path).expect("read file"));
    format!("{:x}", h.finalize())
}

fn deterministic_blob(seed: usize, len: usize) -> Vec<u8> {
    let mut buf = vec![0u8; len];
    for (j, b) in buf.iter_mut().enumerate() {
        *b = ((seed * 131 + j * 7) % 251) as u8;
    }
    buf
}

fn aerotmp_residue(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.to_string_lossy().contains(".aerotmp") {
                out.push(p);
            }
        }
    }
    out
}

#[tokio::test]
#[ignore = "live: requires the sftp-rsync Docker fixture up on :2222"]
async fn sftp_read_pipeline_is_byte_identical_to_serial() {
    if !ssh_key_path().exists() {
        eprintln!("SKIP: fixture ssh_key missing (run fixtures/sftp-rsync/setup.sh)");
        return;
    }
    seed_known_host();

    // Sizes chosen to exercise every boundary of the windowed reader:
    //  - sub-chunk (eff_window collapses to 1, single short read)
    //  - exact one chunk (no tail)
    //  - exact multiple of chunk (full batches, no partial tail)
    //  - multi-batch with a non-aligned partial tail chunk
    //  - large (many batches; the throughput case)
    let cases: &[(&str, usize)] = &[
        ("tiny", 7),
        ("one_chunk", CHUNK),
        ("aligned", CHUNK * 8),
        ("tail", CHUNK * 9 + 12_345),
        ("large", 64 * 1024 * 1024),
    ];

    let src_dir = std::env::temp_dir().join("pd-pipe1-src");
    let _ = std::fs::remove_dir_all(&src_dir);
    std::fs::create_dir_all(&src_dir).unwrap();

    let mut base = SftpProvider::new(fixture_config());
    base.connect().await.expect("base SFTP connect");

    // Seed remote dataset.
    let mut src_hash = std::collections::HashMap::new();
    for (i, (name, len)) in cases.iter().enumerate() {
        let p = src_dir.join(format!("{name}.bin"));
        std::fs::write(&p, deterministic_blob(i + 1, *len)).unwrap();
        src_hash.insert(*name, sha256_file(&p));
        base.upload(
            p.to_str().unwrap(),
            &format!("/workdir/pipe_{name}.bin"),
            None,
        )
        .await
        .unwrap_or_else(|e| panic!("seed upload {name} failed: {e}"));
    }

    let out_root = std::env::temp_dir().join("pd-pipe1-out");
    let _ = std::fs::remove_dir_all(&out_root);
    std::fs::create_dir_all(&out_root).unwrap();

    for (name, len) in cases {
        let remote = format!("/workdir/pipe_{name}.bin");
        let src = src_hash.get(name).cloned().unwrap();

        // Serial (flag unset = shipped default).
        std::env::remove_var("AEROFTP_SFTP_READ_PIPELINE");
        let serial_out = out_root.join(format!("{name}.serial"));
        let t0 = Instant::now();
        base.download(&remote, serial_out.to_str().unwrap(), None)
            .await
            .unwrap_or_else(|e| panic!("serial download {name} failed: {e}"));
        let serial_s = t0.elapsed().as_secs_f64();
        let serial_h = sha256_file(&serial_out);

        // Pipelined (window 8).
        std::env::set_var("AEROFTP_SFTP_READ_PIPELINE", "8");
        let pipe_out = out_root.join(format!("{name}.pipe"));
        let t1 = Instant::now();
        base.download(&remote, pipe_out.to_str().unwrap(), None)
            .await
            .unwrap_or_else(|e| panic!("pipelined download {name} failed: {e}"));
        let pipe_s = t1.elapsed().as_secs_f64();
        let pipe_h = sha256_file(&pipe_out);
        std::env::remove_var("AEROFTP_SFTP_READ_PIPELINE");

        assert_eq!(serial_h, src, "serial != source for {name}");
        assert_eq!(
            pipe_h, src,
            "pipelined != source for {name} (NOT byte-identical)"
        );
        assert!(
            aerotmp_residue(&out_root).is_empty(),
            "F-1: .aerotmp residue after {name}"
        );

        let mib = *len as f64 / 1_048_576.0;
        eprintln!(
            "{name:<10} {len:>9}B  serial {serial_s:>6.3}s ({:>6.1} MiB/s)  \
             pipeline {pipe_s:>6.3}s ({:>6.1} MiB/s)  DIFF0",
            if serial_s > 0.0 { mib / serial_s } else { 0.0 },
            if pipe_s > 0.0 { mib / pipe_s } else { 0.0 },
        );
    }

    // Error path with the flag on: a missing remote must Err and leave no
    // .aerotmp behind (AtomicFile temp guard, same as the serial path).
    std::env::set_var("AEROFTP_SFTP_READ_PIPELINE", "8");
    let err_out = out_root.join("missing.out");
    let err = base
        .download(
            "/workdir/pipe_does_not_exist.bin",
            err_out.to_str().unwrap(),
            None,
        )
        .await;
    std::env::remove_var("AEROFTP_SFTP_READ_PIPELINE");
    assert!(err.is_err(), "missing remote must error with the flag on");
    assert!(
        aerotmp_residue(&out_root).is_empty(),
        "F-1: .aerotmp residue after error path"
    );

    eprintln!("PD-PIPE-1: serial == pipelined == source for all sizes; zero .aerotmp residue");
}
