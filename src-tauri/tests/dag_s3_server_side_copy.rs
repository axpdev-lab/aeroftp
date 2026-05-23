// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! SG-T15: live integration for the shaped-copy server-side path on S3.
//! Verifies that a same-bucket copy through the provider's native
//! `server_side_copy` (`PUT ?x-amz-copy-source: ...`) succeeds without
//! routing the bytes through the local host.
//!
//! Marked `#[ignore]`: requires the S3 lab vault entry. Run:
//! ```bash
//! cargo test --release --test dag_s3_server_side_copy -- --ignored --nocapture
//! ```

#![cfg(unix)]

use std::path::Path;

use ftp_client_gui_lib::credential_store::CredentialStore;
use ftp_client_gui_lib::providers::s3::S3Provider;
use ftp_client_gui_lib::providers::types::S3Config;
use ftp_client_gui_lib::providers::StorageProvider;
use secrecy::SecretString;
use sha2::{Digest, Sha256};

const S3_PROFILE_ID: &str = "srv_axpbuntu_s3_minio";
const S3_USERNAME: &str = "aeroftp-admin";
const S3_BUCKET: &str = "aeroftp-test";
const S3_REGION: &str = "us-east-1";
const S3_ENDPOINT: &str = "https://s3.lab.axpdev.it";
const S3_REMOTE_SOURCE: &str = "_dag_validation/copy_source.bin";
const S3_REMOTE_DEST: &str = "_dag_validation/copy_dest.bin";
const FILE_BYTES: usize = 4 * 1024 * 1024;

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

fn sha256_bytes(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    format!("{:x}", h.finalize())
}

fn random_buf(seed: u64, size: usize) -> Vec<u8> {
    let mut buf = Vec::with_capacity(size);
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    while buf.len() < size {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        buf.extend_from_slice(&state.to_le_bytes());
    }
    buf.truncate(size);
    buf
}

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

#[tokio::test]
#[ignore = "live WAN: SG-T15 S3 server-side copy (vault required)"]
async fn gtc_dag_s3_server_side_copy() {
    let Some(secret) = load_s3_secret() else {
        eprintln!("SKIP: S3 secret unavailable in vault");
        return;
    };

    // Seed a fixture object on S3 (the copy source). Upload happens
    // through the legacy provider path; the test is exclusively about
    // the server-side-copy native trait method that the shaped-copy
    // graph dispatches.
    let src_dir = std::env::temp_dir().join("dag-s3-copy-src");
    let _ = std::fs::remove_dir_all(&src_dir);
    std::fs::create_dir_all(&src_dir).unwrap();
    let local_src = src_dir.join("source.bin");
    let payload = random_buf(0xC04F_E5A1, FILE_BYTES);
    std::fs::write(&local_src, &payload).unwrap();
    let src_sha = sha256_bytes(&payload);

    let mut base = S3Provider::new(s3_config(secret.clone())).expect("S3Provider::new");
    base.connect().await.expect("S3 connect");
    let _ = base.delete(S3_REMOTE_SOURCE).await;
    let _ = base.delete(S3_REMOTE_DEST).await;
    base.upload(local_src.to_str().unwrap(), S3_REMOTE_SOURCE, None)
        .await
        .expect("seed S3 copy source");

    // The provider must advertise server-side copy for the shaped-copy
    // graph to dispatch the `ServerSideCopy` node.
    assert!(
        base.supports_server_side_copy(),
        "S3 provider must advertise supports_server_side_copy"
    );

    // Drive the native server-side copy. The shaped-copy graph would
    // route a single `ServerSideCopy` node through this exact call; we
    // exercise it directly here so the test stays insensitive to the
    // shaped-copy wiring that lands in a future slice.
    base.server_side_copy(S3_REMOTE_SOURCE, S3_REMOTE_DEST)
        .await
        .expect("server-side copy must succeed");

    // Verify the destination object's bytes match the source.
    let dst_local = std::env::temp_dir().join("dag-s3-copy-dst.bin");
    let _ = std::fs::remove_file(&dst_local);
    base.download(S3_REMOTE_DEST, dst_local.to_str().unwrap(), None)
        .await
        .expect("download copied object");
    let dst_sha = sha256_file(&dst_local);
    assert_eq!(
        dst_sha, src_sha,
        "S3 server-side copy destination must match the source bytes \
         (expected sha {src_sha}, got {dst_sha})"
    );

    let _ = base.delete(S3_REMOTE_SOURCE).await;
    let _ = base.delete(S3_REMOTE_DEST).await;
    let _ = std::fs::remove_file(&dst_local);
    let _ = std::fs::remove_dir_all(&src_dir);

    eprintln!("SG-T15 OK: S3 server-side copy byte-identical, sha {src_sha}");
}
