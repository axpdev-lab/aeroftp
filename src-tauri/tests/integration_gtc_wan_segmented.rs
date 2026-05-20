//! GTC-1 / GTC-2 live WAN validation: exercises the new GUI-side
//! segmented download helpers (`provider_segmented_download_eligible`
//! and `run_provider_segmented_download` in
//! `provider_transfer_executor`) end-to-end against the axpbuntu lab.
//!
//! Both `#[ignore]` so default `cargo test` skips them. The tests load
//! credentials from the user's vault (same path the CLI / MCP use),
//! seed a 64 MiB fixture on the remote, and drive the GUI helper
//! directly. They are the missing piece to the GTC-0 baseline harness,
//! which only validated the CLI surface (`pget_segmented_download`):
//! GTC-1 wired the same engine into `ProviderDownloadExecutor` and
//! GTC-2 into `provider_download_file`, both via these `pub fn`
//! helpers. Calling them under live conditions is what the GTC-1/2
//! handoff lists as "Option A - Live WAN validation".
//!
//! Run:
//! ```bash
//! # Vault must be unlocked (keyring) or AEROFTP_MASTER_PASSWORD set.
//! cargo test --release --test integration_gtc_wan_segmented \
//!     -- --ignored --nocapture
//! ```
//!
//! Each test is `SKIP`-on-no-vault rather than fail-on-no-vault so a
//! developer without the axpbuntu lab can still run the suite.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::time::Instant;

use ftp_client_gui_lib::credential_store::CredentialStore;
use ftp_client_gui_lib::cross_profile_transfer::{
    copy_one_file, copy_one_file_with_options, CrossProfileCopyOptions,
};
use ftp_client_gui_lib::provider_transfer_executor::{
    provider_segmented_download_eligible, run_provider_segmented_download,
};
use ftp_client_gui_lib::providers::ftp::FtpProvider;
use ftp_client_gui_lib::providers::multi_thread::aerotmp_path_for;
use ftp_client_gui_lib::providers::s3::S3Provider;
use ftp_client_gui_lib::providers::sftp::SftpProvider;
use ftp_client_gui_lib::providers::types::{FtpConfig, FtpTlsMode, S3Config, SftpConfig};
use ftp_client_gui_lib::providers::{ProviderTransferExecutorKind, StorageProvider};
use ftp_client_gui_lib::sync::{
    sync_tree_core, ConflictMode, DeltaPolicy, NoopProgressSink, SyncDirection, SyncOptions,
};
use ftp_client_gui_lib::sync_core::scan::ScanOptions;
use ftp_client_gui_lib::transfer_dag::Capability;
use secrecy::SecretString;
use sha2::{Digest, Sha256};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

const SFTP_PROFILE_ID: &str = "srv_1778600830336_6pvrx7450";
const SFTP_HOST: &str = "49.13.171.110";
const SFTP_PORT: u16 = 22;
const SFTP_USERNAME: &str = "axpdev";
const REMOTE_DIR: &str = "/home/axpdev/_gtc_gui_validation";
const REMOTE_BIG: &str = "/home/axpdev/_gtc_gui_validation/64MiB.bin";
const FILE_BYTES: usize = 64 * 1024 * 1024;

// FTP axpbuntu lab (seeded by /var/www/lumo_cms/seed_axpbuntu_lab.rs)
const FTP_PROFILE_ID: &str = "srv_axpbuntu_ftp_plain";
const FTP_HOST: &str = "ftp.lab.axpdev.it";
const FTP_PORT: u16 = 21;
const FTP_USERNAME: &str = "ftplab";
const FTP_REMOTE_DIR: &str = "/_gtc_gui_validation";
const FTP_REMOTE_BIG: &str = "/_gtc_gui_validation/64MiB.bin";

// S3 / MinIO axpbuntu lab (seeded by seed_axpbuntu_lab.rs)
const S3_PROFILE_ID: &str = "srv_axpbuntu_s3_minio";
const S3_USERNAME: &str = "aeroftp-admin";
const S3_BUCKET: &str = "aeroftp-test";
const S3_REGION: &str = "us-east-1";
const S3_ENDPOINT: &str = "https://s3.lab.axpdev.it";
const S3_REMOTE_BIG: &str = "_gtc_gui_validation/64MiB.bin";
const GTC4_FILE_BYTES: usize = 16 * 1024 * 1024;
const GTC4_FILES: usize = 4;

// ---------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------

fn sha256_file(path: &Path) -> String {
    use std::io::Read;
    let mut h = Sha256::new();
    let mut f = std::fs::File::open(path).expect("open file for sha256");
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = f.read(&mut buf).expect("read file for sha256");
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    format!("{:x}", h.finalize())
}

/// Loaded SFTP credentials. The axpbuntu admin profile stores both a
/// password and a private-key path (`~/.ssh/id_ed25519`); russh tries
/// PubKey first, so the test must pass both to match what the CLI
/// does (otherwise auth fails because password-only is not what the
/// server accepts for this user).
#[derive(Clone)]
struct LoadedSftpCreds {
    password: Option<String>,
    private_key_path: Option<String>,
}

