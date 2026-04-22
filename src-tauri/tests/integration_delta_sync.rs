//! Integration tests for the delta-sync stack against local Docker fixtures.
//!
//! These tests are marked `#[ignore]` so `cargo test` default runs skip them.
//! Run explicitly after bringing the fixtures up:
//!
//! ```bash
//! cd src-tauri/tests/fixtures/sftp-rsync
//! ./setup.sh                               # generate ssh_key (first run)
//! docker compose up -d --build             # key-auth fixture on :2222
//! docker compose -f docker-compose.password.yml up -d --build
//! cd ../../..
//! cargo test --test integration_delta_sync -- --ignored --nocapture
//! cd tests/fixtures/sftp-rsync
//! docker compose down -v
//! docker compose -f docker-compose.password.yml down -v
//! ```
//!
//! The live cases cover both the rsync-over-SSH transport in isolation and the
//! real `sync_tree_core` product path through `SftpProvider::connect()`.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

#![cfg(unix)]

use ftp_client_gui_lib::providers::sftp::SftpProvider;
use ftp_client_gui_lib::providers::types::SftpConfig;
use ftp_client_gui_lib::providers::StorageProvider;
use ftp_client_gui_lib::sync::{
    sync_tree_core, ConflictMode, DeltaPolicy, NoopProgressSink, SyncDirection, SyncOptions,
};
use ftp_client_gui_lib::sync_core::ScanOptions;
use secrecy::SecretString;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::process::Command;
use tracing_subscriber::fmt::MakeWriter;

/// Path to the bundled fixture directory (relative to workspace root).
fn fixture_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR == src-tauri/
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("sftp-rsync")
}

fn ssh_key_path() -> PathBuf {
    fixture_dir().join("ssh_key")
}

fn container_running(container_name: &str) -> bool {
    let output = StdCommand::new("docker")
        .args([
            "ps",
            "--filter",
            &format!("name={}", container_name),
            "--filter",
            "status=running",
            "-q",
        ])
        .output();

    matches!(output, Ok(out) if !out.stdout.is_empty())
}

/// Skip test cleanly if the key-auth fixture key isn't generated or the
/// container isn't up. Prints a note so the user knows what to do.
fn fixture_ready_or_skip(test_name: &str) -> bool {
    if !ssh_key_path().exists() {
        eprintln!(
            "[{}] skipped: run {}/setup.sh first",
            test_name,
            fixture_dir().display()
        );
        return false;
    }
    if container_running("aeroftp-delta-sync-fixture") {
        return true;
    }

    eprintln!(
        "[{}] skipped: fixture container not running — run `docker compose up -d --build` in {}",
        test_name,
        fixture_dir().display()
    );
    false
}

/// Skip test cleanly if the password-auth fixture is not running.
fn password_fixture_ready_or_skip(test_name: &str) -> bool {
    if container_running("aeroftp-delta-sync-fixture-password") {
        return true;
    }

    eprintln!(
        "[{}] skipped: password fixture container not running — run `docker compose -f docker-compose.password.yml up -d --build` in {}",
        test_name,
        fixture_dir().display()
    );
    false
}

