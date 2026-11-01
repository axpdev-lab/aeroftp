//! More SCP workers than one SSH connection may run at once.
//!
//! SCP workers share their connection, and each command they run is one
//! session channel on it. OpenSSH refuses a channel past `MaxSessions` (10 by
//! default): measured on OpenSSH 9.6 with 16 workers reading at once and no
//! limit on our side, 6 of the 16 were refused with "Failed to open channel".
//! The provider keeps at most 8 commands running per connection, so the rest
//! wait for a place. This test is that measurement with the limit in place:
//! every worker must read its bytes whole. The in-process fake server cannot
//! stand in for it, because it never runs more than 4 commands at once.
//!
//! It uses the server of `live_sftp_replace.rs` over SCP (an OpenSSH server
//! with SFTP serves SCP too) and the same variables:
//!
//! ```bash
//! HOME=$(mktemp -d) AEROFTP_LIVE_SFTP_HOST=127.0.0.1 AEROFTP_LIVE_SFTP_PORT=2222 \
//! AEROFTP_LIVE_SFTP_USER=testuser AEROFTP_LIVE_SFTP_KEY=tests/fixtures/sftp-rsync/ssh_key \
//! AEROFTP_LIVE_SFTP_DIR=/home/testuser \
//!   cargo test --test live_scp_channels -- --ignored --nocapture
//! ```
//!
//! A scratch `HOME` keeps the fixture's ever-new host key out of the real
//! `known_hosts`, as `live_sftp_replace.rs` explains; a host that is not
//! loopback must already be known, or `AEROFTP_LIVE_SFTP_TRUST_UNKNOWN_HOST=1`.

use ftp_client_gui_lib::providers::scp::provider::ScpProvider;
use ftp_client_gui_lib::providers::types::SftpConfig;
use ftp_client_gui_lib::providers::StorageProvider;

fn env_or(key: &str, fallback: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| fallback.to_string())
}

fn trust_unknown_host(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "::1" | "localhost")
        || matches!(
            std::env::var("AEROFTP_LIVE_SFTP_TRUST_UNKNOWN_HOST").as_deref(),
            Ok("1") | Ok("true")
        )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a real OpenSSH server; see the module comment"]
async fn more_workers_than_the_connection_allows_all_read_their_bytes() {
    let key = std::env::var("AEROFTP_LIVE_SFTP_KEY")
        .ok()
        .filter(|k| !k.is_empty());
    let password = std::env::var("AEROFTP_LIVE_SFTP_PASS")
        .ok()
        .filter(|p| !p.is_empty())
        .map(secrecy::SecretString::from);
    assert!(
        key.is_some() || password.is_some(),
        "set AEROFTP_LIVE_SFTP_KEY or AEROFTP_LIVE_SFTP_PASS"
    );
    let host = env_or("AEROFTP_LIVE_SFTP_HOST", "127.0.0.1");
    let dir = env_or("AEROFTP_LIVE_SFTP_DIR", "/tmp");
    let mut p = ScpProvider::new(SftpConfig {
        host: host.clone(),
        port: env_or("AEROFTP_LIVE_SFTP_PORT", "22")
            .parse()
            .expect("AEROFTP_LIVE_SFTP_PORT"),
        username: env_or("AEROFTP_LIVE_SFTP_USER", "root"),
        password,
        private_key_path: key,
        key_passphrase: None,
        initial_path: Some(dir.clone()),
        timeout_secs: 30,
        trust_unknown_hosts: trust_unknown_host(&host),
    });
    p.connect()
        .await
        .unwrap_or_else(|e| panic!("the live server must be reachable over SCP: {e}"));

    let body: Vec<u8> = (0..4 * 1024 * 1024)
        .map(|i| (i * 7 + i / 251) as u8)
        .collect();
    let local = std::env::temp_dir().join(format!("aeroftp-live-scp-{}", uuid::Uuid::new_v4()));
    std::fs::write(&local, &body).unwrap();
    // Per run, not per process: two machines or containers can share a PID
    // and the remote folder.
    let remote = format!(
        "{dir}/aeroftp-live-scp-channels-{}.bin",
        uuid::Uuid::new_v4()
    );
    p.upload(&local.to_string_lossy(), &remote, None)
        .await
        .expect("upload");
    let _ = std::fs::remove_file(&local);

    // Paced, so every read keeps its channel open while the others start.
    p.set_speed_limit(0, 2048).await.unwrap();
    let mut workers: Vec<_> = (0..16)
        .map(|_| p.clone_for_transfer().expect("worker"))
        .collect();
    p.set_speed_limit(0, 0).await.unwrap();
    let reads = workers
        .iter_mut()
        .map(|w| w.read_range(&remote, 0, body.len() as u64));
    let results = futures_util::future::join_all(reads).await;
    drop(workers);
    p.delete(&remote).await.expect("remove the test file");

    let failed: Vec<String> = results
        .into_iter()
        .filter_map(|r| match r {
            Ok(bytes) if bytes == body => None,
            Ok(bytes) => Some(format!("{} bytes instead of {}", bytes.len(), body.len())),
            Err(e) => Some(e.to_string()),
        })
        .collect();
    assert!(
        failed.is_empty(),
        "{} of 16 workers failed: {failed:?}",
        failed.len()
    );
}