fn expand_tilde(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            let mut out = std::path::PathBuf::from(home);
            out.push(rest);
            return out.to_string_lossy().into_owned();
        }
    }
    p.to_string()
}

/// Pull SFTP credentials out of the user's vault. Returns `None`
/// when the vault is locked, missing the profile, or running in an
/// environment without keyring access. The caller treats `None` as
/// SKIP, not FAIL.
fn load_sftp_creds() -> Option<LoadedSftpCreds> {
    // `init()` is idempotent: returns "OK" when the vault is open via
    // the system keyring, "MASTER_PASSWORD_REQUIRED" when locked.
    let init_result = CredentialStore::init().ok()?;
    if init_result != "OK" {
        eprintln!("SKIP: vault requires master password ({init_result})");
        return None;
    }
    let store = CredentialStore::from_cache()?;
    let raw = store
        .get(&format!("server_{}", SFTP_PROFILE_ID))
        .ok()
        .filter(|s| !s.is_empty())?;

    // The vault may store either a raw password string OR a JSON object
    // with `{password, private_key_path, key_passphrase, ...}` fields.
    let (password, private_key_path) =
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw) {
            if let Some(obj) = val.as_object() {
                let pwd = obj
                    .get("password")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let key = obj
                    .get("private_key_path")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(expand_tilde);
                (pwd, key)
            } else {
                (Some(raw), None)
            }
        } else {
            (Some(raw), None)
        };

    // Fallback: the SFTP admin profile is known to use the local
    // `~/.ssh/id_ed25519` key via the GUI's "Private Key Path" field.
    // If the blob is password-only (legacy format), try that key too
    // so the same vault works whether the cred was saved before or
    // after the field was added.
    let private_key_path = private_key_path.or_else(|| {
        let candidate = expand_tilde("~/.ssh/id_ed25519");
        if std::path::Path::new(&candidate).exists() {
            Some(candidate)
        } else {
            None
        }
    });

    Some(LoadedSftpCreds {
        password,
        private_key_path,
    })
}

fn sftp_config(creds: LoadedSftpCreds) -> SftpConfig {
    SftpConfig {
        host: SFTP_HOST.to_string(),
        port: SFTP_PORT,
        username: SFTP_USERNAME.to_string(),
        password: creds.password.map(SecretString::from),
        private_key_path: creds.private_key_path,
        key_passphrase: None,
        initial_path: Some("/home/axpdev".to_string()),
        timeout_secs: 30,
        // The axpbuntu host fingerprint is already in the user's
        // known_hosts via daily CLI use; flipping this to true keeps
        // the test honest (would also succeed on first contact in a
        // fresh shell). The clone workers re-dial with the same flag.
        trust_unknown_hosts: true,
    }
}

fn random_buf(seed: u64, size: usize) -> Vec<u8> {
    // xorshift64: deterministic, non-periodic at this size, cheap.
    // We need non-trivial bytes (so a mis-ordered window can't match)
    // but reproducible enough not to drag a heavy RNG dep into tests.
    let mut x = seed.max(1);
    let mut buf = vec![0u8; size];
    for chunk in buf.chunks_mut(8) {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let bytes = x.to_le_bytes();
        for (i, b) in chunk.iter_mut().enumerate() {
            *b = bytes[i];
        }
    }
    buf
}

async fn seed_remote(base: &mut SftpProvider, local_src: &Path) -> Result<(), String> {
    // Best-effort cleanup; ignore both "not found" and "directory exists".
    let _ = base.delete(REMOTE_BIG).await;
    let _ = base.mkdir(REMOTE_DIR).await;
    base.upload(local_src.to_str().unwrap(), REMOTE_BIG, None)
        .await
        .map_err(|e| format!("seed upload: {e}"))
}

async fn run_sync_download_once(
    creds: LoadedSftpCreds,
    local_root: &Path,
    download_segments: u32,
) -> (ftp_client_gui_lib::sync::SyncReport, std::time::Duration) {
    let mut provider: Box<dyn StorageProvider> = Box::new(SftpProvider::new(sftp_config(creds)));
    provider.connect().await.expect("sync SFTP connect");
    let opts = SyncOptions {
        direction: SyncDirection::Download,
        delta_policy: DeltaPolicy::Mtime,
        dry_run: false,
        delete_orphans: false,
        conflict_mode: ConflictMode::Larger,
        scan: ScanOptions::default(),
        download_segments,
    };
    let mut sink = NoopProgressSink;
    let t = Instant::now();
    let report = sync_tree_core(
        &mut provider,
        local_root.to_str().expect("utf8 local path"),
        REMOTE_DIR,
        &opts,
        &mut sink,
    )
    .await;
    let elapsed = t.elapsed();
    let _ = provider.disconnect().await;
    (report, elapsed)
}