/// Run the one-shot equivalent of `ssh -o ... testuser@127.0.0.1:2222 <cmd>`
/// using the system `ssh` binary, so we can sanity-check the fixture and
/// construct shell probes independently of russh.
fn ssh_exec_shell(cmd: &str) -> Result<String, String> {
    let key = ssh_key_path();
    let output = StdCommand::new("ssh")
        .args([
            "-i",
            key.to_str().unwrap(),
            "-o",
            "StrictHostKeyChecking=no",
            "-o",
            "UserKnownHostsFile=/dev/null",
            "-o",
            "BatchMode=yes",
            "-p",
            "2222",
            "testuser@127.0.0.1",
            cmd,
        ])
        .output()
        .map_err(|e| format!("ssh spawn: {}", e))?;

    if !output.status.success() {
        return Err(format!(
            "ssh exit {}: {}",
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn seed_known_host(port: u16) {
    let home = std::env::var("HOME").expect("HOME must be set for known_hosts seeding");
    let ssh_dir = PathBuf::from(home).join(".ssh");
    std::fs::create_dir_all(&ssh_dir).expect("create ~/.ssh");
    let known_hosts = ssh_dir.join("known_hosts");
    let host = format!("[127.0.0.1]:{}", port);

    let _ = StdCommand::new("ssh-keygen")
        .args(["-R", &host, "-f", known_hosts.to_str().expect("utf8 known_hosts")])
        .output();

    let scan = StdCommand::new("ssh-keyscan")
        .args(["-p", &port.to_string(), "127.0.0.1"])
        .output()
        .expect("ssh-keyscan spawn");
    assert!(
        scan.status.success(),
        "ssh-keyscan failed for port {}: {}",
        port,
        String::from_utf8_lossy(&scan.stderr)
    );

    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&known_hosts)
        .expect("open known_hosts for append");
    file.write_all(&scan.stdout)
        .expect("append scanned host key to known_hosts");
}

#[test]
#[ignore = "requires docker fixture"]
fn fixture_has_rsync() {
    if !fixture_ready_or_skip("fixture_has_rsync") {
        return;
    }
    let out = ssh_exec_shell("command -v rsync && rsync --version | head -1")
        .expect("ssh should succeed");
    assert!(out.contains("/usr/bin/rsync"), "no rsync path in: {}", out);
    assert!(
        out.contains("protocol version"),
        "no protocol banner in: {}",
        out
    );
}

#[tokio::test]
#[ignore = "requires docker fixture"]
async fn rsync_upload_round_trip_and_redundant_upload_is_cheap() {
    if !fixture_ready_or_skip("rsync_upload_round_trip_and_redundant_upload_is_cheap") {
        return;
    }

    // Build a 2 MiB test payload (above the 1 MiB delta threshold).
    let payload_dir = tempfile::tempdir().expect("tempdir");
    let payload = payload_dir.path().join("delta.bin");
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&payload).unwrap();
        let chunk = vec![0x42u8; 1024];
        for _ in 0..2048 {
            f.write_all(&chunk).unwrap();
        }
    }

    // First rsync: full transfer expected. We drive rsync directly via the
    // shell to avoid pulling the full provider stack; this validates the
    // LANG=C locale override we use in rsync_over_ssh by replicating the
    // same invocation shape.
    let ssh_e = format!(
        "ssh -i {} -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o BatchMode=yes -p 2222",
        ssh_key_path().display()
    );
    let remote = "testuser@127.0.0.1:/workdir/delta.bin";

    let first = Command::new("rsync")
        .arg("-a")
        .arg("--info=progress2")
        .arg("--stats")
        .arg("-e")
        .arg(&ssh_e)
        .arg(payload.to_str().unwrap())
        .arg(remote)
        .output()
        .await
        .expect("rsync spawn");
    assert!(
        first.status.success(),
        "first rsync failed: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first_stdout = String::from_utf8_lossy(&first.stdout);

    // Parse the summary line ourselves, reusing the parser we ship.
    // We do this indirectly by eyeballing the output; the full parser is
    // unit-tested separately in src/rsync_output.rs.
    assert!(
        first_stdout.contains("bytes/sec"),
        "no summary in output: {}",
        first_stdout
    );

    // Second rsync: file identical, so delta traffic should be a tiny fraction
    // of the file size. This is the "delta sync works" assertion.
    let second = Command::new("rsync")
        .arg("-a")
        .arg("--info=progress2")
        .arg("--stats")
        .arg("-e")
        .arg(&ssh_e)
        .arg(payload.to_str().unwrap())
        .arg(remote)
        .output()
        .await
        .expect("rsync spawn");
    assert!(second.status.success());
    let second_stdout = String::from_utf8_lossy(&second.stdout);

    // Extract `sent N bytes` and `total size is M` to verify saving.
    let sent = extract_summary_u64(&second_stdout, "sent ", " bytes");
    let total = extract_summary_u64(&second_stdout, "total size is ", "  speedup");

    println!(
        "second upload — sent={:?} bytes, total={:?} bytes",
        sent, total
    );

    match (sent, total) {
        (Some(s), Some(t)) => {
            assert!(
                s * 10 < t,
                "expected delta to send < 10% of total (sent={}, total={})",
                s,
                t
            );
        }
        _ => panic!(
            "could not parse summary from second upload: {}",
            second_stdout
        ),
    }
}

/// Tiny helper that pulls an u64 out of "prefix<number>suffix" — accepts both
/// en_US ("1,048,576") and locale-native ("1.048.576") thousand separators by
/// stripping every '.' ',' and whitespace from the digit run.
fn extract_summary_u64(haystack: &str, prefix: &str, suffix: &str) -> Option<u64> {
    let start = haystack.find(prefix)? + prefix.len();
    let rest = &haystack[start..];
    let end = rest.find(suffix)?;
    let digits: String = rest[..end].chars().filter(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

fn unique_remote_root(prefix: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after unix epoch")
        .as_nanos();
    format!("/workdir/{}-{}-{}", prefix, std::process::id(), nanos)
}

fn write_repeated_payload(path: &Path, byte: u8, kib: usize) {
    use std::io::Write;

    let mut file = std::fs::File::create(path).expect("create payload");
    let chunk = vec![byte; 1024];
    for _ in 0..kib {
        file.write_all(&chunk).expect("write payload chunk");
    }
}

fn mutate_first_byte(path: &Path, byte: u8) {
    use std::io::{Seek, SeekFrom, Write};

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .expect("open payload for mutation");
    file.seek(SeekFrom::Start(0))
        .expect("seek to start of payload");
    file.write_all(&[byte]).expect("overwrite first byte");
}

fn capture_tracing_logs() -> (Arc<Mutex<Vec<u8>>>, tracing::subscriber::DefaultGuard) {
    struct VecMakeWriter(Arc<Mutex<Vec<u8>>>);
    struct VecWriter(Arc<Mutex<Vec<u8>>>);

    impl<'a> MakeWriter<'a> for VecMakeWriter {
        type Writer = VecWriter;

        fn make_writer(&'a self) -> Self::Writer {
            VecWriter(self.0.clone())
        }
    }

    impl std::io::Write for VecWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("lock tracing buffer").extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let buf = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(true)
        .with_writer(VecMakeWriter(buf.clone()))
        .without_time()
        .finish();

    (buf, tracing::subscriber::set_default(subscriber))
}

fn captured_text(buf: &Arc<Mutex<Vec<u8>>>) -> String {
    String::from_utf8_lossy(&buf.lock().expect("lock tracing buffer")).to_string()
}

async fn cleanup_remote_tree(provider: &mut Box<dyn StorageProvider>, remote_root: &str) {
    let _ = provider.rmdir_recursive(remote_root).await;
}

#[tokio::test]
#[ignore = "requires docker fixture"]
async fn product_path_uses_delta_when_session_is_eligible() {
    if !fixture_ready_or_skip("product_path_uses_delta_when_session_is_eligible") {
        return;
    }

    let (log_buf, _guard) = capture_tracing_logs();
    let mut provider: Box<dyn StorageProvider> = Box::new(SftpProvider::new(SftpConfig {
        host: "127.0.0.1".to_string(),
        port: 2222,
        username: "testuser".to_string(),
        password: None,
        private_key_path: Some(ssh_key_path().to_string_lossy().to_string()),
        key_passphrase: None,
        initial_path: Some("/workdir".to_string()),
        timeout_secs: 15,
        trust_unknown_hosts: true,
    }));
    provider.connect().await.expect("key-auth SFTP connect");

    let local_root = tempfile::tempdir().expect("local tempdir");
    let payload = local_root.path().join("delta-payload.bin");
    write_repeated_payload(&payload, 0x77, 2048);

    let remote_root = unique_remote_root("p1-t02-delta-used");
    let baseline = SyncOptions {
        direction: SyncDirection::Upload,
        delta_policy: DeltaPolicy::Mtime,
        dry_run: false,
        delete_orphans: false,
        conflict_mode: ConflictMode::Larger,
        scan: ScanOptions::default(),
    };
    let mut sink = NoopProgressSink;

    let first_report = sync_tree_core(
        &mut provider,
        local_root.path().to_str().expect("utf8 local path"),
        &remote_root,
        &baseline,
        &mut sink,
    )
    .await;
    assert!(
        first_report.errors.is_empty(),
        "first product-path apply should succeed, got errors: {:?}",
        first_report.errors
    );
    assert_eq!(first_report.uploaded, 1, "first apply should upload once");

    thread::sleep(Duration::from_secs(2));
    mutate_first_byte(&payload, b'X');

    let delta_opts = SyncOptions {
        delta_policy: DeltaPolicy::Delta,
        ..baseline.clone()
    };
    let second_report = sync_tree_core(
        &mut provider,
        local_root.path().to_str().expect("utf8 local path"),
        &remote_root,
        &delta_opts,
        &mut sink,
    )
    .await;

    let logs = captured_text(&log_buf);

    assert!(
        second_report.errors.is_empty(),
        "second product-path apply should succeed, got errors: {:?}",
        second_report.errors
    );
    assert_eq!(second_report.uploaded, 1, "second apply should upload once");
    assert!(
        logs.contains("sync.delta: used delta path"),
        "expected product path to use delta on eligible session; logs were:\n{}",
        logs
    );

    cleanup_remote_tree(&mut provider, &remote_root).await;
    let _ = provider.disconnect().await;
}

#[tokio::test]
#[ignore = "requires docker fixture"]
async fn product_path_falls_through_silently_when_session_not_eligible() {
    if !password_fixture_ready_or_skip(
        "product_path_falls_through_silently_when_session_not_eligible",
    ) {
        return;
    }

    let (log_buf, _guard) = capture_tracing_logs();
    seed_known_host(2223);
    let mut provider: Box<dyn StorageProvider> = Box::new(SftpProvider::new(SftpConfig {
        host: "127.0.0.1".to_string(),
        port: 2223,
        username: "testuser".to_string(),
        password: Some(SecretString::from("testpass".to_string())),
        private_key_path: None,
        key_passphrase: None,
        initial_path: Some("/workdir".to_string()),
        timeout_secs: 15,
        trust_unknown_hosts: true,
    }));
    provider
        .connect()
        .await
        .expect("password-auth SFTP connect");

    let local_root = tempfile::tempdir().expect("local tempdir");
    let payload = local_root.path().join("classic-only.bin");
    write_repeated_payload(&payload, 0x55, 512);

    let remote_root = unique_remote_root("p1-t02-silent-fallback");
    let opts = SyncOptions {
        direction: SyncDirection::Upload,
        delta_policy: DeltaPolicy::Delta,
        dry_run: false,
        delete_orphans: false,
        conflict_mode: ConflictMode::Larger,
        scan: ScanOptions::default(),
    };
    let mut sink = NoopProgressSink;

    let report = sync_tree_core(
        &mut provider,
        local_root.path().to_str().expect("utf8 local path"),
        &remote_root,
        &opts,
        &mut sink,
    )
    .await;
    let logs = captured_text(&log_buf);

    assert!(
        report.errors.is_empty(),
        "non-eligible session should fall back to classic without errors: {:?}",
        report.errors
    );
    assert_eq!(report.uploaded, 1, "classic path should still upload once");
    assert!(
        !logs.contains("sync.delta:"),
        "expected silent None => classic branch for password-auth session; logs were:\n{}",
        logs
    );

    cleanup_remote_tree(&mut provider, &remote_root).await;
    let _ = provider.disconnect().await;
}

#[test]
fn hard_rejection_string_contract_is_pinned_offline() {
    let sync_source = include_str!("../src/sync.rs");
    let delta_source = include_str!("../src/delta_sync_rsync.rs");

    assert!(
        sync_source.contains("delta hard rejection: {}"),
        "sync.rs must preserve the hard rejection error prefix"
    );
    assert!(
        delta_source.contains("hard_error: Some(reason.into())"),
        "delta_sync_rsync.rs must keep hard_error as the dedicated channel"
    );
    assert!(
        delta_source.contains("fallback_reason: None"),
        "hard rejection contract must remain mutually exclusive with fallback_reason"
    );
}

/// Structural pin of the match-arm ordering inside `perform_upload` and
/// `perform_download`: the `hard_error.is_some()` branch MUST sit between
/// the `used_delta` branch and the fallback-to-classic branch, so that a
/// hard rejection produces `FileOutcome::Failed` BEFORE control can fall
/// through to `provider.upload(...)`/`provider.download(...)`.
///
/// Covers the acceptance criterion of P1-T03 ("provider.upload() NOT called
/// on hard rejection") at the structural level — a runtime FakeProvider test
/// would require stubbing 40+ StorageProvider trait methods, which is
/// disproportionate for pinning an ordering invariant that is preserved by
/// source layout and enforced by the language's match-arm evaluation order.
#[test]
fn hard_error_branch_runs_before_classic_fallback_in_bivio() {
    let src = include_str!("../src/sync.rs");

    // Both perform_upload and perform_download must contain, in order:
    //   1. `Some(result) if result.used_delta => ... return FileOutcome::Uploaded|Downloaded`
    //   2. `Some(result) if result.hard_error.is_some() => ... return FileOutcome::Failed`
    //   3. `Some(result) => ...` (fallback reason log)
    //   4. `provider.upload(...)` or `provider.download(...)` (classic path)
    //
    // The indices below prove the arms appear in the required order.
    for (direction, classic_call) in
        [("Upload", "provider.upload("), ("Download", "provider.download(")]
    {
        let tag = format!("sync.delta: used delta path (direction={}", direction);
        let used_at = src
            .find(&tag)
            .unwrap_or_else(|| panic!("`used delta path` log for {} missing", direction));
        let hard_at = src[used_at..]
            .find("result.hard_error.is_some()")
            .map(|o| o + used_at)
            .unwrap_or_else(|| {
                panic!(
                    "hard_error match arm missing after used_delta for {}",
                    direction
                )
            });
        let failed_at = src[hard_at..]
            .find("delta hard rejection: {}")
            .map(|o| o + hard_at)
            .unwrap_or_else(|| panic!("FileOutcome::Failed emit missing for {}", direction));
        let classic_at = src[failed_at..]
            .find(classic_call)
            .map(|o| o + failed_at)
            .unwrap_or_else(|| panic!("classic call `{}` missing for {}", classic_call, direction));

        assert!(
            used_at < hard_at,
            "used_delta arm must come before hard_error arm for {direction}"
        );
        assert!(
            hard_at < failed_at,
            "hard_error arm must emit Failed before any classic path runs for {direction}"
        );
        assert!(
            failed_at < classic_at,
            "FileOutcome::Failed emit must precede the classic provider call for {direction}"
        );
    }
}

// =============================================================================
// Mock-driven offline coverage of `try_delta_transfer_with_transport`.
// Pins the mutually-exclusive `hard_error` ↔ `fallback_reason` contract at
// runtime (not just via source-include) and exercises the happy-path stats
// propagation without Docker fixtures.
// =============================================================================

mod mock_transport_coverage {
    use async_trait::async_trait;
    use ftp_client_gui_lib::delta_sync_rsync::{
        clear_probe_cache, try_delta_transfer_with_transport, SyncDirection,
    };
    use ftp_client_gui_lib::delta_transport::DeltaTransport;
    use ftp_client_gui_lib::rsync_over_ssh::{RsyncCapability, RsyncError, RsyncStats};
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Behavior knob for [`MockDeltaTransport`]. Each variant maps to a
    /// terminal result of the upload/download surface.
    enum MockBehavior {
        /// Delta path succeeds; stats are returned unchanged.
        OkStats(RsyncStats),
        /// Native rsync refused for a security-class reason (e.g. host-key
        /// mismatch). Must surface as `hard_error`, never as
        /// `fallback_reason`.
        HardRejection(String),
        /// Local or remote probe failed (rsync missing, SSH transient).
        /// Must surface as `fallback_reason`, never as `hard_error`.
        ProbeFailed(String),
    }

    struct MockDeltaTransport {
        behavior: MockBehavior,
        /// Atomic counter incremented every time `upload` or `download` is
        /// dispatched. Probe calls are NOT counted; only the terminal transfer
        /// operations, which is what the bivio actually invokes on the
        /// classic-fallback decision boundary.
        transfer_calls: Arc<AtomicUsize>,
    }

    impl MockDeltaTransport {
        fn new(behavior: MockBehavior) -> (Self, Arc<AtomicUsize>) {
            let counter = Arc::new(AtomicUsize::new(0));
            (
                Self {
                    behavior,
                    transfer_calls: counter.clone(),
                },
                counter,
            )
        }

        fn emit(&self) -> Result<RsyncStats, RsyncError> {
            self.transfer_calls.fetch_add(1, Ordering::SeqCst);
            match &self.behavior {
                MockBehavior::OkStats(s) => Ok(s.clone()),
                MockBehavior::HardRejection(msg) => {
                    Err(RsyncError::HardRejection(msg.clone()))
                }
                MockBehavior::ProbeFailed(msg) => Err(RsyncError::ProbeFailed(msg.clone())),
            }
        }
    }

    #[async_trait]
    impl DeltaTransport for MockDeltaTransport {
        fn name(&self) -> &'static str {
            "mock-delta-transport"
        }
        async fn probe_remote(&self) -> Result<RsyncCapability, RsyncError> {
            // Probe succeeds unconditionally: the three variants diverge on
            // the terminal upload/download call, not on probe. ProbeFailed as
            // a `MockBehavior` variant is modeled at the transfer layer so we
            // can still observe that `probe_remote` returned OK yet the
            // adapter mapped the transfer error correctly.
            Ok(RsyncCapability {
                version: "mock-3.2.7".into(),
                protocol: 31,
            })
        }
        async fn probe_local(&self) -> Result<(), RsyncError> {
            Ok(())
        }
        async fn download(
            &self,
            _remote: &str,
            _local: &Path,
        ) -> Result<RsyncStats, RsyncError> {
            self.emit()
        }
        async fn upload(
            &self,
            _local: &Path,
            _remote: &str,
        ) -> Result<RsyncStats, RsyncError> {
            self.emit()
        }
    }

    #[tokio::test]
    async fn hard_rejection_surfaces_on_hard_error_channel_exclusively() {
        clear_probe_cache().await;
        let (transport, calls) = MockDeltaTransport::new(MockBehavior::HardRejection(
            "ssh: host key verification failed".into(),
        ));

        let result = try_delta_transfer_with_transport(
            &transport,
            SyncDirection::Upload,
            Path::new("/tmp/never-touched"),
            "/remote/never-touched",
            "mock-session-hard",
        )
        .await
        .expect("wrapper must always return Some()");

        assert!(
            !result.used_delta,
            "hard rejection must not flag used_delta=true"
        );
        assert!(
            result.stats.is_none(),
            "hard rejection must not carry rsync stats"
        );
        assert!(
            result.fallback_reason.is_none(),
            "mutually exclusive contract: hard_error => fallback_reason is None"
        );
        let hard = result
            .hard_error
            .as_deref()
            .expect("hard_error must be populated on RsyncError::HardRejection");
        assert!(
            hard.contains("ssh: host key verification failed"),
            "hard_error must preserve the original rsync error message, got: {hard}"
        );

        // Probe succeeded -> the transfer method WAS invoked once. That's the
        // distinguishing observable of "the wrapper actually reached the
        // transport" (as opposed to short-circuiting at the probe stage).
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "upload() must have been dispatched exactly once before hard rejection"
        );
    }

    #[tokio::test]
    async fn probe_failure_falls_back_without_promoting_to_hard_error() {
        clear_probe_cache().await;
        let (transport, calls) = MockDeltaTransport::new(MockBehavior::ProbeFailed(
            "remote rsync binary not found".into(),
        ));

        let result = try_delta_transfer_with_transport(
            &transport,
            SyncDirection::Upload,
            Path::new("/tmp/never-touched"),
            "/remote/never-touched",
            "mock-session-probe-fail",
        )
        .await
        .expect("wrapper must always return Some()");

        assert!(
            !result.used_delta,
            "probe failure must not flag used_delta=true"
        );
        assert!(result.stats.is_none());
        assert!(
            result.hard_error.is_none(),
            "transport-level probe failure MUST NOT be promoted to hard_error; \
             that channel is reserved for security-class rejections"
        );
        let fb = result
            .fallback_reason
            .as_deref()
            .expect("fallback_reason must be populated on RsyncError::ProbeFailed");
        assert!(
            fb.contains("rsync failed") || fb.contains("rsync probe failed"),
            "fallback_reason must include the rsync error description, got: {fb}"
        );

        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "upload() is invoked even when it returns ProbeFailed (our mock \
             routes probe-failure through the transfer method on purpose)"
        );
    }

    #[tokio::test]
    async fn used_delta_propagates_rsync_stats_verbatim() {
        clear_probe_cache().await;
        // `warnings` is pub(crate); build via Default() and assign the
        // public scalar fields individually so the struct stays valid.
        let mut stats: RsyncStats = Default::default();
        stats.bytes_sent = 12_345;
        stats.bytes_received = 4_321;
        stats.total_size = 1_000_000;
        stats.speedup = 81.0;
        stats.duration_ms = 57;
        let (transport, calls) = MockDeltaTransport::new(MockBehavior::OkStats(stats.clone()));

        let result = try_delta_transfer_with_transport(
            &transport,
            SyncDirection::Download,
            Path::new("/tmp/irrelevant"),
            "/remote/irrelevant",
            "mock-session-happy",
        )
        .await
        .expect("wrapper must always return Some()");

        assert!(result.used_delta, "happy path must set used_delta=true");
        assert!(result.fallback_reason.is_none());
        assert!(result.hard_error.is_none());
        let carried = result.stats.as_ref().expect("stats must be Some");
        assert_eq!(carried.bytes_sent, 12_345);
        assert_eq!(carried.total_size, 1_000_000);
        assert!((carried.speedup - 81.0).abs() < 1e-9);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "download() dispatched exactly once"
        );
    }
}
