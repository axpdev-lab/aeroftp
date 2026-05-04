//! Integration tests for the Backblaze B2 native provider against the live
//! B2 service.
//!
//! These tests are marked `#[ignore]` so `cargo test` default runs skip them.
//! Run explicitly after exporting the credentials of a throw-away bucket:
//!
//! ```bash
//! export AEROFTP_TEST_B2_KEY_ID=K00xxxxxxxxxxxxxxxxxx
//! export AEROFTP_TEST_B2_KEY=K00yyyyyyyyyyyyyyyyyyyy
//! export AEROFTP_TEST_B2_BUCKET=aeroftp-test-bucket
//! cd src-tauri
//! cargo test --test integration_b2 -- --ignored --nocapture
//! ```
//!
//! All tests cooperate inside an isolated prefix (`aeroftp-it/<run-id>/`) and
//! delete every artifact at the end. Use a dedicated bucket: keep no other
//! data there.
//!
//! Cost note: the multipart test uploads a 250 MB blob and downloads it back.
//! B2 free tier is 10 GB stored + 1 GB download/day; running the suite once
//! per day is well within the free quota.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use ftp_client_gui_lib::providers::b2::{B2Config, B2Provider};
use ftp_client_gui_lib::providers::{ProviderError, StorageProvider};
use secrecy::SecretString;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Pull `(keyId, applicationKey, bucket)` from the environment, or `None` if
/// any is missing or empty. Returning a tuple keeps the skip path single-line.
fn b2_creds_from_env() -> Option<(String, String, String)> {
    let key_id = std::env::var("AEROFTP_TEST_B2_KEY_ID").ok()?;
    let key = std::env::var("AEROFTP_TEST_B2_KEY").ok()?;
    let bucket = std::env::var("AEROFTP_TEST_B2_BUCKET").ok()?;
    if key_id.is_empty() || key.is_empty() || bucket.is_empty() {
        return None;
    }
    Some((key_id, key, bucket))
}

fn skip_unless_creds(test_name: &str) -> Option<(String, String, String)> {
    match b2_creds_from_env() {
        Some(c) => Some(c),
        None => {
            eprintln!(
                "[{}] skipped: set AEROFTP_TEST_B2_KEY_ID, AEROFTP_TEST_B2_KEY, AEROFTP_TEST_B2_BUCKET to enable",
                test_name
            );
            None
        }
    }
}

/// Deterministic per-run prefix so concurrent CI runs don't collide.
fn run_prefix(label: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("aeroftp-it/{}-{}/", label, nanos)
}