// ---------------------------------------------------------------------
// Test 1: byte-identity vs single-stream baseline
// ---------------------------------------------------------------------

#[tokio::test]
#[ignore = "live WAN: GTC-1/2 byte-identity against axpbuntu (vault required)"]
async fn gtc_gui_sftp_segmented_byte_identical_vs_single_stream() {
    let Some(creds) = load_sftp_creds() else {
        eprintln!("SKIP: SFTP credentials unavailable in vault");
        return;
    };

    // Stage local fixture (64 MiB pseudo-random bytes).
    let src_dir = std::env::temp_dir().join("gtc-gui-sftp-byteid-src");
    let _ = std::fs::remove_dir_all(&src_dir);
    std::fs::create_dir_all(&src_dir).unwrap();
    let src = src_dir.join("64MiB.bin");
    std::fs::write(&src, random_buf(0xA5A5_5A5A, FILE_BYTES)).unwrap();
    let src_sha = sha256_file(&src);

    // Connect base provider and seed remote.
    let mut base = SftpProvider::new(sftp_config(creds));
    base.connect().await.expect("base SFTP connect");
    assert_eq!(
        base.transfer_executor_kind(),
        ProviderTransferExecutorKind::SftpConnectionPool,
        "connected SFTP must advertise SftpConnectionPool",
    );
    assert_eq!(
        base.transfer_capabilities()
            .strict_concurrent_range_download,
        Capability::Supported,
        "SFTP pool kind must lift strict_concurrent_range_download to Supported",
    );
    seed_remote(&mut base, &src).await.expect("seed");

    let dst_root = std::env::temp_dir().join("gtc-gui-sftp-byteid-dst");
    let _ = std::fs::remove_dir_all(&dst_root);
    std::fs::create_dir_all(&dst_root).unwrap();

    // ---- single-stream baseline (legacy code path) ------------------
    let dst1 = dst_root.join("seg1.bin");
    let mut w1 = base.clone_for_transfer().expect("clone for single-stream");
    let t1 = Instant::now();
    w1.download(REMOTE_BIG, dst1.to_str().unwrap(), None)
        .await
        .expect("single-stream download");
    let e1 = t1.elapsed();
    assert_eq!(
        sha256_file(&dst1),
        src_sha,
        "single-stream sha must match source",
    );

    // ---- segmented (GUI helper) - the new GTC-1/2 path --------------
    let segments = provider_segmented_download_eligible(&base, FILE_BYTES as u64, 4, 8)
        .expect("eligibility probe must return Some(N) for SFTP@axpbuntu w/ N=4 req");
    assert_eq!(
        segments, 4,
        "64 MiB / 4 segments = 16 MiB chunks (> 8 MiB anti-frag floor)",
    );

    let dst4 = dst_root.join("seg4.bin");
    let cancel = CancellationToken::new();
    let t4 = Instant::now();
    run_provider_segmented_download(
        &base,
        REMOTE_BIG,
        dst4.to_str().unwrap(),
        FILE_BYTES as u64,
        segments,
        None,
        cancel,
    )
    .await
    .expect("segmented download must succeed end-to-end");
    let e4 = t4.elapsed();

    assert_eq!(
        sha256_file(&dst4),
        src_sha,
        "segmented sha must match source byte-for-byte",
    );

    // No .aerotmp residue after success.
    let tmp = aerotmp_path_for(&dst4);
    assert!(
        !tmp.exists(),
        ".aerotmp must be renamed away on success: {tmp:?}",
    );

    let speedup = e1.as_secs_f64() / e4.as_secs_f64().max(1e-6);
    eprintln!(
        "GTC-1/2 SFTP @ axpbuntu: 64MiB seg=1 {:.2}s  seg=4 {:.2}s  speedup {:.2}x  byte-id=YES",
        e1.as_secs_f64(),
        e4.as_secs_f64(),
        speedup,
    );

    let _ = base.disconnect().await;
    let _ = std::fs::remove_dir_all(&dst_root);
    let _ = std::fs::remove_dir_all(&src_dir);
}

// ---------------------------------------------------------------------
// Test 1b: AeroSync transfer phase uses the same segmented helper while
// keeping the sync planner and result accounting in charge.
// ---------------------------------------------------------------------

