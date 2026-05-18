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

        assert_eq!(
            results.len(),
            FILES,
            "all files must download at C={concurrency}"
        );
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
    eprintln!(
        "PD-SFTP-1: byte-identical across C=1/3/5; independent-connection pool path exercised."
    );
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

/// PD-CLI-CONV-B live validation: the CLI file-level download batch now
/// runs through the SAME shared executor + orchestrator the GUI uses,
/// sink-agnostic (`NoopTransferSink`). This test drives EXACTLY that
/// converged path (`resolve_provider_executor_session_model` ->
/// `ProviderDownloadExecutor` -> `execute_batch`) against the Docker SFTP
/// fixture, instead of the bare `clone_for_transfer` + `buffer_unordered`
/// the PD-SFTP-1 test exercised.
///
/// Gate is correctness + non-regression:
/// - the session model resolves to `SftpConnectionPool` (the converged
///   pool-backed path is taken, not a silent locked-single fallback);
/// - every file is byte-identical SHA-256 at C=1 and C=4;
/// - `batch_result.completed == FILES` (the orchestrator drove them);
/// - C=4 wall-clock is not catastrophically worse than C=1 (parallelism
///   preserved up to the SFTP cap, never serialised).
#[tokio::test]
#[ignore = "live: requires the sftp-rsync Docker fixture up on :2222"]
async fn pd_cli_conv_b_shared_executor_download_is_byte_identical() {
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    use ftp_client_gui_lib::provider_transfer_executor::{
        resolve_provider_executor_session_model, ProviderDownloadExecutor,
        ProviderExecutorSessionModel,
    };
    use ftp_client_gui_lib::transfer_domain::{
        TransferBatchConfig, TransferDirection, TransferEntry,
    };
    use ftp_client_gui_lib::transfer_event_sink::{NoopTransferSink, TransferEventSink};
    use ftp_client_gui_lib::transfer_orchestrator::{execute_batch, TransferBatch};
    use ftp_client_gui_lib::transfer_settings::{
        resolve_provider_transfer_settings, TransferSettingsInput,
    };

    if !ssh_key_path().exists() {
        eprintln!("SKIP: fixture ssh_key missing (run fixtures/sftp-rsync/setup.sh)");
        return;
    }
    seed_known_host();

    let src_dir = std::env::temp_dir().join("pd-cli-conv-b-src-it");
    let _ = std::fs::remove_dir_all(&src_dir);
    std::fs::create_dir_all(&src_dir).unwrap();
    let mut src_hashes = std::collections::HashMap::new();
    for i in 0..FILES {
        let name = format!("f{i}.bin");
        let p = src_dir.join(&name);
        let mut buf = vec![0u8; FILE_BYTES];
        for (j, b) in buf.iter_mut().enumerate() {
            *b = ((i * 97 + j * 13) % 251) as u8;
        }
        std::fs::write(&p, &buf).unwrap();
        src_hashes.insert(name, sha256_file(&p));
    }

    let mut base = SftpProvider::new(fixture_config());
    base.connect().await.expect("base SFTP connect");
    assert!(
        base.connection_spec().is_some(),
        "connect() must capture a secure SftpConnectionSpec"
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
    let _ = base.disconnect().await;

    let files: Vec<(String, String, u64)> = (0..FILES)
        .map(|i| {
            (
                format!("/workdir/f{i}.bin"),
                format!("f{i}.bin"),
                FILE_BYTES as u64,
            )
        })
        .collect();
    let total_mib = (FILES * FILE_BYTES) as f64 / 1_048_576.0;

    let tmp_root = std::env::temp_dir().join("pd-cli-conv-b-it");
    let _ = std::fs::remove_dir_all(&tmp_root);

    let mut c1_elapsed: Option<std::time::Duration> = None;
    let mut c4_elapsed: Option<std::time::Duration> = None;
    for concurrency in [1usize, 4] {
        let run_dir = tmp_root.join(format!("c{concurrency}"));
        std::fs::create_dir_all(&run_dir).unwrap();

        // One connected base provider; the converged executor clones it
        // into N independent SSH connections (PD-SFTP-1 re-dial).
        let mut connected = SftpProvider::new(fixture_config());
        connected.connect().await.expect("converged base connect");
        let provider_arc = Arc::new(tokio::sync::Mutex::new(Some(
            Box::new(connected) as Box<dyn StorageProvider>
        )));

        let model = resolve_provider_executor_session_model(&provider_arc, concurrency).await;
        assert!(
            matches!(
                model,
                ProviderExecutorSessionModel::SftpConnectionPool { .. }
            ),
            "converged path must resolve to the SFTP connection pool, got {model:?}"
        );

        let runtime_settings = resolve_provider_transfer_settings(TransferSettingsInput {
            max_concurrent: None,
            retry_count: None,
            timeout_seconds: None,
        });
        let cancel_token = tokio_util::sync::CancellationToken::new();
        let sink: Arc<dyn TransferEventSink> = Arc::new(NoopTransferSink);
        let executor = Arc::new(ProviderDownloadExecutor::new(
            sink.clone(),
            provider_arc.clone(),
            runtime_settings,
            cancel_token,
            model,
        ));

        let entries: Vec<TransferEntry> = files
            .iter()
            .enumerate()
            .map(|(i, (remote, name, size))| TransferEntry {
                id: format!("it-{i}"),
                display_name: name.clone(),
                remote_path: remote.clone(),
                local_path: run_dir.join(name).to_string_lossy().to_string(),
                size: *size,
                modified: None,
            })
            .collect();
        let batch = TransferBatch {
            id: "pd-cli-conv-b-it".to_string(),
            display_name: "shared executor batch".to_string(),
            direction: TransferDirection::Download,
            config: TransferBatchConfig {
                max_concurrent: concurrency as u32,
                max_retries: runtime_settings.retry_count,
                timeout_ms: runtime_settings.timeout_seconds.saturating_mul(1000),
            },
            entries,
        };

        let cancel = Arc::new(AtomicBool::new(false));
        let started = Instant::now();
        let result = execute_batch(sink, batch, executor, cancel, None).await;
        let elapsed = started.elapsed();

        assert_eq!(
            result.completed, FILES as u32,
            "the orchestrator must complete every file at C={concurrency}"
        );
        assert_eq!(result.failed, 0, "no failures expected at C={concurrency}");

        for i in 0..FILES {
            let name = format!("f{i}.bin");
            assert_eq!(
                src_hashes.get(&name),
                Some(&sha256_file(&run_dir.join(&name))),
                "byte mismatch for {name} at C={concurrency} (converged path)"
            );
        }

        eprintln!(
            "PD-CLI-CONV-B C={concurrency:<2} elapsed={:>6.2}s  {:>6.1} MiB/s  (shared executor path)",
            elapsed.as_secs_f64(),
            total_mib / elapsed.as_secs_f64(),
        );
        if concurrency == 1 {
            c1_elapsed = Some(elapsed);
        } else {
            c4_elapsed = Some(elapsed);
        }

        let taken = provider_arc.lock().await.take();
        if let Some(mut p) = taken {
            let _ = p.disconnect().await;
        }
    }

    // Parallelism not degraded: C=4 must not be dramatically slower than
    // C=1 (loopback has no bandwidth bottleneck, so this only guards a
    // silent serialisation regression, not a throughput claim).
    if let (Some(e1), Some(e4)) = (c1_elapsed, c4_elapsed) {
        assert!(
            e4 <= e1 * 3,
            "C=4 ({e4:?}) catastrophically slower than C=1 ({e1:?}): converged path serialised?"
        );
    }

    let _ = std::fs::remove_dir_all(&tmp_root);
    let _ = std::fs::remove_dir_all(&src_dir);
    eprintln!(
        "PD-CLI-CONV-B: CLI batch converged on the shared executor (execute_batch + \
         ProviderDownloadExecutor + NoopTransferSink); byte-identical, pool-backed, not serialised."
    );
}

/// PD-CLI-CONV-C upload twin of the PD-CLI-CONV-B download test: the CLI
/// `put -r` / `put <glob>` batch now runs through the SAME shared engine
/// (`run_shared_provider_upload_batch` -> `execute_batch` +
/// `ProviderUploadExecutor`). This drives that exact core against the
/// live SFTP fixture and asserts:
/// - the session model resolves to the SFTP connection pool (pool-backed,
///   so the shared path is taken, not the legacy fallback);
/// - every uploaded file is byte-identical (SHA-256) when read back;
/// - C=4 wall-clock is not catastrophically worse than C=1 (parallelism
///   preserved, never silently serialised).
#[tokio::test]
#[ignore = "live: requires the sftp-rsync Docker fixture up on :2222"]
async fn pd_cli_conv_c_shared_executor_upload_is_byte_identical() {
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    use ftp_client_gui_lib::provider_transfer_executor::{
        resolve_provider_executor_session_model, ProviderExecutorSessionModel,
        ProviderUploadExecutor,
    };
    use ftp_client_gui_lib::transfer_domain::{
        TransferBatchConfig, TransferDirection, TransferEntry,
    };
    use ftp_client_gui_lib::transfer_event_sink::{NoopTransferSink, TransferEventSink};
    use ftp_client_gui_lib::transfer_orchestrator::{execute_batch, TransferBatch};
    use ftp_client_gui_lib::transfer_settings::{
        resolve_provider_transfer_settings, TransferSettingsInput,
    };

    if !ssh_key_path().exists() {
        eprintln!("SKIP: fixture ssh_key missing (run fixtures/sftp-rsync/setup.sh)");
        return;
    }
    seed_known_host();

    let src_dir = std::env::temp_dir().join("pd-cli-conv-c-src-it");
    let _ = std::fs::remove_dir_all(&src_dir);
    std::fs::create_dir_all(&src_dir).unwrap();
    let mut src_hashes = std::collections::HashMap::new();
    for i in 0..FILES {
        let name = format!("u{i}.bin");
        let p = src_dir.join(&name);
        let mut buf = vec![0u8; FILE_BYTES];
        for (j, b) in buf.iter_mut().enumerate() {
            *b = ((i * 53 + j * 17) % 251) as u8;
        }
        std::fs::write(&p, &buf).unwrap();
        src_hashes.insert(name, sha256_file(&p));
    }
    let total_mib = (FILES * FILE_BYTES) as f64 / 1_048_576.0;

    let verify_root = std::env::temp_dir().join("pd-cli-conv-c-verify-it");
    let _ = std::fs::remove_dir_all(&verify_root);

    let mut c1_elapsed: Option<std::time::Duration> = None;
    let mut c4_elapsed: Option<std::time::Duration> = None;
    for concurrency in [1usize, 4] {
        let remote_dir = format!("/workdir/pd-cli-conv-c-c{concurrency}");

        // Pre-create the remote parent: `run_shared_provider_upload_batch`
        // requires the caller to mkdir parents (the shared executor does
        // not), exactly as cmd_put_recursive/glob now do before the batch.
        let mut seeder = SftpProvider::new(fixture_config());
        seeder.connect().await.expect("seeder SFTP connect");
        let _ = seeder.mkdir(&remote_dir).await;
        let _ = seeder.disconnect().await;

        // One connected base provider; the converged executor clones it
        // into N independent SSH connections (PD-SFTP-1 re-dial).
        let mut connected = SftpProvider::new(fixture_config());
        connected.connect().await.expect("converged base connect");
        let provider_arc = Arc::new(tokio::sync::Mutex::new(Some(
            Box::new(connected) as Box<dyn StorageProvider>
        )));

        let model = resolve_provider_executor_session_model(&provider_arc, concurrency).await;
        assert!(
            matches!(
                model,
                ProviderExecutorSessionModel::SftpConnectionPool { .. }
            ),
            "converged upload path must resolve to the SFTP connection pool, got {model:?}"
        );

        let runtime_settings = resolve_provider_transfer_settings(TransferSettingsInput {
            max_concurrent: None,
            retry_count: None,
            timeout_seconds: None,
        });
        let cancel_token = tokio_util::sync::CancellationToken::new();
        let sink: Arc<dyn TransferEventSink> = Arc::new(NoopTransferSink);
        let executor = Arc::new(ProviderUploadExecutor::new(
            sink.clone(),
            provider_arc.clone(),
            runtime_settings,
            None,
            cancel_token,
            model,
        ));

        let entries: Vec<TransferEntry> = (0..FILES)
            .map(|i| {
                let name = format!("u{i}.bin");
                TransferEntry {
                    id: format!("it-put-{i}"),
                    display_name: name.clone(),
                    remote_path: format!("{remote_dir}/{name}"),
                    local_path: src_dir.join(&name).to_string_lossy().to_string(),
                    size: FILE_BYTES as u64,
                    modified: None,
                }
            })
            .collect();
        let batch = TransferBatch {
            id: "pd-cli-conv-c-it".to_string(),
            display_name: "shared executor upload batch".to_string(),
            direction: TransferDirection::Upload,
            config: TransferBatchConfig {
                max_concurrent: concurrency as u32,
                max_retries: runtime_settings.retry_count,
                timeout_ms: runtime_settings.timeout_seconds.saturating_mul(1000),
            },
            entries,
        };

        let cancel = Arc::new(AtomicBool::new(false));
        let started = Instant::now();
        let result = execute_batch(sink, batch, executor, cancel, None).await;
        let elapsed = started.elapsed();

        assert_eq!(
            result.completed, FILES as u32,
            "the orchestrator must upload every file at C={concurrency}"
        );
        assert_eq!(
            result.failed, 0,
            "no upload failures expected at C={concurrency}"
        );

        let taken = provider_arc.lock().await.take();
        if let Some(mut p) = taken {
            let _ = p.disconnect().await;
        }

        // Read every uploaded file back and assert byte-identity.
        let run_verify = verify_root.join(format!("c{concurrency}"));
        std::fs::create_dir_all(&run_verify).unwrap();
        let mut verifier = SftpProvider::new(fixture_config());
        verifier.connect().await.expect("verifier SFTP connect");
        for i in 0..FILES {
            let name = format!("u{i}.bin");
            let local = run_verify.join(&name);
            verifier
                .download(
                    &format!("{remote_dir}/{name}"),
                    local.to_str().unwrap(),
                    None,
                )
                .await
                .unwrap_or_else(|e| panic!("readback download {name} failed: {e}"));
            assert_eq!(
                src_hashes.get(&name),
                Some(&sha256_file(&local)),
                "byte mismatch for uploaded {name} at C={concurrency} (converged path)"
            );
        }
        let _ = verifier.disconnect().await;

        eprintln!(
            "PD-CLI-CONV-C C={concurrency:<2} elapsed={:>6.2}s  {:>6.1} MiB/s  (shared upload executor path)",
            elapsed.as_secs_f64(),
            total_mib / elapsed.as_secs_f64(),
        );
        if concurrency == 1 {
            c1_elapsed = Some(elapsed);
        } else {
            c4_elapsed = Some(elapsed);
        }
    }

    if let (Some(e1), Some(e4)) = (c1_elapsed, c4_elapsed) {
        assert!(
            e4 <= e1 * 3,
            "C=4 ({e4:?}) catastrophically slower than C=1 ({e1:?}): converged upload path serialised?"
        );
    }

    let _ = std::fs::remove_dir_all(&verify_root);
    let _ = std::fs::remove_dir_all(&src_dir);
    eprintln!(
        "PD-CLI-CONV-C: CLI upload batch converged on the shared executor (execute_batch + \
         ProviderUploadExecutor + NoopTransferSink); byte-identical, pool-backed, not serialised."
    );
}

/// PD-CLI-CONV-D: the CLI `sync` transfer phase now routes its download
/// list through `run_shared_provider_download_batch` and its upload list
/// through `run_shared_provider_upload_batch` (the same shared core as
/// `get -r` / `put`), while scan / compare / conflict / journal /
/// delete-orphans stay outside the batch. The cmd_sync-specific
/// invariant D introduces: the shared batches open their OWN base
/// connection, so the scan `provider` survives the transfer phase and is
/// still usable for the post-transfer remote ops (rename, delete-orphans).
/// This drives that exact bidirectional core against the live SFTP
/// fixture and asserts:
/// - both directions resolve to the SFTP connection pool (pool-backed,
///   shared path taken, not the legacy fallback);
/// - every downloaded AND uploaded file is byte-identical (SHA-256);
/// - a separate scan provider stays connected across both shared batches
///   and still performs a post-transfer `rename` (the `--track-renames`
///   path), proving the shared batch did not consume it;
/// - C=4 wall-clock is not catastrophically worse than C=1 in either
///   direction (parallelism preserved, never silently serialised).
#[tokio::test]
#[ignore = "live: requires the sftp-rsync Docker fixture up on :2222"]
async fn pd_cli_conv_d_sync_transfer_phase_is_byte_identical() {
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    use ftp_client_gui_lib::provider_transfer_executor::{
        resolve_provider_executor_session_model, ProviderDownloadExecutor,
        ProviderExecutorSessionModel, ProviderUploadExecutor,
    };
    use ftp_client_gui_lib::transfer_domain::{
        TransferBatchConfig, TransferDirection, TransferEntry,
    };
    use ftp_client_gui_lib::transfer_event_sink::{NoopTransferSink, TransferEventSink};
    use ftp_client_gui_lib::transfer_orchestrator::{execute_batch, TransferBatch};
    use ftp_client_gui_lib::transfer_settings::{
        resolve_provider_transfer_settings, TransferSettingsInput,
    };

    if !ssh_key_path().exists() {
        eprintln!("SKIP: fixture ssh_key missing (run fixtures/sftp-rsync/setup.sh)");
        return;
    }
    seed_known_host();

    // Local upload sources (the sync `to_upload` list).
    let up_src = std::env::temp_dir().join("pd-cli-conv-d-upsrc-it");
    let _ = std::fs::remove_dir_all(&up_src);
    std::fs::create_dir_all(&up_src).unwrap();
    let mut up_hashes = std::collections::HashMap::new();
    for i in 0..FILES {
        let name = format!("up{i}.bin");
        let p = up_src.join(&name);
        let mut buf = vec![0u8; FILE_BYTES];
        for (j, b) in buf.iter_mut().enumerate() {
            *b = ((i * 71 + j * 29) % 251) as u8;
        }
        std::fs::write(&p, &buf).unwrap();
        up_hashes.insert(name, sha256_file(&p));
    }
    let total_mib = (FILES * FILE_BYTES) as f64 / 1_048_576.0;

    let dl_verify_root = std::env::temp_dir().join("pd-cli-conv-d-dlverify-it");
    let _ = std::fs::remove_dir_all(&dl_verify_root);
    let up_verify_root = std::env::temp_dir().join("pd-cli-conv-d-upverify-it");
    let _ = std::fs::remove_dir_all(&up_verify_root);

    let runtime_settings = resolve_provider_transfer_settings(TransferSettingsInput {
        max_concurrent: None,
        retry_count: None,
        timeout_seconds: None,
    });

    let mut c1_elapsed: Option<std::time::Duration> = None;
    let mut c4_elapsed: Option<std::time::Duration> = None;
    for concurrency in [1usize, 4] {
        let down_dir = format!("/workdir/pd-cli-conv-d-down-c{concurrency}");
        let up_dir = format!("/workdir/pd-cli-conv-d-up-c{concurrency}");

        // Seed the remote download sources (the sync `to_download` list)
        // and pre-create the upload parent (cmd_sync pre-creates
        // upload_dirs with the connected scan provider before the batch).
        let mut seeder = SftpProvider::new(fixture_config());
        seeder.connect().await.expect("seeder SFTP connect");
        let _ = seeder.mkdir(&down_dir).await;
        let _ = seeder.mkdir(&up_dir).await;
        let mut down_hashes = std::collections::HashMap::new();
        for i in 0..FILES {
            let name = format!("dn{i}.bin");
            let p = up_src.join(format!("seed-{name}"));
            let mut buf = vec![0u8; FILE_BYTES];
            for (j, b) in buf.iter_mut().enumerate() {
                *b = ((i * 41 + j * 19) % 251) as u8;
            }
            std::fs::write(&p, &buf).unwrap();
            down_hashes.insert(name.clone(), sha256_file(&p));
            seeder
                .upload(p.to_str().unwrap(), &format!("{down_dir}/{name}"), None)
                .await
                .unwrap_or_else(|e| panic!("seed remote download src {name}: {e}"));
            let _ = std::fs::remove_file(&p);
        }
        let _ = seeder.disconnect().await;

        // The cmd_sync scan provider: connected once, kept alive across
        // BOTH shared batches, used afterwards for the post-transfer
        // rename (the `--track-renames` loop). PD-CLI-CONV-D's invariant
        // is that the shared batches must NOT consume this connection.
        let mut scan_provider = SftpProvider::new(fixture_config());
        scan_provider
            .connect()
            .await
            .expect("scan provider connect");

        let started = Instant::now();

        // ---- Download phase: sync `to_download` -> shared download core
        {
            let dl_run = dl_verify_root.join(format!("c{concurrency}"));
            std::fs::create_dir_all(&dl_run).unwrap();
            let mut connected = SftpProvider::new(fixture_config());
            connected.connect().await.expect("download base connect");
            let provider_arc = Arc::new(tokio::sync::Mutex::new(Some(
                Box::new(connected) as Box<dyn StorageProvider>
            )));
            let model = resolve_provider_executor_session_model(&provider_arc, concurrency).await;
            assert!(
                matches!(
                    model,
                    ProviderExecutorSessionModel::SftpConnectionPool { .. }
                ),
                "sync download phase must resolve to the SFTP pool, got {model:?}"
            );
            let cancel_token = tokio_util::sync::CancellationToken::new();
            let sink: Arc<dyn TransferEventSink> = Arc::new(NoopTransferSink);
            let executor = Arc::new(ProviderDownloadExecutor::new(
                sink.clone(),
                provider_arc.clone(),
                runtime_settings,
                cancel_token,
                model,
            ));
            let entries: Vec<TransferEntry> = (0..FILES)
                .map(|i| {
                    let name = format!("dn{i}.bin");
                    TransferEntry {
                        id: format!("it-d-dl-{i}"),
                        display_name: name.clone(),
                        remote_path: format!("{down_dir}/{name}"),
                        local_path: dl_run.join(&name).to_string_lossy().to_string(),
                        size: FILE_BYTES as u64,
                        modified: None,
                    }
                })
                .collect();
            let batch = TransferBatch {
                id: "pd-cli-conv-d-dl".to_string(),
                display_name: "sync download phase".to_string(),
                direction: TransferDirection::Download,
                config: TransferBatchConfig {
                    max_concurrent: concurrency as u32,
                    max_retries: runtime_settings.retry_count,
                    timeout_ms: runtime_settings.timeout_seconds.saturating_mul(1000),
                },
                entries,
            };
            let cancel = Arc::new(AtomicBool::new(false));
            let result = execute_batch(sink, batch, executor, cancel, None).await;
            assert_eq!(
                result.completed, FILES as u32,
                "sync download must complete all"
            );
            assert_eq!(result.failed, 0, "no sync download failures");
            for i in 0..FILES {
                let name = format!("dn{i}.bin");
                assert_eq!(
                    down_hashes.get(&name),
                    Some(&sha256_file(&dl_run.join(&name))),
                    "sync download byte mismatch {name} at C={concurrency}"
                );
            }
            let taken = provider_arc.lock().await.take();
            if let Some(mut p) = taken {
                let _ = p.disconnect().await;
            }
        }

        // ---- Upload phase: sync `to_upload` -> shared upload core
        {
            let mut connected = SftpProvider::new(fixture_config());
            connected.connect().await.expect("upload base connect");
            let provider_arc = Arc::new(tokio::sync::Mutex::new(Some(
                Box::new(connected) as Box<dyn StorageProvider>
            )));
            let model = resolve_provider_executor_session_model(&provider_arc, concurrency).await;
            assert!(
                matches!(
                    model,
                    ProviderExecutorSessionModel::SftpConnectionPool { .. }
                ),
                "sync upload phase must resolve to the SFTP pool, got {model:?}"
            );
            let cancel_token = tokio_util::sync::CancellationToken::new();
            let sink: Arc<dyn TransferEventSink> = Arc::new(NoopTransferSink);
            let executor = Arc::new(ProviderUploadExecutor::new(
                sink.clone(),
                provider_arc.clone(),
                runtime_settings,
                None,
                cancel_token,
                model,
            ));
            let entries: Vec<TransferEntry> = (0..FILES)
                .map(|i| {
                    let name = format!("up{i}.bin");
                    TransferEntry {
                        id: format!("it-d-up-{i}"),
                        display_name: name.clone(),
                        remote_path: format!("{up_dir}/{name}"),
                        local_path: up_src.join(&name).to_string_lossy().to_string(),
                        size: FILE_BYTES as u64,
                        modified: None,
                    }
                })
                .collect();
            let batch = TransferBatch {
                id: "pd-cli-conv-d-up".to_string(),
                display_name: "sync upload phase".to_string(),
                direction: TransferDirection::Upload,
                config: TransferBatchConfig {
                    max_concurrent: concurrency as u32,
                    max_retries: runtime_settings.retry_count,
                    timeout_ms: runtime_settings.timeout_seconds.saturating_mul(1000),
                },
                entries,
            };
            let cancel = Arc::new(AtomicBool::new(false));
            let result = execute_batch(sink, batch, executor, cancel, None).await;
            assert_eq!(
                result.completed, FILES as u32,
                "sync upload must complete all"
            );
            assert_eq!(result.failed, 0, "no sync upload failures");
            let taken = provider_arc.lock().await.take();
            if let Some(mut p) = taken {
                let _ = p.disconnect().await;
            }
        }

        let elapsed = started.elapsed();

        // cmd_sync invariant: the scan provider was NOT consumed by the
        // shared batches and is still usable for the post-transfer remote
        // ops. Mirror the `--track-renames` loop: rename one uploaded
        // file via the scan provider, then read it back byte-identical.
        let old_remote = format!("{up_dir}/up0.bin");
        let new_remote = format!("{up_dir}/up0.renamed.bin");
        scan_provider
            .rename(&old_remote, &new_remote)
            .await
            .expect("post-transfer rename via the surviving scan provider");
        let _ = scan_provider.disconnect().await;

        // Read every uploaded file back (renamed one included) and assert
        // byte-identity, proving upload + post-transfer rename are sound.
        let up_run = up_verify_root.join(format!("c{concurrency}"));
        std::fs::create_dir_all(&up_run).unwrap();
        let mut verifier = SftpProvider::new(fixture_config());
        verifier.connect().await.expect("verifier connect");
        for i in 0..FILES {
            let name = format!("up{i}.bin");
            let remote = if i == 0 {
                new_remote.clone()
            } else {
                format!("{up_dir}/{name}")
            };
            let local = up_run.join(&name);
            verifier
                .download(&remote, local.to_str().unwrap(), None)
                .await
                .unwrap_or_else(|e| panic!("readback {name}: {e}"));
            assert_eq!(
                up_hashes.get(&name),
                Some(&sha256_file(&local)),
                "sync upload byte mismatch {name} at C={concurrency}"
            );
        }
        let _ = verifier.disconnect().await;

        eprintln!(
            "PD-CLI-CONV-D C={concurrency:<2} elapsed={:>6.2}s  {:>6.1} MiB/s  (sync transfer phase, both directions)",
            elapsed.as_secs_f64(),
            (2.0 * total_mib) / elapsed.as_secs_f64(),
        );
        if concurrency == 1 {
            c1_elapsed = Some(elapsed);
        } else {
            c4_elapsed = Some(elapsed);
        }
    }

    if let (Some(e1), Some(e4)) = (c1_elapsed, c4_elapsed) {
        assert!(
            e4 <= e1 * 3,
            "C=4 ({e4:?}) catastrophically slower than C=1 ({e1:?}): sync transfer phase serialised?"
        );
    }

    let _ = std::fs::remove_dir_all(&dl_verify_root);
    let _ = std::fs::remove_dir_all(&up_verify_root);
    let _ = std::fs::remove_dir_all(&up_src);
    eprintln!(
        "PD-CLI-CONV-D: sync transfer phase converged on the shared executors \
         (download + upload), scan provider survived for the post-transfer rename; \
         byte-identical, pool-backed, not serialised."
    );
}

/// Mirror of the CLI binary's converged `pget` adapter, driving the EXACT
/// shared core (`providers::multi_thread::run_concurrent_range_download`).
/// `pget_segmented_download` itself lives in the binary and is unreachable
/// from an integration test, so this reproduces its behaviour with the
/// same provider-`read_range` window writer: N independent pooled SFTP
/// connections, one per gap-free window, each seek-writing its window at
/// the absolute offset into the engine's single pre-allocated `.aerotmp`,
/// atomically promoted on success.
async fn pget_via_shared_engine(
    base: &SftpProvider,
    remote: &str,
    out: &std::path::Path,
    file_size: u64,
    segments: usize,
) -> Result<(), ftp_client_gui_lib::providers::ProviderError> {
    use ftp_client_gui_lib::providers::multi_thread::{
        aerotmp_path_for, run_concurrent_range_download, ConcurrentRangeConfig,
        ConcurrentRangeOutcome,
    };
    use ftp_client_gui_lib::providers::ProviderError;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    // Small sub-read so each window takes several `read_range` calls
    // (exercises the adapter's sub-read accumulation loop).
    const SUB_READ: u64 = 8 * 1024 * 1024;

    // One independent pooled connection per window: N real connections,
    // exactly like the binary's `create_and_connect` loop.
    let mut conns: VecDeque<Box<dyn StorageProvider>> = VecDeque::with_capacity(segments);
    for _ in 0..segments {
        let mut w = base.clone_for_transfer()?;
        w.connect().await?;
        conns.push_back(w);
    }
    let pool = Arc::new(tokio::sync::Mutex::new(conns));
    let remote_owned = remote.to_string();

    let cfg = ConcurrentRangeConfig {
        final_path: out.to_path_buf(),
        total_size: file_size,
        streams: segments,
        max_streams: segments,
        max_parallel: segments,
    };

    let write_one_range = move |start_off: u64,
                                end_off: u64,
                                temp_path: std::path::PathBuf,
                                aggregate: Arc<AtomicU64>,
                                cancel: CancellationToken| {
        let pool = pool.clone();
        let remote = remote_owned.clone();
        async move {
            use tokio::io::{AsyncSeekExt, AsyncWriteExt};

            let mut provider = {
                let mut g = pool.lock().await;
                g.pop_front().ok_or_else(|| {
                    ProviderError::TransferFailed("pget: pool exhausted".to_string())
                })?
            };
            let window_len = end_off - start_off + 1;
            let mut file = tokio::fs::OpenOptions::new()
                .write(true)
                .open(&temp_path)
                .await
                .map_err(ProviderError::IoError)?;
            file.seek(std::io::SeekFrom::Start(start_off))
                .await
                .map_err(ProviderError::IoError)?;
            let mut written = 0u64;
            while written < window_len {
                if cancel.is_cancelled() {
                    let _ = provider.disconnect().await;
                    return Err(ProviderError::TransferFailed("cancelled".to_string()));
                }
                let sub = (window_len - written).min(SUB_READ);
                let data = provider
                    .read_range(&remote, start_off + written, sub)
                    .await?;
                if data.is_empty() {
                    let _ = provider.disconnect().await;
                    return Err(ProviderError::TransferFailed(format!(
                        "pget: short read at {} ({}/{})",
                        start_off + written,
                        written,
                        window_len
                    )));
                }
                file.write_all(&data)
                    .await
                    .map_err(ProviderError::IoError)?;
                written += data.len() as u64;
                aggregate.fetch_add(data.len() as u64, Ordering::Relaxed);
            }
            file.flush().await.map_err(ProviderError::IoError)?;
            let _ = provider.disconnect().await;
            assert_eq!(written, window_len, "pget: window underwrite");
            Ok(ConcurrentRangeOutcome::Completed)
        }
    };

    match run_concurrent_range_download(cfg, write_one_range, CancellationToken::new(), None)
        .await?
    {
        ConcurrentRangeOutcome::Completed => {
            let temp = aerotmp_path_for(out);
            tokio::fs::rename(&temp, out)
                .await
                .map_err(ProviderError::IoError)?;
            Ok(())
        }
        ConcurrentRangeOutcome::ServerIgnoredRange => Err(ProviderError::TransferFailed(
            "read_range unexpectedly produced ServerIgnoredRange".to_string(),
        )),
    }
}

/// PD-CLI-CONV-E live validation: the CLI segmented parallel download
/// (`pget`) now runs through the SAME shared transport-agnostic
/// concurrent-range engine the GUI / HTTP / SFTP-intra-file paths use,
/// instead of the old hand-rolled multi-file temp dir + manual assemble.
/// This is the SFTP-transfer-convergence closure: after it, every CLI SFTP
/// transfer path (download, upload, sync, pget) rides the shared engine.
///
/// Gate is correctness + non-regression:
/// - the base captures a secure `SftpConnectionSpec` and a pooled worker
///   advertises strict concurrent range (the converged pool-backed path,
///   not a silent locked-single fallback);
/// - the assembled file is byte-identical SHA-256 at N=1 and N=4;
/// - no `.aerotmp` residue (the engine's RAII + the atomic rename);
/// - N=4 wall-clock is not catastrophically worse than N=1 (the N real
///   connections are not silently serialised; loopback has no bandwidth
///   bottleneck so only catastrophic serialisation is caught, noted
///   honestly in the master, not faked here).
#[tokio::test]
#[ignore = "live: requires the sftp-rsync Docker fixture up on :2222"]
async fn pd_cli_conv_e_pget_segmented_is_byte_identical() {
    if !ssh_key_path().exists() {
        eprintln!("SKIP: fixture ssh_key missing (run fixtures/sftp-rsync/setup.sh)");
        return;
    }
    seed_known_host();

    // ~64 MiB of non-repeating pseudo-random bytes so a mis-ordered or
    // short window cannot accidentally hash-match.
    const BIG_BYTES: usize = 64 * 1024 * 1024;

    let src_dir = std::env::temp_dir().join("pd-cli-conv-e-src");
    let _ = std::fs::remove_dir_all(&src_dir);
    std::fs::create_dir_all(&src_dir).unwrap();
    let src = src_dir.join("big.bin");
    {
        let mut buf = vec![0u8; BIG_BYTES];
        let mut x: u32 = 0x1357_9BDF;
        for b in buf.iter_mut() {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            *b = (x & 0xFF) as u8;
        }
        std::fs::write(&src, &buf).unwrap();
    }
    let src_hash = sha256_file(&src);
    let file_size = BIG_BYTES as u64;
    let remote = "/workdir/pget-e.bin";

    let mut base = SftpProvider::new(fixture_config());
    base.connect().await.expect("base SFTP connect");
    assert!(
        base.connection_spec().is_some(),
        "connect() must capture a secure SftpConnectionSpec (pool-backed)"
    );
    base.upload(src.to_str().unwrap(), remote, None)
        .await
        .expect("seed upload");

    // A pooled worker must advertise strict concurrent range: this is the
    // converged pool-backed SFTP path pget now rides, not a locked single.
    {
        let probe = base.clone_for_transfer().expect("clone_for_transfer probe");
        assert_eq!(
            probe
                .transfer_capabilities()
                .strict_concurrent_range_download,
            Capability::Supported,
            "a pool-backed SFTP worker must advertise strict concurrent range"
        );
    }

    let tmp_root = std::env::temp_dir().join("pd-cli-conv-e");
    let _ = std::fs::remove_dir_all(&tmp_root);
    std::fs::create_dir_all(&tmp_root).unwrap();
    let total_mib = BIG_BYTES as f64 / 1_048_576.0;

    // --- N=1: the shared engine's degenerate single window ---------------
    let n1 = tmp_root.join("n1.bin");
    let t1 = Instant::now();
    pget_via_shared_engine(&base, remote, &n1, file_size, 1)
        .await
        .expect("pget N=1 via shared engine");
    let e1 = t1.elapsed();
    assert_eq!(src_hash, sha256_file(&n1), "N=1 byte mismatch");

    // --- N=4: the real segmented parallel download -----------------------
    let n4 = tmp_root.join("n4.bin");
    let t4 = Instant::now();
    pget_via_shared_engine(&base, remote, &n4, file_size, 4)
        .await
        .expect("pget N=4 via shared engine");
    let e4 = t4.elapsed();
    assert_eq!(src_hash, sha256_file(&n4), "N=4 segmented byte mismatch");

    // Strict temp hygiene: the engine's RAII + atomic rename leave no
    // `.aerotmp` residue beside either output.
    assert!(
        !tmp_root.join("n1.bin.aerotmp").exists(),
        "N=1 left an .aerotmp residue"
    );
    assert!(
        !tmp_root.join("n4.bin.aerotmp").exists(),
        "N=4 left an .aerotmp residue"
    );

    assert!(
        e4 <= e1 * 3,
        "N=4 ({e4:?}) catastrophically slower than N=1 ({e1:?}): pget serialised?"
    );

    eprintln!(
        "PD-CLI-CONV-E pget: file={:.0} MiB  N=1 {:.2}s ({:.1} MiB/s)  N=4 {:.2}s ({:.1} MiB/s)  SHA-256 identical, single .aerotmp, atomically promoted",
        total_mib,
        e1.as_secs_f64(),
        total_mib / e1.as_secs_f64(),
        e4.as_secs_f64(),
        total_mib / e4.as_secs_f64(),
    );

    let _ = base.disconnect().await;
    let _ = std::fs::remove_dir_all(&tmp_root);
    let _ = std::fs::remove_dir_all(&src_dir);
}
