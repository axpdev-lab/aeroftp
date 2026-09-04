// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Live checks for Backblaze B2 reached through its **S3-compatible** endpoint,
//! which is a different code path from the native B2 provider covered by
//! `integration_b2.rs`: the native path reads B2's own `contentSha1`, this one
//! reads the S3 `ETag` and accepts it only when it is exactly 32 hex characters
//! (`etag_to_md5` in `providers/s3.rs`), omitting anything else rather than
//! publishing a value that is not an MD5.
//!
//! Why this file exists, and why now. From **2026-09-14** Backblaze encrypts
//! new uploads and copies with SSE-B2 by default. On AWS, SSE-S3 leaves the
//! ETag equal to the object MD5 for single-part objects, and only SSE-KMS and
//! SSE-C make it opaque, which is what the published caveat for the S3 ETag
//! says. Whether B2 behaves the same through its S3 layer is not something to
//! assume: if its ETag becomes opaque, `etag_to_md5` correctly returns nothing,
//! the digest quietly disappears, and `sync`, `dedupe` and `verify` fall back
//! to size and mtime with no error raised anywhere. These tests fail when that
//! happens, and print what the server actually reported so a run from before
//! the change can be compared with one from after it.
//!
//! Marked `#[ignore]`, and additionally gated on a throw-away bucket's
//! credentials plus the S3 endpoint for the bucket's region:
//!
//! ```bash
//! export AEROFTP_TEST_B2_KEY_ID=K00xxxxxxxxxxxxxxxxxx
//! export AEROFTP_TEST_B2_KEY=K00yyyyyyyyyyyyyyyyyyyy
//! export AEROFTP_TEST_B2_BUCKET=aeroftp-test-bucket
//! export AEROFTP_TEST_B2_S3_ENDPOINT=https://s3.eu-central-003.backblazeb2.com
//! cd src-tauri
//! cargo test --test integration_b2_s3_compat -- --ignored --nocapture
//! ```
//!
//! The region is read out of the endpoint host (`s3.<region>.backblazeb2.com`)
//! and can be overridden with `AEROFTP_TEST_B2_S3_REGION`. Traffic is a couple
//! of MB per run.

use ftp_client_gui_lib::providers::s3::S3Provider;
use ftp_client_gui_lib::providers::types::S3Config;
use ftp_client_gui_lib::providers::StorageProvider;
use md5::Md5;
use secrecy::SecretString;
use sha2::Digest;
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

struct Creds {
    key_id: String,
    key: String,
    bucket: String,
    endpoint: String,
    region: String,
}

/// Region for SigV4, from `AEROFTP_TEST_B2_S3_REGION` or read out of the B2 S3
/// endpoint host, which is `s3.<region>.backblazeb2.com`. Returns `None` when
/// neither is available: signing with a guessed region fails at connect with an
/// authentication error that would look like bad credentials.
fn region_for(endpoint: &str) -> Option<String> {
    if let Ok(explicit) = std::env::var("AEROFTP_TEST_B2_S3_REGION") {
        if !explicit.trim().is_empty() {
            return Some(explicit.trim().to_string());
        }
    }
    let host = endpoint
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let mut parts = host.split('.');
    match (parts.next(), parts.next()) {
        (Some("s3"), Some(region)) if !region.is_empty() => Some(region.to_string()),
        _ => None,
    }
}

fn skip_unless_creds(test_name: &str) -> Option<Creds> {
    let key_id = std::env::var("AEROFTP_TEST_B2_KEY_ID").unwrap_or_default();
    let key = std::env::var("AEROFTP_TEST_B2_KEY").unwrap_or_default();
    let bucket = std::env::var("AEROFTP_TEST_B2_BUCKET").unwrap_or_default();
    let endpoint = std::env::var("AEROFTP_TEST_B2_S3_ENDPOINT").unwrap_or_default();
    if key_id.is_empty() || key.is_empty() || bucket.is_empty() || endpoint.is_empty() {
        eprintln!(
            "[{}] skipped: set AEROFTP_TEST_B2_KEY_ID, AEROFTP_TEST_B2_KEY, AEROFTP_TEST_B2_BUCKET and AEROFTP_TEST_B2_S3_ENDPOINT to enable",
            test_name
        );
        return None;
    }
    let Some(region) = region_for(&endpoint) else {
        eprintln!(
            "[{}] skipped: cannot read a region out of {:?}; set AEROFTP_TEST_B2_S3_REGION",
            test_name, endpoint
        );
        return None;
    };
    Some(Creds {
        key_id,
        key,
        bucket,
        endpoint,
        region,
    })
}