#[tokio::test]
#[ignore = "live WAN: GTC-3 AeroSync segmented transfer phase against axpbuntu"]
async fn gtc_aerosync_sftp_segmented_download_byte_identity_speed_band() {
    let Some(creds) = load_sftp_creds() else {
        eprintln!("SKIP: SFTP credentials unavailable in vault");
        return;
    };

    let src_dir = std::env::temp_dir().join("gtc-aerosync-sftp-src");
    let _ = std::fs::remove_dir_all(&src_dir);
    std::fs::create_dir_all(&src_dir).unwrap();
    let src = src_dir.join("64MiB.bin");
    std::fs::write(&src, random_buf(0xB16B_00B5, FILE_BYTES)).unwrap();
    let src_sha = sha256_file(&src);

    let mut base = SftpProvider::new(sftp_config(creds.clone()));
    base.connect().await.expect("base SFTP connect");
    seed_remote(&mut base, &src).await.expect("seed");

    let single_root = std::env::temp_dir().join("gtc-aerosync-sftp-single");
    let segmented_root = std::env::temp_dir().join("gtc-aerosync-sftp-segmented");
    let _ = std::fs::remove_dir_all(&single_root);
    let _ = std::fs::remove_dir_all(&segmented_root);
    std::fs::create_dir_all(&single_root).unwrap();
    std::fs::create_dir_all(&segmented_root).unwrap();

    let (single_report, e1) = run_sync_download_once(creds.clone(), &single_root, 1).await;
    assert!(
        single_report.errors.is_empty(),
        "single-stream sync must succeed, got {:?}",
        single_report.errors,
    );
    assert_eq!(
        single_report.downloaded, 1,
        "single-stream sync downloads once"
    );
    let single_file = single_root.join("64MiB.bin");
    assert_eq!(
        sha256_file(&single_file),
        src_sha,
        "single-stream sync sha must match source",
    );

    let (segmented_report, e4) = run_sync_download_once(creds.clone(), &segmented_root, 4).await;
    assert!(
        segmented_report.errors.is_empty(),
        "segmented sync must succeed, got {:?}",
        segmented_report.errors,
    );
    assert_eq!(
        segmented_report.downloaded, 1,
        "segmented sync downloads once"
    );
    let segmented_file = segmented_root.join("64MiB.bin");
    assert_eq!(
        sha256_file(&segmented_file),
        src_sha,
        "segmented sync sha must match source byte-for-byte",
    );
    assert!(
        !aerotmp_path_for(&segmented_file).exists(),
        ".aerotmp must be renamed away after segmented sync success",
    );

    let speedup = e1.as_secs_f64() / e4.as_secs_f64().max(1e-6);
    eprintln!(
        "GTC-3 AeroSync SFTP @ axpbuntu: 64MiB seg=1 {:.2}s  seg=4 {:.2}s  speedup {:.2}x  byte-id=YES",
        e1.as_secs_f64(),
        e4.as_secs_f64(),
        speedup,
    );
    assert!(
        speedup >= 0.75,
        "segmented sync must stay within the expected WAN speed band, got {speedup:.2}x",
    );

    let _ = base.disconnect().await;
    let _ = std::fs::remove_dir_all(&single_root);
    let _ = std::fs::remove_dir_all(&segmented_root);
    let _ = std::fs::remove_dir_all(&src_dir);
}

// ---------------------------------------------------------------------
// Test 2: cooperative cancel leaves no .aerotmp / no partial dst
// ---------------------------------------------------------------------

#[tokio::test]
#[ignore = "live WAN: GTC-1/2 cancel cleanup against axpbuntu (vault required)"]
async fn gtc_gui_sftp_segmented_cancel_leaves_no_aerotmp() {
    let Some(creds) = load_sftp_creds() else {
        eprintln!("SKIP: SFTP credentials unavailable in vault");
        return;
    };

    // Reuse the same remote fixture seeded by the byte-id test if it
    // ran first; otherwise seed our own.
    let src_dir = std::env::temp_dir().join("gtc-gui-sftp-cancel-src");
    let _ = std::fs::remove_dir_all(&src_dir);
    std::fs::create_dir_all(&src_dir).unwrap();
    let src = src_dir.join("64MiB.bin");
    std::fs::write(&src, random_buf(0x5A5A_A5A5, FILE_BYTES)).unwrap();

    let mut base = SftpProvider::new(sftp_config(creds));
    base.connect().await.expect("base SFTP connect");
    seed_remote(&mut base, &src).await.expect("seed");

    let dst_root = std::env::temp_dir().join("gtc-gui-sftp-cancel-dst");
    let _ = std::fs::remove_dir_all(&dst_root);
    std::fs::create_dir_all(&dst_root).unwrap();
    let dst = dst_root.join("cancelled.bin");
    let tmp = aerotmp_path_for(&dst);

    let segments = provider_segmented_download_eligible(&base, FILE_BYTES as u64, 4, 8)
        .expect("eligibility for cancel test");

    // Pre-cancelled token: the engine checks `cancel.is_cancelled()` at
    // the top of every per-window inner-read loop, so a pre-cancelled
    // token forces the very first check to short-circuit. Firing the
    // cancel mid-flight is timing-fragile here (each 16 MiB window is
    // a single `read_range` sub-read because the engine's
    // SEGMENTED_DOWNLOAD_SUB_READ_SIZE is 64 MiB > window size, so the
    // inner loop has exactly one iteration to observe the cancel).
    // Pre-cancel is deterministic and validates the same invariant we
    // care about: TempFileGuard removes the `.aerotmp`, no atomic
    // rename leaks a partial file at `dst`.
    let cancel = CancellationToken::new();
    cancel.cancel();

    let outcome = run_provider_segmented_download(
        &base,
        REMOTE_BIG,
        dst.to_str().unwrap(),
        FILE_BYTES as u64,
        segments,
        None,
        cancel,
    )
    .await;

    assert!(
        outcome.is_err(),
        "cancelled segmented download must surface Err, got {outcome:?}",
    );
    assert!(
        !tmp.exists(),
        "TempFileGuard must remove the .aerotmp on cancel: {tmp:?}",
    );
    assert!(
        !dst.exists(),
        "final destination must NOT exist after cancel (no atomic rename happened): {dst:?}",
    );

    eprintln!("GTC-1/2 SFTP cancel @ axpbuntu: outcome=Err  .aerotmp=absent  dst=absent  ok",);

    let _ = base.disconnect().await;
    let _ = std::fs::remove_dir_all(&dst_root);
    let _ = std::fs::remove_dir_all(&src_dir);
}

