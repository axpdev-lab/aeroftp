#![cfg(all(test, feature = "proto_native_rsync"))]

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tokio::time::sleep;

use crate::rsync_native_proto::driver::SessionDriver;
use crate::rsync_native_proto::engine_adapter::CurrentDeltaSyncBridge;
use crate::rsync_native_proto::protocol::{FileMetadataMessage, NativeFrameCodec};
use crate::rsync_native_proto::remote_command::RemoteCommandSpec;
use crate::rsync_native_proto::session::NativeRsyncSession;
use crate::rsync_native_proto::ssh_transport::{
    SshHostKeyPolicy, SshRemoteShellTransport, SshTransportConfig,
};
use crate::rsync_native_proto::transport::{
    BidirectionalByteStream, RemoteExecRequest, RemoteShellTransport,
};
use crate::rsync_native_proto::types::{NativeRsyncConfig, NativeRsyncErrorKind};

fn env_path(name: &str) -> PathBuf {
    PathBuf::from(env::var(name).unwrap_or_else(|_| panic!("missing env var {name}")))
}

fn write_bytes(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, bytes).unwrap();
}

fn make_payload(size: usize) -> Vec<u8> {
    (0..size).map(|index| (index % 251) as u8).collect()
}

fn mutate_payload(basis: &[u8], offset: usize, patch: &[u8]) -> Vec<u8> {
    let mut out = basis.to_vec();
    let end = offset + patch.len();
    out[offset..end].copy_from_slice(patch);
    out
}

fn base_config_with_prefix(prefix: &str) -> SshTransportConfig {
    let var = |name: &str| format!("{prefix}_{name}");
    let key_path = env_path(&var("SSH_KEY"));
    let max_frame_size = env::var(var("MAX_FRAME_SIZE"))
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(32 * 1024 * 1024);
    let host = env::var(var("HOST")).unwrap_or_else(|_| "127.0.0.1".to_string());
    let port = env::var(var("PORT"))
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(2222);
    let username = env::var(var("USER")).unwrap_or_else(|_| "testuser".to_string());
    let mut config = SshTransportConfig::localhost_test(key_path, max_frame_size);
    config.host = host;
    config.port = port;
    config.username = username;
    if let Ok(fingerprint) = env::var(var("HOST_FINGERPRINT")) {
        if !fingerprint.is_empty() {
            config.host_key_policy = SshHostKeyPolicy::pinned_hex(fingerprint);
        }
    }
    config
}

fn base_config() -> SshTransportConfig {
    base_config_with_prefix("RSNP_TEST")
}

fn ssh_transport() -> SshRemoteShellTransport {
    SshRemoteShellTransport::new(base_config())
}

fn file_metadata_for(path: &Path, bytes: &[u8]) -> FileMetadataMessage {
    FileMetadataMessage {
        path: path.display().to_string(),
        size: bytes.len() as u64,
        mode: 0o644,
        modified_unix_secs: 0,
    }
}

#[tokio::test]
#[ignore = "requires the Docker RSNP SSH fixture"]
async fn live_probe_reports_protocol_31() {
    let transport = ssh_transport();
    let probe = transport.probe().await.unwrap();
    assert_eq!(probe.protocol.as_u32(), 31);
    assert!(probe.remote_banner.contains("rsnp-proto server protocol 31"));
}

#[tokio::test]
#[ignore = "requires the Docker RSNP SSH fixture"]
async fn live_upload_applies_delta_to_remote_target() {
    let local_file = env_path("RSNP_TEST_LOCAL_UPLOAD_FILE");
    let remote_target = env::var("RSNP_TEST_REMOTE_UPLOAD_TARGET").unwrap();

    let basis = make_payload(256 * 1024);
    let updated = mutate_payload(&basis, 8 * 1024, b"native-live-upload");
    write_bytes(&local_file, &updated);

    let transport = ssh_transport();
    let codec = NativeFrameCodec::new(NativeRsyncConfig::default().max_frame_size);
    let session = NativeRsyncSession::new(transport, NativeRsyncConfig::default());
    let mut driver = SessionDriver::new(session, codec);
    let outcome = driver
        .drive_upload_with_engine(
            RemoteCommandSpec::native_upload(remote_target),
            file_metadata_for(&local_file, &updated),
            updated.clone(),
            &CurrentDeltaSyncBridge,
        )
        .await
        .unwrap();

    assert!(outcome.stats.literal_bytes > 0);
    let remote_bytes = fs::read(env_path("RSNP_TEST_EXPECT_UPLOAD_FILE")).unwrap();
    assert_eq!(remote_bytes, updated);
}

