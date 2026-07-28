// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! SG-T16: live byte-identical integration for the shaped-graph multipart
//! upload path on Backblaze B2. Drives [`execute_single_file_dag`] over a
//! B2 profile with a capability set that advertises `multipart_upload`,
//! so the runner orchestrates the native `b2_start_large_file` ->
//! `b2_upload_part` -> `b2_finish_large_file` flow.
//!
//! Marked `#[ignore]`: requires a B2 vault entry under `srv_b2_dag`.
//! Run:
//! ```bash
//! cargo test --release --test dag_b2_multipart -- --ignored --nocapture
//! ```

#![cfg(unix)]

use std::path::Path;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use ftp_client_gui_lib::credential_store::CredentialStore;
use ftp_client_gui_lib::providers::b2::{B2Config, B2Provider};
use ftp_client_gui_lib::providers::StorageProvider;
use ftp_client_gui_lib::transfer_dag::{
    DagObserver, NoopDagObserver, TransferDagBuilder, TransferDirection,
};
use ftp_client_gui_lib::transfer_dag_single_file::execute_single_file_dag;
use secrecy::SecretString;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

const B2_PROFILE_ID: &str = "srv_b2_dag";
const B2_REMOTE_MULTIPART: &str = "_dag_validation/multipart_256MiB.bin";
// 256 MiB fixture: B2 advertises `LARGE_FILE_PART_SIZE = 100 MiB` as its
// `preferred_chunk_size`, and `SINGLE_UPLOAD_RECOMMENDED_MAX = 200 MiB` is the
// threshold below which B2 stays on the single-PUT path. 256 MiB clears both
// gates: the shaped builder produces 3 `UploadPart` nodes, and B2 actually
// drives `b2_start_large_file` -> `b2_upload_part` -> `b2_finish_large_file`.
const FILE_BYTES: usize = 256 * 1024 * 1024;

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

/// Load `(application_key_id, application_key, bucket)` from the vault.
/// Returns `None` when the vault is not unlocked or the profile is absent.
fn load_b2_creds() -> Option<(String, String, String)> {
    let init_result = CredentialStore::init().ok()?;
    if init_result != "OK" {
        eprintln!("SKIP: vault requires master password ({init_result})");
        return None;
    }
    let store = CredentialStore::from_cache()?;
    let raw = store
        .get(&format!("server_{}", B2_PROFILE_ID))
        .ok()
        .filter(|s| !s.is_empty())?;
    let val: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let obj = val.as_object()?;
    let key_id = obj.get("username").and_then(|v| v.as_str())?.to_string();
    let key = obj.get("password").and_then(|v| v.as_str())?.to_string();
    let bucket = obj
        .get("extra")
        .and_then(|v| v.as_object())
        .and_then(|o| o.get("bucket"))
        .and_then(|v| v.as_str())?
        .to_string();
    Some((key_id, key, bucket))
}

fn b2_config(key_id: String, key: String, bucket: String) -> B2Config {
    B2Config {
        application_key_id: key_id,
        application_key: SecretString::from(key),
        bucket,
        initial_path: None,
    }
}

#[tokio::test]
#[ignore = "live WAN: SG-T16 B2 multipart shaped-graph upload (vault required)"]
async fn gtc_dag_b2_multipart_byte_identical() {
    let Some((key_id, key, bucket)) = load_b2_creds() else {
        eprintln!("SKIP: B2 credentials unavailable in vault (profile {B2_PROFILE_ID})");
        return;
    };

    let src_dir = std::env::temp_dir().join("dag-b2-multipart-src");
    let _ = std::fs::remove_dir_all(&src_dir);
    std::fs::create_dir_all(&src_dir).unwrap();
    let src = src_dir.join("256MiB.bin");
    let payload = random_buf(0xB2BA_FEED, FILE_BYTES);
    std::fs::write(&src, &payload).unwrap();
    let src_sha = sha256_bytes(&payload);

    let mut base = B2Provider::new(b2_config(key_id.clone(), key.clone(), bucket.clone()));
    base.connect().await.expect("base B2 connect");
    let _ = base.delete(B2_REMOTE_MULTIPART).await;

    let worker: Box<dyn StorageProvider> =
        Box::new(B2Provider::new(b2_config(key_id, key, bucket)));
    let arc_provider = Arc::new(Mutex::new(Some(worker)));
    {
        let mut guard = arc_provider.lock().await;
        guard
            .as_mut()
            .unwrap()
            .connect()
            .await
            .expect("worker B2 connect");
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
        "B2 provider must advertise multipart_upload capability"
    );

    let file_size = std::fs::metadata(&src).unwrap().len();
    let built = TransferDagBuilder::shaped_file(TransferDirection::Upload, &caps, file_size);
    assert!(
        built.transfer.len() >= 3,
        "256 MiB / 100 MiB part size must produce >= 3 UploadPart nodes, got {}",
        built.transfer.len()
    );

    let report_size = Arc::new(AtomicU64::new(file_size));
    let observer: Arc<dyn DagObserver> = Arc::new(NoopDagObserver);
    let checkpoint_dir = tempfile::tempdir().expect("checkpoint tempdir");
    execute_single_file_dag(
        &built,
        Arc::clone(&arc_provider),
        B2_REMOTE_MULTIPART.to_string(),
        src.to_str().unwrap().to_string(),
        None,
        None,
        observer,
        report_size,
        file_size,
        None,
        Some(
            ftp_client_gui_lib::transfer_dag::TransferCheckpointStore::new(checkpoint_dir.path())
                .expect("per-test checkpoint store"),
        ),
    )
    .await
    .expect("shaped multipart upload must succeed");

    let dst = std::env::temp_dir().join("dag-b2-multipart-dst.bin");
    let _ = std::fs::remove_file(&dst);
    base.download(B2_REMOTE_MULTIPART, dst.to_str().unwrap(), None)
        .await
        .expect("download finalized object for verification");
    let dst_sha = sha256_file(&dst);
    assert_eq!(
        dst_sha, src_sha,
        "B2 multipart finalized object must match the source bytes \
         (expected sha {src_sha}, got {dst_sha})"
    );

    let _ = base.delete(B2_REMOTE_MULTIPART).await;
    let _ = std::fs::remove_file(&dst);
    let _ = std::fs::remove_dir_all(&src_dir);

    eprintln!(
        "SG-T16 OK: B2 shaped-graph multipart upload byte-identical, \
         {} parts, sha {}",
        built.transfer.len(),
        src_sha
    );
}