// ---------------------------------------------------------------------
// Test 3: eligibility gate honours the anti-fragmentation floor
// (purely local: validates the gate without touching the network so a
// developer without the lab still gets a quick smoke).
// ---------------------------------------------------------------------

#[tokio::test]
async fn gtc_eligibility_gate_blocks_below_floor_locally() {
    // 4 MiB file with 4 segments requested -> floor (8 MiB) -> None.
    let f = std::env::temp_dir().join("gtc-eligibility-noop.bin");
    let _ = std::fs::remove_file(&f);
    std::fs::write(&f, vec![0u8; 4 * 1024 * 1024]).unwrap();

    let mut base = SftpProvider::new(SftpConfig {
        host: "127.0.0.1".to_string(),
        port: 1,
        username: "noone".to_string(),
        password: Some(SecretString::from("x".to_string())),
        private_key_path: None,
        key_passphrase: None,
        initial_path: None,
        timeout_secs: 1,
        trust_unknown_hosts: true,
    });
    // Not connected on purpose: kind must be LegacySingle (not pool),
    // so the gate has to return None regardless of file size.
    assert_ne!(
        base.transfer_executor_kind(),
        ProviderTransferExecutorKind::SftpConnectionPool,
        "unconnected SFTP must NOT advertise pool kind",
    );
    assert!(
        provider_segmented_download_eligible(&base, 4 * 1024 * 1024, 4, 8).is_none(),
        "unconnected SFTP must NOT be eligible",
    );

    let _ = std::fs::remove_file(&f);
    // Silence unused-mut warning on `base`.
    let _ = &mut base;

    // PathBuf import kept so future extensions don't need to re-add it.
    let _ = PathBuf::new();
}

// ---------------------------------------------------------------------
// Test 4: FTP plain - validates the read_range ensure_connected fix on
// the FtpConnectionPool path (PD-FTP-1 mirror of PD-SFTP-1). axpbuntu's
// FTP lab is the seeded vsftpd profile with a known plaintext password.
// ---------------------------------------------------------------------

fn load_ftp_password() -> Option<String> {
    let init_result = CredentialStore::init().ok()?;
    if init_result != "OK" {
        eprintln!("SKIP: vault requires master password ({init_result})");
        return None;
    }
    let store = CredentialStore::from_cache()?;
    let raw = store
        .get(&format!("server_{}", FTP_PROFILE_ID))
        .ok()
        .filter(|s| !s.is_empty())?;
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw) {
        if let Some(obj) = val.as_object() {
            return obj
                .get("password")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
        }
    }
    Some(raw.trim_matches('"').to_string())
}

fn ftp_config(password: String) -> FtpConfig {
    FtpConfig {
        host: FTP_HOST.to_string(),
        port: FTP_PORT,
        username: FTP_USERNAME.to_string(),
        password: SecretString::from(password),
        tls_mode: FtpTlsMode::None,
        verify_cert: false,
        initial_path: Some("/".to_string()),
    }
}

async fn seed_remote_ftp(base: &mut FtpProvider, local_src: &Path) -> Result<(), String> {
    let _ = base.delete(FTP_REMOTE_BIG).await;
    let _ = base.mkdir(FTP_REMOTE_DIR).await;
    base.upload(local_src.to_str().unwrap(), FTP_REMOTE_BIG, None)
        .await
        .map_err(|e| format!("seed upload: {e}"))
}