#[tokio::test]
#[ignore = "requires the Docker RSNP SSH fixture"]
async fn live_download_applies_delta_to_local_target() {
    let local_target = env_path("RSNP_TEST_LOCAL_DOWNLOAD_FILE");
    let remote_target = env::var("RSNP_TEST_REMOTE_DOWNLOAD_TARGET").unwrap();

    let basis = make_payload(256 * 1024);
    let updated = mutate_payload(&basis, 8 * 1024, b"native-live-download");
    write_bytes(&local_target, &basis);
    write_bytes(&env_path("RSNP_TEST_EXPECT_DOWNLOAD_FILE"), &updated);

    let transport = ssh_transport();
    let codec = NativeFrameCodec::new(NativeRsyncConfig::default().max_frame_size);
    let session = NativeRsyncSession::new(transport, NativeRsyncConfig::default());
    let mut driver = SessionDriver::new(session, codec);
    let outcome = driver
        .drive_download_with_engine(
            RemoteCommandSpec::native_download(remote_target),
            basis.clone(),
            &CurrentDeltaSyncBridge,
        )
        .await
        .unwrap();

    assert!(outcome.stats.literal_bytes > 0);
    let local_bytes = outcome.reconstructed.unwrap();
    let expected = fs::read(env_path("RSNP_TEST_EXPECT_DOWNLOAD_FILE")).unwrap();
    assert_eq!(local_bytes, expected);
}

#[tokio::test]
#[ignore = "requires the Docker RSNP SSH fixture"]
async fn live_host_key_pin_mismatch_rejects_session() {
    // Supply an obviously-wrong fingerprint so that `connect_and_auth`
    // must reject the session after the handshake but before authentication.
    // This exercises the `PinnedFingerprintSha256` arm of `enforce_host_key_policy`.
    let mut config = base_config();
    config.host_key_policy = SshHostKeyPolicy::pinned_hex(
        "0000000000000000000000000000000000000000000000000000000000000000",
    );
    // Small connect timeout so the test never hangs on a wrong host.
    config.connect_timeout_ms = 5_000;
    let transport = SshRemoteShellTransport::new(config);
    let err = transport.probe().await.unwrap_err();
    assert_eq!(
        err.kind,
        NativeRsyncErrorKind::HostKeyRejected,
        "expected HostKeyRejected, got {:?}: {}",
        err.kind,
        err.detail
    );
    assert!(err.detail.contains("fingerprint mismatch"));
}

#[tokio::test]
#[ignore = "requires the Docker RSNP SSH fixture"]
async fn live_cancel_during_read_unblocks_quickly() {
    // The `rsync_proto_serve` upload stream reads its first frame from the
    // client before emitting anything. If we open the stream and try to
    // read immediately, we block inside libssh2 until either the server
    // speaks (which it never will without our Hello) or the I/O times out.
    // Cancelling via `transport.cancel()` must unblock the read in well
    // under the configured `io_timeout_ms`.
    let mut config = base_config();
    // 10s is the default; keep it — the whole point is that cancel wins.
    config.io_timeout_ms = 10_000;
    let transport = SshRemoteShellTransport::new(config);
    let stream_request = RemoteExecRequest {
        program: "/opt/rsnp/bin/rsync_proto_serve".to_string(),
        args: vec![
            "--mode".to_string(),
            "upload".to_string(),
            "--target".to_string(),
            env::var("RSNP_TEST_REMOTE_UPLOAD_TARGET").unwrap(),
        ],
        environment: Vec::new(),
    };
    let mut stream = transport
        .open_stream(stream_request)
        .await
        .expect("open_stream against rsync_proto_serve");

    let handle = transport.cancel_handle();
    let cancel_task = tokio::spawn(async move {
        sleep(Duration::from_millis(200)).await;
        handle.cancel();
    });

    let started = Instant::now();
    let err = stream
        .read_frame()
        .await
        .expect_err("read_frame must fail once cancel fires");
    let elapsed = started.elapsed();

    cancel_task.await.unwrap();

    assert!(
        elapsed < Duration::from_secs(3),
        "cancel did not unblock the read in time: {elapsed:?}"
    );
    assert!(
        matches!(
            err.kind,
            NativeRsyncErrorKind::Cancelled | NativeRsyncErrorKind::TransportFailure
        ),
        "expected Cancelled or TransportFailure, got {:?}: {}",
        err.kind,
        err.detail
    );
}

