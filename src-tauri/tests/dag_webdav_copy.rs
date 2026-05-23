// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! SG-T17: live integration for the shaped-copy server-side path on
//! WebDAV. Exercises the provider's native RFC 4918 `COPY` method
//! through the `server_side_copy` trait surface that the shaped-copy
//! graph dispatches.
//!
//! Marked `#[ignore]`: requires a WebDAV vault entry under
//! `srv_webdav_dag`. Run:
//! ```bash
//! cargo test --release --test dag_webdav_copy -- --ignored --nocapture
//! ```

#![cfg(unix)]

use std::path::Path;

use ftp_client_gui_lib::credential_store::CredentialStore;
use ftp_client_gui_lib::providers::types::WebDavConfig;
use ftp_client_gui_lib::providers::webdav::WebDavProvider;
use ftp_client_gui_lib::providers::StorageProvider;
use secrecy::SecretString;
use sha2::{Digest, Sha256};

const WEBDAV_PROFILE_ID: &str = "srv_webdav_dag";
const WEBDAV_REMOTE_SOURCE: &str = "_dag_validation/copy_source.bin";
const WEBDAV_REMOTE_DEST: &str = "_dag_validation/copy_dest.bin";
const FILE_BYTES: usize = 2 * 1024 * 1024;

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

/// Load `(url, username, password)` from the vault.
fn load_webdav_creds() -> Option<(String, String, String)> {
    let init_result = CredentialStore::init().ok()?;
    if init_result != "OK" {
        eprintln!("SKIP: vault requires master password ({init_result})");
        return None;
    }
    let store = CredentialStore::from_cache()?;
    let raw = store
        .get(&format!("server_{}", WEBDAV_PROFILE_ID))
        .ok()
        .filter(|s| !s.is_empty())?;
    let val: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let obj = val.as_object()?;
    let url = obj
        .get("extra")
        .and_then(|v| v.as_object())
        .and_then(|o| o.get("url"))
        .and_then(|v| v.as_str())?
        .to_string();
    let username = obj.get("username").and_then(|v| v.as_str())?.to_string();
    let password = obj.get("password").and_then(|v| v.as_str())?.to_string();
    Some((url, username, password))
}

fn webdav_config(url: String, username: String, password: String) -> WebDavConfig {
    WebDavConfig {
        url,
        username,
        password: SecretString::from(password),
        initial_path: None,
        verify_cert: true,
        anonymous: false,
    }
}

#[tokio::test]
#[ignore = "live WAN: SG-T17 WebDAV server-side COPY (vault required)"]
async fn gtc_dag_webdav_server_side_copy() {
    let Some((url, username, password)) = load_webdav_creds() else {
        eprintln!("SKIP: WebDAV credentials unavailable in vault (profile {WEBDAV_PROFILE_ID})");
        return;
    };

    let src_dir = std::env::temp_dir().join("dag-webdav-copy-src");
    let _ = std::fs::remove_dir_all(&src_dir);
    std::fs::create_dir_all(&src_dir).unwrap();
    let local_src = src_dir.join("source.bin");
    let payload = random_buf(0xDADA_C04F, FILE_BYTES);
    std::fs::write(&local_src, &payload).unwrap();
    let src_sha = sha256_bytes(&payload);

    let mut base =
        WebDavProvider::new(webdav_config(url, username, password)).expect("WebDavProvider::new");
    base.connect().await.expect("WebDAV connect");
    let _ = base.delete(WEBDAV_REMOTE_SOURCE).await;
    let _ = base.delete(WEBDAV_REMOTE_DEST).await;
    base.upload(local_src.to_str().unwrap(), WEBDAV_REMOTE_SOURCE, None)
        .await
        .expect("seed WebDAV copy source");

    assert!(
        base.supports_server_side_copy(),
        "WebDAV provider must advertise supports_server_side_copy"
    );

    base.server_side_copy(WEBDAV_REMOTE_SOURCE, WEBDAV_REMOTE_DEST)
        .await
        .expect("WebDAV COPY must succeed");

    let dst_local = std::env::temp_dir().join("dag-webdav-copy-dst.bin");
    let _ = std::fs::remove_file(&dst_local);
    base.download(WEBDAV_REMOTE_DEST, dst_local.to_str().unwrap(), None)
        .await
        .expect("download copied object");
    let dst_sha = sha256_file(&dst_local);
    assert_eq!(
        dst_sha, src_sha,
        "WebDAV copy destination must match the source bytes \
         (expected sha {src_sha}, got {dst_sha})"
    );

    let _ = base.delete(WEBDAV_REMOTE_SOURCE).await;
    let _ = base.delete(WEBDAV_REMOTE_DEST).await;
    let _ = std::fs::remove_file(&dst_local);
    let _ = std::fs::remove_dir_all(&src_dir);

    eprintln!("SG-T17 OK: WebDAV server-side COPY byte-identical, sha {src_sha}");
}