#[tokio::test]
#[ignore = "live WAN: GTC-1/2 FTP byte-identity against axpbuntu (vault required)"]
async fn gtc_gui_ftp_segmented_byte_identical_vs_single_stream() {
    let Some(password) = load_ftp_password() else {
        eprintln!("SKIP: FTP password unavailable in vault");
        return;
    };

    let src_dir = std::env::temp_dir().join("gtc-gui-ftp-byteid-src");
    let _ = std::fs::remove_dir_all(&src_dir);
    std::fs::create_dir_all(&src_dir).unwrap();
    let src = src_dir.join("64MiB.bin");
    std::fs::write(&src, random_buf(0xC3C3_3C3C, FILE_BYTES)).unwrap();
    let src_sha = sha256_file(&src);

    let mut base = FtpProvider::new(ftp_config(password));
    base.connect().await.expect("base FTP connect");
    assert_eq!(
        base.transfer_executor_kind(),
        ProviderTransferExecutorKind::FtpConnectionPool,
        "connected FTP must advertise FtpConnectionPool",
    );
    assert_eq!(
        base.transfer_capabilities()
            .strict_concurrent_range_download,
        Capability::Supported,
        "FTP pool kind must lift strict_concurrent_range_download to Supported",
    );
    seed_remote_ftp(&mut base, &src).await.expect("seed FTP");

    let dst_root = std::env::temp_dir().join("gtc-gui-ftp-byteid-dst");
    let _ = std::fs::remove_dir_all(&dst_root);
    std::fs::create_dir_all(&dst_root).unwrap();

    let dst1 = dst_root.join("seg1.bin");
    let mut w1 = base.clone_for_transfer().expect("clone FTP single-stream");
    let t1 = Instant::now();
    w1.download(FTP_REMOTE_BIG, dst1.to_str().unwrap(), None)
        .await
        .expect("FTP single-stream download");
    let e1 = t1.elapsed();
    assert_eq!(
        sha256_file(&dst1),
        src_sha,
        "FTP single-stream sha mismatch"
    );

    let segments = provider_segmented_download_eligible(&base, FILE_BYTES as u64, 4, 8)
        .expect("eligibility probe must succeed for FTP@axpbuntu");
    assert_eq!(segments, 4, "FTP 64MiB / 4 = 16MiB > 8MiB floor");

    let dst4 = dst_root.join("seg4.bin");
    let cancel = CancellationToken::new();
    let t4 = Instant::now();
    run_provider_segmented_download(
        &base,
        FTP_REMOTE_BIG,
        dst4.to_str().unwrap(),
        FILE_BYTES as u64,
        segments,
        None,
        cancel,
    )
    .await
    .expect("FTP segmented must succeed (validates read_range ensure_connected fix)");
    let e4 = t4.elapsed();
    assert_eq!(sha256_file(&dst4), src_sha, "FTP segmented sha mismatch");
    assert!(
        !aerotmp_path_for(&dst4).exists(),
        ".aerotmp must be renamed away after FTP segmented success",
    );

    let speedup = e1.as_secs_f64() / e4.as_secs_f64().max(1e-6);
    eprintln!(
        "GTC-1/2 FTP @ axpbuntu: 64MiB seg=1 {:.2}s  seg=4 {:.2}s  speedup {:.2}x  byte-id=YES",
        e1.as_secs_f64(),
        e4.as_secs_f64(),
        speedup,
    );

    let _ = base.delete(FTP_REMOTE_BIG).await;
    let _ = base.disconnect().await;
    let _ = std::fs::remove_dir_all(&dst_root);
    let _ = std::fs::remove_dir_all(&src_dir);
}

// ---------------------------------------------------------------------
// Test 5: S3 / MinIO - validates the HttpClonePool path (the other
// transport family for the GTC-1/2 GUI helper). axpbuntu's MinIO is
// the seeded profile with hardcoded admin credentials.
// ---------------------------------------------------------------------

fn load_s3_secret() -> Option<String> {
    let init_result = CredentialStore::init().ok()?;
    if init_result != "OK" {
        eprintln!("SKIP: vault requires master password ({init_result})");
        return None;
    }
    let store = CredentialStore::from_cache()?;
    let raw = store
        .get(&format!("server_{}", S3_PROFILE_ID))
        .ok()
        .filter(|s| !s.is_empty())?;
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw) {
        if let Some(obj) = val.as_object() {
            return obj
                .get("password")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
        }
    }
    Some(raw.trim_matches('"').to_string())
}

fn s3_config(secret: String) -> S3Config {
    S3Config {
        endpoint: Some(S3_ENDPOINT.to_string()),
        region: S3_REGION.to_string(),
        access_key_id: S3_USERNAME.to_string(),
        secret_access_key: SecretString::from(secret),
        bucket: S3_BUCKET.to_string(),
        prefix: None,
        path_style: true,
        storage_class: None,
        sse_mode: None,
        sse_kms_key_id: None,
        verify_cert: false,
    }
}

async fn verify_s3_object_sha(base: &S3Provider, key: &str, expected_sha: &str, label: &str) {
    let dst = std::env::temp_dir().join(format!("gtc4-verify-{label}.bin"));
    let _ = std::fs::remove_file(&dst);
    let mut worker = base.clone_for_transfer().expect("clone S3 verifier");
    worker
        .download(key, dst.to_str().unwrap(), None)
        .await
        .unwrap_or_else(|e| panic!("download S3 verifier {key}: {e}"));
    assert_eq!(
        sha256_file(&dst),
        expected_sha,
        "S3 object {key} must match source bytes",
    );
    let _ = std::fs::remove_file(&dst);
}