/// S8a byte-oracle lane. The real rsync server is invoked via sshd's
/// `ForceCommand` tee wrapper, so every byte it emits is captured under
/// `/workspace/real_capture/<ts>/capture_out.bin` and available to later
/// sinergie as a parity oracle.
///
/// This test does not parse the real rsync wire. It proves:
///   1. The real-rsync lane is reachable on the configured port.
///   2. `RemoteCommandFlavor::WrapperParity::upload(..)` produces an
///      invocation the real server accepts (it does not exit with an
///      argument-parse error before emitting anything).
///   3. The server emits a non-empty greeting payload, consistent with
///      rsync protocol 31's initial version exchange.
///
/// Once S8b lands the multiplex demux, the captured bytes from this lane
/// become the fixture the demux is validated against.
#[tokio::test]
#[ignore = "requires the Docker real-rsync SSH fixture"]
async fn live_real_rsync_lane_emits_protocol_31_greeting() {
    use std::io::Read;
    use std::net::TcpStream as StdTcpStream;
    use std::time::Duration as StdDuration;

    // Conditional skip: this test shares the `cargo test live_tests`
    // selector with the native-lane tests (which use RSNP_TEST_*
    // env vars). When only the native harness is running, the real-rsync
    // env is not set — skip rather than fail so a `live_tests` sweep
    // across lanes works without bespoke filters.
    if env::var("RSNP_TEST_REAL_SSH_KEY").is_err() {
        eprintln!("skipping: RSNP_TEST_REAL_SSH_KEY not set (real-rsync lane inactive)");
        return;
    }

    let config = base_config_with_prefix("RSNP_TEST_REAL");
    let remote_target = env::var("RSNP_TEST_REAL_REMOTE_UPLOAD_TARGET")
        .expect("RSNP_TEST_REAL_REMOTE_UPLOAD_TARGET must point at the remote target path");

    // Bypass our RSNP codec entirely: we are the client here and we are not
    // supposed to know how to talk real rsync yet. Open a raw exec channel
    // and read whatever the server puts on stdout within a small window.
    let tcp = StdTcpStream::connect((config.host.as_str(), config.port))
        .expect("tcp connect to real-rsync lane");
    tcp.set_read_timeout(Some(StdDuration::from_secs(5))).unwrap();
    tcp.set_write_timeout(Some(StdDuration::from_secs(5))).unwrap();
    let mut sess = ssh2::Session::new().unwrap();
    sess.set_tcp_stream(tcp);
    sess.handshake().expect("ssh handshake against real-rsync lane");

    // Verify the Ed25519 fingerprint if one was pinned, so we fail fast if
    // the harness did not extract it for some reason.
    if let SshHostKeyPolicy::PinnedFingerprintSha256 { sha256_hex } = &config.host_key_policy {
        use sha2::{Digest, Sha256};
        let (host_key, _) = sess
            .host_key()
            .expect("remote host key available after handshake");
        let digest = Sha256::digest(host_key);
        let actual = digest
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        assert_eq!(
            actual,
            sha256_hex.to_lowercase(),
            "pinned fingerprint does not match real-rsync lane's Ed25519 host key"
        );
    }

    sess.userauth_pubkey_file(&config.username, None, &config.private_key_path, None)
        .expect("pubkey auth against real-rsync lane");
    assert!(sess.authenticated());

    let command = RemoteCommandSpec::upload(remote_target).to_command_line();
    let mut channel = sess.channel_session().unwrap();
    channel.exec(&command).expect("exec real rsync --server");

    let mut greeting = Vec::with_capacity(64);
    let mut tmp = [0u8; 64];
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline && greeting.is_empty() {
        match channel.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => {
                greeting.extend_from_slice(&tmp[..n]);
                break;
            }
            Err(err) => {
                // libssh2 WOULD_BLOCK is the common case here; loop until
                // either the deadline fires or the server speaks.
                if err.raw_os_error() == Some(11) || err.kind() == std::io::ErrorKind::WouldBlock {
                    std::thread::sleep(Duration::from_millis(25));
                    continue;
                }
                panic!("unexpected read error from real rsync greeting: {err}");
            }
        }
    }

    // We deliberately close the channel without responding: the rsync server
    // on the other side will EPIPE and tear down, which is fine — the tee
    // wrapper has already persisted the greeting bytes under
    // /workspace/real_capture/<ts>/capture_out.bin for S8b.
    let _ = channel.close();
    let _ = channel.wait_close();

    assert!(
        !greeting.is_empty(),
        "real rsync server produced no greeting bytes within the deadline"
    );
    // rsync protocol 31 opens the exec channel with a 4-byte LE protocol
    // version (0x1f, 0x00, 0x00, 0x00). We do not assert the exact shape
    // yet (sub-protocol negotiation can add extra preamble bytes on some
    // builds), but the first byte should be the low byte of the version.
    assert!(
        greeting[0] == 0x1f || greeting[0] == 0x20,
        "unexpected first greeting byte {:#04x} — expected 0x1f (protocol 31) or 0x20 (32)",
        greeting[0]
    );
}