fn make_provider(creds: &(String, String, String), initial_path: Option<String>) -> B2Provider {
    B2Provider::new(B2Config {
        application_key_id: creds.0.clone(),
        application_key: SecretString::new(creds.1.clone().into()),
        bucket: creds.2.clone(),
        initial_path,
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    let d = h.finalize();
    let mut s = String::with_capacity(d.len() * 2);
    for b in d {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

async fn read_local_sha256(path: &std::path::Path) -> String {
    let bytes = tokio::fs::read(path).await.expect("read local for sha256");
    sha256_hex(&bytes)
}

/// Best-effort sweep: hide every file under `prefix` so the test does not
/// leak artifacts. Caller already deals with auth state, so failures here
/// are logged and swallowed.
async fn cleanup_prefix(p: &mut B2Provider, prefix: &str) {
    let abs = format!("/{}", prefix.trim_end_matches('/'));
    if let Err(e) = p.rmdir_recursive(&abs).await {
        eprintln!("cleanup_prefix: rmdir_recursive({}) failed: {}", abs, e);
    }
}

#[tokio::test]
#[ignore = "requires AEROFTP_TEST_B2_* env"]
async fn connect_list_pwd_disconnect_round_trip() {
    let creds = match skip_unless_creds("connect_list_pwd_disconnect_round_trip") {
        Some(c) => c,
        None => return,
    };
    let mut p = make_provider(&creds, None);
    p.connect().await.expect("connect");
    assert!(
        p.is_connected(),
        "connected flag must be true after connect"
    );
    let cwd = p.pwd().await.expect("pwd");
    assert_eq!(cwd, "/", "default cwd is bucket root");
    let _entries = p.list("/").await.expect("list root");
    p.disconnect().await.expect("disconnect");
    assert!(
        !p.is_connected(),
        "connected flag must be false after disconnect"
    );
}

#[tokio::test]
#[ignore = "requires AEROFTP_TEST_B2_* env"]
async fn small_upload_download_checksum_match_and_cleanup() {
    let creds = match skip_unless_creds("small_upload_download_checksum_match_and_cleanup") {
        Some(c) => c,
        None => return,
    };
    let prefix = run_prefix("small");
    let key = format!("{}hello.bin", prefix);
    let mut p = make_provider(&creds, None);
    p.connect().await.expect("connect");

    // Build a 1 MiB blob with a non-trivial pattern.
    let payload: Vec<u8> = (0..1024 * 1024).map(|i| (i & 0xff) as u8).collect();
    let expected_sha = sha256_hex(&payload);

    let local_dir = std::env::temp_dir().join(format!("aeroftp-it-{}", std::process::id()));
    tokio::fs::create_dir_all(&local_dir)
        .await
        .expect("mk tmp dir");
    let local_in: PathBuf = local_dir.join("upload.bin");
    let local_out: PathBuf = local_dir.join("download.bin");
    tokio::fs::write(&local_in, &payload)
        .await
        .expect("write upload");

    let upload_result = p
        .upload(local_in.to_str().unwrap(), &format!("/{}", key), None)
        .await;
    assert!(upload_result.is_ok(), "upload failed: {:?}", upload_result);

    // Confirm the file shows up on a directory listing.
    let listing = p
        .list(&format!("/{}", prefix.trim_end_matches('/')))
        .await
        .expect("list after upload");
    assert!(
        listing.iter().any(|e| e.name == "hello.bin"),
        "uploaded file missing from listing: {:?}",
        listing
    );

    // Round-trip check.
    p.download(&format!("/{}", key), local_out.to_str().unwrap(), None)
        .await
        .expect("download");
    let actual_sha = read_local_sha256(&local_out).await;
    assert_eq!(actual_sha, expected_sha, "round-trip checksum mismatch");

    cleanup_prefix(&mut p, &prefix).await;
    let _ = tokio::fs::remove_dir_all(&local_dir).await;
    p.disconnect().await.ok();
}

#[tokio::test]
#[ignore = "requires AEROFTP_TEST_B2_* env"]
async fn large_upload_forces_chunked_workflow_and_round_trips() {
    // 250 MB >  SINGLE_UPLOAD_RECOMMENDED_MAX (200 MB) → exercises
    // start_large_file / get_upload_part_url / upload_part / finish_large_file.
    let creds = match skip_unless_creds("large_upload_forces_chunked_workflow_and_round_trips") {
        Some(c) => c,
        None => return,
    };
    let prefix = run_prefix("large");
    let key = format!("{}large.bin", prefix);
    let mut p = make_provider(&creds, None);
    p.connect().await.expect("connect");

    let size: usize = 250 * 1024 * 1024;
    let mut payload = vec![0u8; size];
    // Fill with a pseudo-random pattern so SHA-256 is not all-zeros (which
    // would mask streaming errors).
    for (i, b) in payload.iter_mut().enumerate() {
        *b = ((i.wrapping_mul(2654435761)) & 0xff) as u8;
    }
    let expected_sha = sha256_hex(&payload);

    let local_dir = std::env::temp_dir().join(format!("aeroftp-it-large-{}", std::process::id()));
    tokio::fs::create_dir_all(&local_dir)
        .await
        .expect("mk tmp dir");
    let local_in: PathBuf = local_dir.join("upload.bin");
    let local_out: PathBuf = local_dir.join("download.bin");
    tokio::fs::write(&local_in, &payload)
        .await
        .expect("write upload");
    drop(payload); // release 250 MB before the round trip

    p.upload(local_in.to_str().unwrap(), &format!("/{}", key), None)
        .await
        .expect("large upload");
    p.download(&format!("/{}", key), local_out.to_str().unwrap(), None)
        .await
        .expect("large download");
    let actual_sha = read_local_sha256(&local_out).await;
    assert_eq!(
        actual_sha, expected_sha,
        "large round-trip checksum mismatch"
    );

    cleanup_prefix(&mut p, &prefix).await;
    let _ = tokio::fs::remove_dir_all(&local_dir).await;
    p.disconnect().await.ok();
}

#[tokio::test]
#[ignore = "requires AEROFTP_TEST_B2_* env"]
async fn rename_moves_file_and_source_disappears() {
    let creds = match skip_unless_creds("rename_moves_file_and_source_disappears") {
        Some(c) => c,
        None => return,
    };
    let prefix = run_prefix("rename");
    let src_key = format!("{}original.txt", prefix);
    let dst_key = format!("{}renamed.txt", prefix);
    let mut p = make_provider(&creds, None);
    p.connect().await.expect("connect");

    let local_dir = std::env::temp_dir().join(format!("aeroftp-it-rename-{}", std::process::id()));
    tokio::fs::create_dir_all(&local_dir)
        .await
        .expect("mk tmp dir");
    let local_in = local_dir.join("rename-payload.txt");
    tokio::fs::write(&local_in, b"rename payload\n")
        .await
        .expect("write");

    p.upload(local_in.to_str().unwrap(), &format!("/{}", src_key), None)
        .await
        .expect("upload");
    p.rename(&format!("/{}", src_key), &format!("/{}", dst_key))
        .await
        .expect("rename");

    // Source must disappear, destination must be present.
    assert!(
        !p.exists(&format!("/{}", src_key)).await.unwrap_or(true),
        "source key still exists after rename"
    );
    assert!(
        p.exists(&format!("/{}", dst_key)).await.unwrap_or(false),
        "destination key missing after rename"
    );

    cleanup_prefix(&mut p, &prefix).await;
    let _ = tokio::fs::remove_dir_all(&local_dir).await;
    p.disconnect().await.ok();
}

#[tokio::test]
#[ignore = "requires AEROFTP_TEST_B2_* env"]
async fn rmdir_recursive_clears_a_subtree() {
    let creds = match skip_unless_creds("rmdir_recursive_clears_a_subtree") {
        Some(c) => c,
        None => return,
    };
    let prefix = run_prefix("rmdir");
    let mut p = make_provider(&creds, None);
    p.connect().await.expect("connect");

    let local_dir = std::env::temp_dir().join(format!("aeroftp-it-rmdir-{}", std::process::id()));
    tokio::fs::create_dir_all(&local_dir)
        .await
        .expect("mk tmp dir");
    let local_in = local_dir.join("payload.txt");
    tokio::fs::write(&local_in, b"x").await.expect("write");

    // Seed three files at varying depths.
    for k in ["a.txt", "sub/b.txt", "sub/deep/c.txt"] {
        let key = format!("{}{}", prefix, k);
        p.upload(local_in.to_str().unwrap(), &format!("/{}", key), None)
            .await
            .expect("seed upload");
    }
    let before = p
        .list(&format!("/{}", prefix.trim_end_matches('/')))
        .await
        .expect("list pre");
    assert!(!before.is_empty(), "seed should populate the prefix");

    p.rmdir_recursive(&format!("/{}", prefix.trim_end_matches('/')))
        .await
        .expect("rmdir_recursive");

    let after = p
        .list(&format!("/{}", prefix.trim_end_matches('/')))
        .await
        .expect("list post");
    assert!(after.is_empty(), "rmdir_recursive should empty the prefix");

    let _ = tokio::fs::remove_dir_all(&local_dir).await;
    p.disconnect().await.ok();
}

#[tokio::test]
#[ignore = "requires AEROFTP_TEST_B2_* env"]
async fn invalid_application_key_surfaces_authentication_failed() {
    // We do not need a real bucket to exercise the auth failure path -
    // authorize() rejects the credentials before list_buckets is called.
    let creds = match skip_unless_creds("invalid_application_key_surfaces_authentication_failed") {
        Some(c) => c,
        None => return,
    };
    let bad = (
        creds.0.clone(),
        "definitely-not-a-real-key".to_string(),
        creds.2,
    );
    let mut p = make_provider(&bad, None);
    let err = p.connect().await.expect_err("must reject bad credentials");
    // Either AuthenticationFailed or ConnectionFailed: different B2 backends
    // map the 401 differently. Both are acceptable; what we forbid is silent
    // "Ok" on bad creds.
    assert!(
        matches!(
            err,
            ProviderError::AuthenticationFailed(_)
                | ProviderError::PermissionDenied(_)
                | ProviderError::ConnectionFailed(_)
        ),
        "unexpected error variant on bad creds: {:?}",
        err
    );
}

/// Opt-in: rename of a file > 5 GB exercises the b2_copy_part chunked path.
/// This test is double-gated: it requires both the standard B2 env vars AND
/// `AEROFTP_TEST_B2_LARGE_RENAME=1` because it transfers and stores ~5.1 GB.
#[tokio::test]
#[ignore = "requires AEROFTP_TEST_B2_* + AEROFTP_TEST_B2_LARGE_RENAME=1 (transfers ~5.1 GB)"]
async fn rename_above_5gb_uses_chunked_copy_part_path() {
    if std::env::var("AEROFTP_TEST_B2_LARGE_RENAME")
        .ok()
        .as_deref()
        != Some("1")
    {
        eprintln!("[rename_above_5gb_uses_chunked_copy_part_path] skipped: set AEROFTP_TEST_B2_LARGE_RENAME=1 to enable");
        return;
    }
    let creds = match skip_unless_creds("rename_above_5gb_uses_chunked_copy_part_path") {
        Some(c) => c,
        None => return,
    };
    let prefix = run_prefix("rename-large");
    let src_key = format!("{}source.bin", prefix);
    let dst_key = format!("{}target.bin", prefix);
    let mut p = make_provider(&creds, None);
    p.connect().await.expect("connect");

    // 5 GB + 1 MB → guarantees the chunked path. We stream zeros from
    // /dev/zero so we don't need a 5 GB local buffer; the upload itself
    // will use the large-file workflow because size > 200 MB.
    let size: u64 = 5 * 1024 * 1024 * 1024 + 1024 * 1024;
    let local_dir =
        std::env::temp_dir().join(format!("aeroftp-it-rename-large-{}", std::process::id()));
    tokio::fs::create_dir_all(&local_dir)
        .await
        .expect("mk tmp dir");
    let local_in = local_dir.join("source.bin");
    // Use a sparse file: works on ext4/xfs/btrfs/apfs but not on every
    // filesystem; if sparse is unsupported the file will be physically zeros.
    {
        use tokio::io::{AsyncSeekExt, AsyncWriteExt};
        let mut f = tokio::fs::File::create(&local_in)
            .await
            .expect("create sparse");
        f.set_len(size).await.expect("set_len sparse");
        f.seek(std::io::SeekFrom::Start(size - 1))
            .await
            .expect("seek");
        f.write_all(&[0u8]).await.expect("trail byte");
        f.flush().await.expect("flush");
    }

    p.upload(local_in.to_str().unwrap(), &format!("/{}", src_key), None)
        .await
        .expect("upload 5 GB+");
    p.rename(&format!("/{}", src_key), &format!("/{}", dst_key))
        .await
        .expect("rename via copy_part");
    assert!(
        !p.exists(&format!("/{}", src_key)).await.unwrap_or(true),
        "source must disappear after chunked rename"
    );
    assert!(
        p.exists(&format!("/{}", dst_key)).await.unwrap_or(false),
        "destination must exist after chunked rename"
    );
    let dst_size = p.size(&format!("/{}", dst_key)).await.expect("size");
    assert_eq!(dst_size, size, "destination size must match source size");

    cleanup_prefix(&mut p, &prefix).await;
    let _ = tokio::fs::remove_dir_all(&local_dir).await;
    p.disconnect().await.ok();
}