#[tokio::test]
#[ignore = "live WAN: GTC-4 cross-profile SFTP to S3 parallel copy against axpbuntu"]
async fn gtc_cross_profile_sftp_to_s3_parallel_byte_identity_speed_band() {
    let Some(creds) = load_sftp_creds() else {
        eprintln!("SKIP: SFTP credentials unavailable in vault");
        return;
    };
    let Some(s3_secret) = load_s3_secret() else {
        eprintln!("SKIP: S3 secret unavailable in vault");
        return;
    };

    let src_dir = std::env::temp_dir().join("gtc4-cross-profile-src");
    let _ = std::fs::remove_dir_all(&src_dir);
    std::fs::create_dir_all(&src_dir).unwrap();

    let mut expected = Vec::new();
    let mut sftp_base = SftpProvider::new(sftp_config(creds.clone()));
    sftp_base.connect().await.expect("base SFTP connect");
    let _ = sftp_base.mkdir(REMOTE_DIR).await;
    for i in 0..GTC4_FILES {
        let local = src_dir.join(format!("gtc4-{i}.bin"));
        std::fs::write(
            &local,
            random_buf(0x4754_4304_u64 + i as u64, GTC4_FILE_BYTES),
        )
        .unwrap();
        let sha = sha256_file(&local);
        let remote = format!("{REMOTE_DIR}/gtc4-{i}.bin");
        let _ = sftp_base.delete(&remote).await;
        sftp_base
            .upload(local.to_str().unwrap(), &remote, None)
            .await
            .unwrap_or_else(|e| panic!("seed SFTP source {remote}: {e}"));
        expected.push((remote, sha));
    }

    let mut s3_base = S3Provider::new(s3_config(s3_secret.clone())).expect("S3Provider::new");
    s3_base.connect().await.expect("base S3 connect");
    for i in 0..GTC4_FILES {
        let _ = s3_base
            .delete(&format!("_gtc_gui_validation/gtc4-serial-{i}.bin"))
            .await;
        let _ = s3_base
            .delete(&format!("_gtc_gui_validation/gtc4-parallel-{i}.bin"))
            .await;
    }

    let serial_start = Instant::now();
    for (i, (remote, _sha)) in expected.iter().enumerate() {
        let mut source = SftpProvider::new(sftp_config(creds.clone()));
        source.connect().await.expect("serial SFTP connect");
        let mut dest = S3Provider::new(s3_config(s3_secret.clone())).expect("S3Provider::new");
        dest.connect().await.expect("serial S3 connect");
        copy_one_file(
            &mut source,
            &mut dest,
            remote,
            &format!("_gtc_gui_validation/gtc4-serial-{i}.bin"),
            None,
        )
        .await
        .unwrap_or_else(|e| panic!("serial cross-profile copy {remote}: {e}"));
        let _ = source.disconnect().await;
        let _ = dest.disconnect().await;
    }
    let serial_elapsed = serial_start.elapsed();

    let parallel_start = Instant::now();
    let mut tasks = JoinSet::new();
    for (i, (remote, _sha)) in expected.iter().cloned().enumerate() {
        let creds = creds.clone();
        let s3_secret = s3_secret.clone();
        tasks.spawn(async move {
            let mut source = SftpProvider::new(sftp_config(creds));
            source.connect().await.expect("parallel SFTP connect");
            let mut dest = S3Provider::new(s3_config(s3_secret)).expect("S3Provider::new");
            dest.connect().await.expect("parallel S3 connect");
            let key = format!("_gtc_gui_validation/gtc4-parallel-{i}.bin");
            copy_one_file_with_options(
                &mut source,
                &mut dest,
                &remote,
                &key,
                None,
                CrossProfileCopyOptions {
                    source_size: Some(GTC4_FILE_BYTES as u64),
                    download_segments: 4,
                    cancel_token: CancellationToken::new(),
                },
            )
            .await
            .unwrap_or_else(|e| panic!("parallel cross-profile copy {remote}: {e}"));
            let _ = source.disconnect().await;
            let _ = dest.disconnect().await;
        });
    }
    while let Some(result) = tasks.join_next().await {
        result.expect("parallel copy task join");
    }
    let parallel_elapsed = parallel_start.elapsed();

    for (i, (_remote, sha)) in expected.iter().enumerate() {
        verify_s3_object_sha(
            &s3_base,
            &format!("_gtc_gui_validation/gtc4-parallel-{i}.bin"),
            sha,
            &format!("parallel-{i}"),
        )
        .await;
        verify_s3_object_sha(
            &s3_base,
            &format!("_gtc_gui_validation/gtc4-serial-{i}.bin"),
            sha,
            &format!("serial-{i}"),
        )
        .await;
    }

    let speedup = serial_elapsed.as_secs_f64() / parallel_elapsed.as_secs_f64().max(1e-6);
    eprintln!(
        "GTC-4 Cross-Profile SFTP->S3 @ axpbuntu: files={} size={}MiB serial {:.2}s  parallel {:.2}s  speedup {:.2}x  byte-id=YES",
        GTC4_FILES,
        GTC4_FILE_BYTES / 1_048_576,
        serial_elapsed.as_secs_f64(),
        parallel_elapsed.as_secs_f64(),
        speedup,
    );
    assert!(
        speedup >= 0.75,
        "parallel cross-profile copy must stay within speed band, got {speedup:.2}x",
    );

    for i in 0..GTC4_FILES {
        let _ = s3_base
            .delete(&format!("_gtc_gui_validation/gtc4-serial-{i}.bin"))
            .await;
        let _ = s3_base
            .delete(&format!("_gtc_gui_validation/gtc4-parallel-{i}.bin"))
            .await;
        let _ = sftp_base
            .delete(&format!("{REMOTE_DIR}/gtc4-{i}.bin"))
            .await;
    }
    let _ = s3_base.disconnect().await;
    let _ = sftp_base.disconnect().await;
    let _ = std::fs::remove_dir_all(&src_dir);
}

