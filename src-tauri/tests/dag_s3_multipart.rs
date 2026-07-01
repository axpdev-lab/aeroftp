// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! SG-T14: live byte-identical integration for the shaped-graph multipart
//! upload path. Drives [`execute_single_file_dag`] over the S3 lab profile
//! with a capability set that advertises `multipart_upload`, so the runner
//! orchestrates the native `begin_multipart_upload` -> N `upload_part` ->
//! `complete_multipart_upload` flow. Verifies the finalized object's
//! sha256 matches the seed buffer.
//!
//! Marked `#[ignore]`: requires the S3 lab vault entry. Run:
//! ```bash
//! cargo test --release --test dag_s3_multipart -- --ignored --nocapture
//! ```

#![cfg(unix)]

use std::path::Path;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use ftp_client_gui_lib::credential_store::CredentialStore;
use ftp_client_gui_lib::providers::s3::S3Provider;
use ftp_client_gui_lib::providers::types::S3Config;
use ftp_client_gui_lib::providers::StorageProvider;
use ftp_client_gui_lib::transfer_dag::{
    DagObserver, NoopDagObserver, TransferDagBuilder, TransferDirection,
};
use ftp_client_gui_lib::transfer_dag_single_file::execute_single_file_dag;
use secrecy::SecretString;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

const S3_PROFILE_ID: &str = "srv_axpbuntu_s3_minio";
const S3_USERNAME: &str = "s3-tester";
const S3_BUCKET: &str = "aeroftp-test";
const S3_REGION: &str = "us-east-1";
const S3_ENDPOINT: &str = "https://s3.lab.example.test";
const S3_REMOTE_MULTIPART: &str = "_dag_validation/multipart_48MiB.bin";
// 48 MiB fixture so the shaped builder produces >= 3 `UploadPart` nodes when
// driven through the S3 provider's advertised `preferred_chunk_size` of 16 MiB
// (`S3Provider::MULTIPART_PART_SIZE`, surfaced via `transfer_optimization_hints`
// -> `TransferCapabilities::from_provider_hints`). The earlier 24 MiB fixture
// silently degraded to 2 parts once S3 started advertising its real 16 MiB
// preference instead of the builder's 8 MiB fallback.
const FILE_BYTES: usize = 48 * 1024 * 1024;

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
        session_token: None,
        role_arn: None,
        role_external_id: None,
        role_session_name: None,
        role_duration_seconds: None,
        role_mfa_serial: None,
        role_mfa_token_code: None,
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
#[ignore = "live WAN: SG-T14 S3 multipart shaped-graph upload (vault required)"]
async fn gtc_dag_s3_multipart_byte_identical() {
    let Some(secret) = load_s3_secret() else {
        eprintln!("SKIP: S3 secret unavailable in vault");
        return;
    };

    // Seed a 48 MiB random fixture; the shaped builder partitions it into
    // 3 x 16 MiB parts because S3 advertises a 16 MiB `preferred_chunk_size`.
    let src_dir = std::env::temp_dir().join("dag-s3-multipart-src");
    let _ = std::fs::remove_dir_all(&src_dir);
    std::fs::create_dir_all(&src_dir).unwrap();
    let src = src_dir.join("48MiB.bin");
    let payload = random_buf(0xD8A6_53A1, FILE_BYTES);
    std::fs::write(&src, &payload).unwrap();
    let src_sha = sha256_bytes(&payload);
    assert_eq!(sha256_file(&src), src_sha);

    // Base provider seeds the destination tree and verifies the final
    // object after the DAG upload completes.
    let mut base = S3Provider::new(s3_config(secret.clone())).expect("S3Provider::new base");
    base.connect().await.expect("base S3 connect");
    let _ = base.delete(S3_REMOTE_MULTIPART).await;

    // Worker provider: the connected handle the DAG runner drives. The
    // shaped builder reads its `transfer_capabilities()` to pick the
    // multipart fan-out shape; the provider's native multipart trait
    // methods perform the begin/upload_part/complete dispatch.
    let worker_box: Box<dyn StorageProvider> =
        Box::new(S3Provider::new(s3_config(secret.clone())).expect("S3Provider::new worker"));
    let arc_provider = Arc::new(Mutex::new(Some(worker_box)));
    {
        let mut guard = arc_provider.lock().await;
        guard
            .as_mut()
            .unwrap()
            .connect()
            .await
            .expect("worker S3 connect");
    }

    let caps = {
        let guard = arc_provider.lock().await;
        guard
            .as_ref()
            .map(|p| p.transfer_capabilities())
            .unwrap_or_default()
    };
    assert!(
        caps.multipart_upload.is_available(),
        "S3 provider must advertise multipart_upload capability"
    );

    let file_size = std::fs::metadata(&src).unwrap().len();
    let built = TransferDagBuilder::shaped_file(TransferDirection::Upload, &caps, file_size);
    assert!(
        built.transfer.len() >= 3,
        "48 MiB / 16 MiB part size must produce >= 3 UploadPart nodes, got {}",
        built.transfer.len()
    );

    let report_size = Arc::new(AtomicU64::new(file_size));
    let observer: Arc<dyn DagObserver> = Arc::new(NoopDagObserver);
    execute_single_file_dag(
        &built,
        Arc::clone(&arc_provider),
        S3_REMOTE_MULTIPART.to_string(),
        src.to_str().unwrap().to_string(),
        None,
        None,
        observer,
        report_size,
        file_size,
        None,
    )
    .await
    .expect("shaped multipart upload must succeed");

    // Download and verify byte identity. Skip if the part size in this
    // build differs from the test assumption: the byte-identical contract
    // is what matters, not the part count.
    let dst = std::env::temp_dir().join("dag-s3-multipart-dst.bin");
    let _ = std::fs::remove_file(&dst);
    base.download(S3_REMOTE_MULTIPART, dst.to_str().unwrap(), None)
        .await
        .expect("download finalized object for verification");
    let dst_sha = sha256_file(&dst);
    assert_eq!(
        dst_sha, src_sha,
        "S3 multipart finalized object must match the source bytes \
         (expected sha {src_sha}, got {dst_sha})"
    );

    let _ = base.delete(S3_REMOTE_MULTIPART).await;
    let _ = std::fs::remove_file(&dst);
    let _ = std::fs::remove_dir_all(&src_dir);

    eprintln!(
        "SG-T14 OK: S3 shaped-graph multipart upload byte-identical, \
         {} parts, sha {}",
        built.transfer.len(),
        src_sha
    );
}