fn make_provider(c: &Creds) -> S3Provider {
    S3Provider::new(S3Config {
        endpoint: Some(c.endpoint.clone()),
        region: c.region.clone(),
        access_key_id: c.key_id.clone(),
        secret_access_key: SecretString::from(c.key.clone()),
        session_token: None,
        role_arn: None,
        role_external_id: None,
        role_session_name: None,
        role_duration_seconds: None,
        role_mfa_serial: None,
        role_mfa_token_code: None,
        bucket: c.bucket.clone(),
        prefix: None,
        // B2's S3 layer serves buckets path-style, matching the `backblaze`
        // preset's own default.
        path_style: true,
        storage_class: None,
        // Deliberately not set: the point is what the SERVICE does by default,
        // which is what a user's profile will hit from 2026-09-14 onwards.
        sse_mode: None,
        sse_kms_key_id: None,
        verify_cert: true,
    })
    .expect("build S3 provider for the B2 S3-compatible endpoint")
}

fn md5_hex(bytes: &[u8]) -> String {
    let mut h = Md5::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

fn run_prefix(label: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("aeroftp-it/{}-{}/", label, nanos)
}

fn report(label: &str, digests: &HashMap<String, String>) {
    if digests.is_empty() {
        eprintln!("[b2-s3-digest-baseline] {label}: (none reported)");
        return;
    }
    let mut pairs: Vec<_> = digests.iter().collect();
    pairs.sort();
    for (algo, value) in pairs {
        eprintln!("[b2-s3-digest-baseline] {label}: {algo}={value}");
    }
}

async fn write_temp(name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("aeroftp-b2s3-{}", std::process::id()));
    tokio::fs::create_dir_all(&dir).await.expect("mk tmp dir");
    let path = dir.join(name);
    tokio::fs::write(&path, bytes).await.expect("write temp");
    path
}

#[tokio::test]
#[ignore = "requires AEROFTP_TEST_B2_* env + AEROFTP_TEST_B2_S3_ENDPOINT"]
async fn single_part_object_still_reports_its_etag_as_the_content_md5() {
    let Some(creds) =
        skip_unless_creds("single_part_object_still_reports_its_etag_as_the_content_md5")
    else {
        return;
    };
    let prefix = run_prefix("b2s3-single");
    let key = format!("{}hello.bin", prefix);
    let mut p = make_provider(&creds);
    p.connect().await.expect("connect");

    let payload: Vec<u8> = (0..1024 * 1024).map(|i| (i & 0xff) as u8).collect();
    let local = write_temp("upload.bin", &payload).await;

    p.upload(local.to_str().unwrap(), &format!("/{}", key), None)
        .await
        .expect("upload");

    let digests = p.checksum(&format!("/{}", key)).await.expect("checksum");
    report("single-part", &digests);
    let reported = digests.get("md5").cloned();

    // Clean up BEFORE asserting. A changed digest is the failure this test
    // exists to catch, so the assertion below is expected to fire one day, and
    // a panic above the delete would leave the object behind in exactly the
    // case the test was written for.
    let _ = p.delete(&format!("/{}", key)).await;
    let _ = tokio::fs::remove_file(&local).await;
    p.disconnect().await.ok();

    assert_eq!(
        reported.as_deref(),
        Some(md5_hex(&payload).as_str()),
        "B2's S3 ETag is no longer the object MD5 for a single-part upload: the digest is gone and \
         sync/dedupe/verify silently fall back to size and mtime"
    );
}

#[tokio::test]
#[ignore = "requires AEROFTP_TEST_B2_* env + AEROFTP_TEST_B2_S3_ENDPOINT"]
async fn server_side_copy_keeps_the_etag_usable() {
    let Some(creds) = skip_unless_creds("server_side_copy_keeps_the_etag_usable") else {
        return;
    };
    let prefix = run_prefix("b2s3-copy");
    let src = format!("{}original.bin", prefix);
    let dst = format!("{}renamed.bin", prefix);
    let mut p = make_provider(&creds);
    p.connect().await.expect("connect");

    let payload = b"b2 s3-compat copy payload\n".to_vec();
    let local = write_temp("copy.bin", &payload).await;
    p.upload(local.to_str().unwrap(), &format!("/{}", src), None)
        .await
        .expect("upload");

    // A rename here is a server-side copy: the destination bytes are written by
    // the service, not uploaded by us, which is the case most exposed to a
    // change in how the service writes objects.
    p.rename(&format!("/{}", src), &format!("/{}", dst))
        .await
        .expect("rename");

    let digests = p.checksum(&format!("/{}", dst)).await.expect("checksum");
    report("server-side-copy", &digests);
    let reported = digests.get("md5").cloned();

    let _ = p.delete(&format!("/{}", dst)).await;
    let _ = tokio::fs::remove_file(&local).await;
    p.disconnect().await.ok();

    assert_eq!(
        reported.as_deref(),
        Some(md5_hex(&payload).as_str()),
        "the server-side copy's ETag is no longer the content MD5"
    );
}