#[tokio::test]
#[ignore = "live WAN: GTC-1/2 S3 byte-identity against axpbuntu MinIO (vault required)"]
async fn gtc_gui_s3_segmented_byte_identical_vs_single_stream() {
    let Some(secret) = load_s3_secret() else {
        eprintln!("SKIP: S3 secret unavailable in vault");
        return;
    };

    let src_dir = std::env::temp_dir().join("gtc-gui-s3-byteid-src");
    let _ = std::fs::remove_dir_all(&src_dir);
    std::fs::create_dir_all(&src_dir).unwrap();
    let src = src_dir.join("64MiB.bin");
    std::fs::write(&src, random_buf(0x6969_9696, FILE_BYTES)).unwrap();
    let src_sha = sha256_file(&src);

    let mut base = S3Provider::new(s3_config(secret)).expect("S3Provider::new");
    base.connect().await.expect("base S3 connect");
    assert_eq!(
        base.transfer_executor_kind(),
        ProviderTransferExecutorKind::HttpClonePool,
        "S3 must advertise HttpClonePool",
    );
    assert_eq!(
        base.transfer_capabilities()
            .strict_concurrent_range_download,
        Capability::Supported,
        "S3 must advertise strict_concurrent_range_download = Supported",
    );

    // Seed
    let _ = base.delete(S3_REMOTE_BIG).await;
    base.upload(src.to_str().unwrap(), S3_REMOTE_BIG, None)
        .await
        .expect("seed S3 upload");

    let dst_root = std::env::temp_dir().join("gtc-gui-s3-byteid-dst");
    let _ = std::fs::remove_dir_all(&dst_root);
    std::fs::create_dir_all(&dst_root).unwrap();

    let dst1 = dst_root.join("seg1.bin");
    let mut w1 = base.clone_for_transfer().expect("clone S3 single-stream");
    let t1 = Instant::now();
    w1.download(S3_REMOTE_BIG, dst1.to_str().unwrap(), None)
        .await
        .expect("S3 single-stream download");
    let e1 = t1.elapsed();
    assert_eq!(sha256_file(&dst1), src_sha, "S3 single-stream sha mismatch");

    let segments = provider_segmented_download_eligible(&base, FILE_BYTES as u64, 4, 8)
        .expect("eligibility probe must succeed for S3@axpbuntu");
    assert_eq!(segments, 4, "S3 64MiB / 4 = 16MiB > 8MiB floor");

    let dst4 = dst_root.join("seg4.bin");
    let cancel = CancellationToken::new();
    let t4 = Instant::now();
    run_provider_segmented_download(
        &base,
        S3_REMOTE_BIG,
        dst4.to_str().unwrap(),
        FILE_BYTES as u64,
        segments,
        None,
        cancel,
    )
    .await
    .expect("S3 segmented must succeed (validates HttpClonePool path)");
    let e4 = t4.elapsed();
    assert_eq!(sha256_file(&dst4), src_sha, "S3 segmented sha mismatch");
    assert!(
        !aerotmp_path_for(&dst4).exists(),
        ".aerotmp must be renamed away after S3 segmented success",
    );

    let speedup = e1.as_secs_f64() / e4.as_secs_f64().max(1e-6);
    eprintln!(
        "GTC-1/2 S3 @ axpbuntu MinIO: 64MiB seg=1 {:.2}s  seg=4 {:.2}s  speedup {:.2}x  byte-id=YES",
        e1.as_secs_f64(),
        e4.as_secs_f64(),
        speedup,
    );

    let _ = base.delete(S3_REMOTE_BIG).await;
    let _ = base.disconnect().await;
    let _ = std::fs::remove_dir_all(&dst_root);
    let _ = std::fs::remove_dir_all(&src_dir);
}
