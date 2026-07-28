#![cfg(all(test, feature = "aerorsync"))]

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::aerorsync::delta_transport_impl::AerorsyncDeltaTransport;
use crate::aerorsync::remote_command::RemoteCommandSpec;
use crate::aerorsync::ssh_transport::{SshHostKeyPolicy, SshTransportConfig};
use crate::aerorsync::transport::RemoteExecRequest;
use crate::delta_transport::DeltaTransport;

fn env_path(name: &str) -> PathBuf {
    PathBuf::from(env::var(name).unwrap_or_else(|_| panic!("missing env var {name}")))
}

/// Host-side root of the real-rsync harness bind mount.
///
/// `docker-compose.real-rsync.yml` maps `capture/workspace` to `/workspace` in
/// the container, so a file written here appears to the remote `rsync
/// --server` at the mirrored path. That is why the tests below write "remote"
/// fixtures with plain `fs::write` instead of copying them over SSH.
///
/// It used to be hardcoded to the in-repo directory. That made the whole lane
/// unrunnable for anyone whose checkout does not own that tree — on a machine
/// where the workspace had been created by a different account, three tests
/// died on `PermissionDenied` inside `write_bytes` before reaching a single
/// wire byte. `RSNP_TEST_REAL_WORKSPACE` lets a runner point both the mount
/// and the tests at a directory it can actually write, and the old path stays
/// the default so existing setups are unaffected.
fn real_workspace() -> PathBuf {
    match env::var("RSNP_TEST_REAL_WORKSPACE") {
        Ok(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/aerorsync/capture/workspace/real"),
    }
}

/// Whether the real-rsync lane is configured, with a loud failure when the
/// caller declared it mandatory.
///
/// Every test in this file opens with an early return when the lane is not
/// set up. That is right for a developer sweeping `cargo test live_tests`
/// across lanes, and wrong for CI: libtest has no "skipped" state, so the
/// early return prints one line to stderr and reports **ok**. Eight tests
/// that never ran are indistinguishable, in a result list, from eight that
/// passed — which is exactly how this lane stayed silently inactive.
///
/// Setting `RSNP_TEST_REAL_REQUIRED=1` turns the missing configuration into a
/// panic, so a job that means to exercise the lane cannot pass by skipping it.
fn real_lane_active() -> bool {
    if env::var("RSNP_TEST_REAL_SSH_KEY").is_ok() {
        return true;
    }
    let required = env::var("RSNP_TEST_REAL_REQUIRED")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    assert!(
        !required,
        "RSNP_TEST_REAL_REQUIRED is set but RSNP_TEST_REAL_SSH_KEY is not: \
         the real-rsync lane was declared mandatory and is not configured. \
         Refusing to report a skipped test as a pass."
    );
    eprintln!("skipping: RSNP_TEST_REAL_SSH_KEY not set (real-rsync lane inactive)");
    false
}

fn write_bytes(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, bytes).unwrap();
}

fn make_incompressible_payload(size: usize, seed: u64) -> Vec<u8> {
    let mut state = seed;
    (0..size)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect()
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
    // env is not set: skip rather than fail so a `live_tests` sweep
    // across lanes works without bespoke filters. `RSNP_TEST_REAL_REQUIRED=1`
    // makes that skip a hard failure for callers that mean to run the lane.
    if !real_lane_active() {
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
    tcp.set_read_timeout(Some(StdDuration::from_secs(5)))
        .unwrap();
    tcp.set_write_timeout(Some(StdDuration::from_secs(5)))
        .unwrap();
    let mut sess = ssh2::Session::new().unwrap();
    sess.set_tcp_stream(tcp);
    sess.handshake()
        .expect("ssh handshake against real-rsync lane");

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
    // on the other side will EPIPE and tear down, which is fine: the tee
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
        "unexpected first greeting byte {:#04x}: expected 0x1f (protocol 31) or 0x20 (32)",
        greeting[0]
    );
}

/// Shared setup for the production wire-path live tests against stock rsync.
/// Returns `(transport, remote, local, expected)`. Each test MUST pass its
/// own local path env var so parallel cargo tests do not race on the same
/// `<target>.aerotmp` / rename.
fn real_rsync_delta_download_inputs(
    local_env: &str,
) -> (AerorsyncDeltaTransport, String, PathBuf, Vec<u8>) {
    let mut ssh = base_config_with_prefix("RSNP_TEST_REAL");
    // Probe stock rsync --version (not aerorsync_serve).
    ssh.probe_request = RemoteExecRequest {
        program: "rsync".to_string(),
        args: vec!["--version".to_string()],
        environment: Vec::new(),
    };
    // 256 KiB fixture is below the production min_file_size; download has no
    // size gate, but keep min at 0 so a future upload twin stays usable.
    let transport = AerorsyncDeltaTransport::new(ssh, 0);
    let remote = env::var("RSNP_TEST_REAL_REMOTE_DOWNLOAD_TARGET")
        .expect("RSNP_TEST_REAL_REMOTE_DOWNLOAD_TARGET must point at the remote file");
    let local = env_path(local_env);
    let expected = fs::read(env_path("RSNP_TEST_REAL_EXPECT_DOWNLOAD_FILE"))
        .expect("read expected download fixture");
    (transport, remote, local, expected)
}

fn real_rsync_delta_upload_inputs() -> (AerorsyncDeltaTransport, String, PathBuf, PathBuf) {
    let mut ssh = base_config_with_prefix("RSNP_TEST_REAL");
    ssh.probe_request = RemoteExecRequest {
        program: "rsync".to_string(),
        args: vec!["--version".to_string()],
        environment: Vec::new(),
    };
    let transport = AerorsyncDeltaTransport::new(ssh, 0);
    let remote = env::var("RSNP_TEST_REAL_REMOTE_UPLOAD_TARGET")
        .expect("RSNP_TEST_REAL_REMOTE_UPLOAD_TARGET must point at the remote file");
    let workspace = real_workspace();
    (
        transport,
        remote,
        workspace.join("local/upload.bin"),
        workspace.join("upload/target.bin"),
    )
}

/// Production wire path against stock rsync (B3-12 / B3-15 xxh128 path).
///
/// Drives
/// [`AerorsyncDeltaTransport`] / `do_download` against a real `rsync --server
/// --sender` peer. That is the path that negotiates xxh128 by default,
/// confirms rolling hits with truncated block-strong (B3-15), and verifies
/// the sender's whole-file trailer before commit (B3-12).
///
/// Env (same `RSNP_TEST_REAL_*` namespace as the greeting test, plus paths):
/// - `RSNP_TEST_REAL_SSH_KEY`, `HOST`, `PORT`, `USER`, optional `HOST_FINGERPRINT`
/// - `RSNP_TEST_REAL_REMOTE_DOWNLOAD_TARGET` (absolute path on the fixture)
/// - `RSNP_TEST_REAL_LOCAL_DOWNLOAD_FILE` (host-side baseline / target)
/// - `RSNP_TEST_REAL_EXPECT_DOWNLOAD_FILE` (expected final bytes)
#[tokio::test]
#[ignore = "requires the Docker real-rsync SSH fixture + AerorsyncDeltaTransport"]
async fn live_real_rsync_native_delta_download_verifies_whole_file() {
    if !real_lane_active() {
        return;
    }

    let (transport, remote, local, expected) =
        real_rsync_delta_download_inputs("RSNP_TEST_REAL_LOCAL_DOWNLOAD_FILE");

    // Baseline is already the older basis on disk (harness seeds it). A
    // successful delta reconstructs `expected` and commits atomically.
    let stats = transport
        .download(&remote, &local)
        .await
        .unwrap_or_else(|e| panic!("native delta download against real rsync failed: {e:?}"));

    let got = fs::read(&local).expect("read reconstructed local target");
    assert_eq!(
        got, expected,
        "reconstructed local bytes must match the remote source"
    );
    assert_eq!(
        stats.total_size,
        expected.len() as u64,
        "stats.total_size must equal reconstructed length"
    );
    eprintln!(
        "live real-rsync native delta download (default/xxh128): total_size={} bytes_sent={} bytes_received={} speedup={:.2} duration_ms={}",
        stats.total_size,
        stats.bytes_sent,
        stats.bytes_received,
        stats.speedup,
        stats.duration_ms
    );
    assert!(
        stats.bytes_received < stats.total_size || stats.total_size < 4096,
        "expected a real delta (bytes_received < total_size) for the 256 KiB near-identical basis; got bytes_received={} total={}",
        stats.bytes_received,
        stats.total_size
    );
}

/// Same production wire path with checksum negotiation forced to md5
/// (`AEROFTP_RSYNC_CSUM_ALGOS=md5`), exercising B3-14 whole-file trailer
/// verify and B3-17 block-strong confirm against a stock rsync 3.2.7 peer.
///
/// Uses `RSNP_TEST_REAL_LOCAL_DOWNLOAD_FILE_MD5` so it can run in parallel
/// with the xxh128 twin without racing on the same `.aerotmp`.
#[tokio::test]
#[ignore = "requires the Docker real-rsync SSH fixture + md5 preamble override"]
async fn live_real_rsync_native_delta_download_md5_peer() {
    if !real_lane_active() {
        return;
    }

    // Force md5 as the only advertised client algo so negotiation cannot
    // prefer xxh128. PreambleProfile::for_host reads this via with_env_overrides.
    // SAFETY: live test process; no other concurrent reader of this var in
    // the md5 peer path. Cleared before return.
    unsafe {
        env::set_var("AEROFTP_RSYNC_CSUM_ALGOS", "md5");
    }

    let (transport, remote, local, expected) =
        real_rsync_delta_download_inputs("RSNP_TEST_REAL_LOCAL_DOWNLOAD_FILE_MD5");

    let result = transport.download(&remote, &local).await;
    unsafe {
        env::remove_var("AEROFTP_RSYNC_CSUM_ALGOS");
    }

    let stats = result.unwrap_or_else(|e| {
        panic!("native delta download (md5 peer) against real rsync failed: {e:?}")
    });
    let got = fs::read(&local).expect("read reconstructed local target");
    assert_eq!(got, expected, "md5-peer reconstruction must match remote");
    eprintln!(
        "live real-rsync native delta download (md5): total_size={} bytes_sent={} bytes_received={} speedup={:.2}",
        stats.total_size, stats.bytes_sent, stats.bytes_received, stats.speedup
    );
    assert!(
        stats.bytes_received < stats.total_size || stats.total_size < 4096,
        "expected a real delta under md5 block-strong; got bytes_received={} total={}",
        stats.bytes_received,
        stats.total_size
    );
}

/// Y-RSC.3: same production wire path with checksum negotiation forced
/// to the legacy md4 (`AEROFTP_RSYNC_CSUM_ALGOS=md4`), proving a delta
/// transfer with a REAL whole-file verify: before Y-RSC.3 the md4
/// branch was a deliberate no-op. A passing run pins live, against the
/// stock peer, both md4 decisions taken from the rsync 3.2.7 sources:
/// the UNSEEDED trailer (a seeded recompute would mismatch and abort)
/// and the data-then-seed per-block order (a wrong order would confirm
/// no rolling hit and zero the delta, tripping the bytes_received
/// assertion).
///
/// Uses `RSNP_TEST_REAL_LOCAL_DOWNLOAD_FILE_MD4` so it can run in
/// parallel with the other twins without racing on the same `.aerotmp`.
#[tokio::test]
#[ignore = "requires the Docker real-rsync SSH fixture + md4 preamble override"]
async fn live_real_rsync_native_delta_download_md4_peer() {
    if !real_lane_active() {
        return;
    }

    // SAFETY: live test process; no other concurrent reader of this var
    // in the md4 peer path. Cleared before return.
    unsafe {
        env::set_var("AEROFTP_RSYNC_CSUM_ALGOS", "md4");
    }

    let (transport, remote, local, expected) =
        real_rsync_delta_download_inputs("RSNP_TEST_REAL_LOCAL_DOWNLOAD_FILE_MD4");

    let result = transport.download(&remote, &local).await;
    unsafe {
        env::remove_var("AEROFTP_RSYNC_CSUM_ALGOS");
    }

    let stats = result.unwrap_or_else(|e| {
        panic!("native delta download (md4 peer) against real rsync failed: {e:?}")
    });
    let got = fs::read(&local).expect("read reconstructed local target");
    assert_eq!(got, expected, "md4-peer reconstruction must match remote");
    eprintln!(
        "live real-rsync native delta download (md4): total_size={} bytes_sent={} bytes_received={} speedup={:.2}",
        stats.total_size, stats.bytes_sent, stats.bytes_received, stats.speedup
    );
    assert!(
        stats.bytes_received < stats.total_size || stats.total_size < 4096,
        "expected a real delta under md4 block-strong; got bytes_received={} total={}",
        stats.bytes_received,
        stats.total_size
    );
}

/// Y-RSC.3: sha1 twin of the md4 test above. sha1 is NOT in our
/// byte-pinned default advertisement; forcing
/// `AEROFTP_RSYNC_CSUM_ALGOS=sha1` makes it the winner against a stock
/// OpenSSL-built rsync (the harness advertises `... md5 md4 sha1 none`).
/// Pins the 20-byte unseeded trailer and the seed-first per-block order
/// of the EVP branch, live.
#[tokio::test]
#[ignore = "requires the Docker real-rsync SSH fixture + sha1 preamble override"]
async fn live_real_rsync_native_delta_download_sha1_peer() {
    if !real_lane_active() {
        return;
    }

    // SAFETY: live test process; no other concurrent reader of this var
    // in the sha1 peer path. Cleared before return.
    unsafe {
        env::set_var("AEROFTP_RSYNC_CSUM_ALGOS", "sha1");
    }

    let (transport, remote, local, expected) =
        real_rsync_delta_download_inputs("RSNP_TEST_REAL_LOCAL_DOWNLOAD_FILE_SHA1");

    let result = transport.download(&remote, &local).await;
    unsafe {
        env::remove_var("AEROFTP_RSYNC_CSUM_ALGOS");
    }

    let stats = result.unwrap_or_else(|e| {
        panic!("native delta download (sha1 peer) against real rsync failed: {e:?}")
    });
    let got = fs::read(&local).expect("read reconstructed local target");
    assert_eq!(got, expected, "sha1-peer reconstruction must match remote");
    eprintln!(
        "live real-rsync native delta download (sha1): total_size={} bytes_sent={} bytes_received={} speedup={:.2}",
        stats.total_size, stats.bytes_sent, stats.bytes_received, stats.speedup
    );
    assert!(
        stats.bytes_received < stats.total_size || stats.total_size < 4096,
        "expected a real delta under sha1 block-strong; got bytes_received={} total={}",
        stats.bytes_received,
        stats.total_size
    );
}

/// CLAUDE-AV-B3-18: production download path against stock rsync 3.2.7
/// with each 8-byte checksum winner forced in turn. Before the fix both
/// cases decoded the complete file-list frame as truncated and timed out
/// waiting for a second frame that the server would never send.
///
/// Run with `--test-threads=1`: the checksum advertisement override is a
/// process-global environment variable.
#[tokio::test]
#[ignore = "requires the Docker real-rsync SSH fixture + xxh64/xxh3 overrides"]
async fn live_real_rsync_native_download_completes_for_xxh64_and_xxh3_peers() {
    if !real_lane_active() {
        return;
    }

    let workspace = real_workspace();
    let remote_bind = workspace.join("download/target.bin");
    let expected_path = env_path("RSNP_TEST_REAL_EXPECT_DOWNLOAD_FILE");
    let baseline = make_incompressible_payload(1024 * 1024, 0xA11C_E55A_55E5_1A11);
    let mut expected_bytes = baseline.clone();
    for byte in &mut expected_bytes[512 * 1024..512 * 1024 + 4096] {
        *byte ^= 0x5A;
    }
    write_bytes(&remote_bind, &expected_bytes);
    write_bytes(&expected_path, &expected_bytes);

    for algorithm in ["xxh64", "xxh3"] {
        let (transport, remote, local, expected) =
            real_rsync_delta_download_inputs("RSNP_TEST_REAL_LOCAL_DOWNLOAD_FILE");
        assert!(
            !expected.is_empty(),
            "the real-rsync fixture must contain a non-empty download target"
        );

        // Each successful download replaces the baseline with the remote
        // bytes. Reintroduce one deterministic difference before every run
        // so both algorithms exercise the transfer path independently.
        fs::write(&local, &baseline).expect("reseed local baseline");

        // SAFETY: ignored live test, documented and invoked with one test
        // thread. Clear the override before asserting on the result.
        unsafe {
            env::set_var("AEROFTP_RSYNC_CSUM_ALGOS", algorithm);
        }
        let result = transport.download(&remote, &local).await;
        unsafe {
            env::remove_var("AEROFTP_RSYNC_CSUM_ALGOS");
        }

        let stats = result.unwrap_or_else(|e| {
            panic!("native download ({algorithm} peer) against real rsync failed: {e:?}")
        });
        let got = fs::read(&local).expect("read reconstructed local target");
        assert_eq!(
            got, expected,
            "{algorithm}-peer reconstruction must match remote"
        );
        assert_eq!(stats.total_size, expected.len() as u64);
        eprintln!(
            "live real-rsync native download ({algorithm}): total_size={} bytes_sent={} bytes_received={} copy_blocks={} speedup={:.2} duration_ms={}",
            stats.total_size,
            stats.bytes_sent,
            stats.bytes_received,
            stats.copy_blocks,
            stats.speedup,
            stats.duration_ms
        );
        assert!(
            stats.copy_blocks > 0,
            "{algorithm} download must decode at least one CopyRun block"
        );
    }
}

/// Production upload path against stock rsync 3.2.7 with each 8-byte
/// checksum winner forced in turn. The baseline is deterministic
/// pseudo-random data with one localised edit, so compressed literals
/// cannot masquerade as delta reuse.
#[tokio::test]
#[ignore = "requires the Docker real-rsync SSH fixture + xxh64/xxh3 overrides"]
async fn live_real_rsync_native_upload_completes_for_xxh64_and_xxh3_peers() {
    if !real_lane_active() {
        return;
    }

    let algorithms = env::var("RSNP_TEST_ONLY_CSUM_ALGO")
        .map(|value| vec![value])
        .unwrap_or_else(|_| vec!["xxh64".to_string(), "xxh3".to_string()]);
    for algorithm in algorithms {
        let (transport, remote, local, remote_bind) = real_rsync_delta_upload_inputs();
        let baseline = make_incompressible_payload(1024 * 1024, 0xC0FF_EE11_2233_4455);
        let mut expected = baseline.clone();
        for byte in &mut expected[512 * 1024..512 * 1024 + 4096] {
            *byte ^= 0xA5;
        }
        write_bytes(&remote_bind, &baseline);
        write_bytes(&local, &expected);

        // SAFETY: ignored live test, documented and invoked with one test
        // thread. Clear the override before asserting on the result.
        unsafe {
            env::set_var("AEROFTP_RSYNC_CSUM_ALGOS", &algorithm);
        }
        let result = transport.upload(&local, &remote).await;
        unsafe {
            env::remove_var("AEROFTP_RSYNC_CSUM_ALGOS");
        }

        let stats = result.unwrap_or_else(|e| {
            panic!("native upload ({algorithm} peer) against real rsync failed: {e:?}")
        });
        let got = fs::read(&remote_bind).expect("read reconstructed remote target");
        assert_eq!(
            got, expected,
            "{algorithm}-peer reconstruction must match local source"
        );
        assert_eq!(stats.total_size, expected.len() as u64);
        eprintln!(
            "live real-rsync native upload ({algorithm}): total_size={} bytes_sent={} bytes_received={} copy_blocks={} speedup={:.2} duration_ms={}",
            stats.total_size,
            stats.bytes_sent,
            stats.bytes_received,
            stats.copy_blocks,
            stats.speedup,
            stats.duration_ms
        );
        assert!(
            stats.copy_blocks > 0,
            "{algorithm} upload must emit at least one CopyRun block"
        );
    }
}

/// Y-RSC.3: upload twin for the two last-resort compatibility winners.
/// Same harness and assertions as the xxh64/xxh3 loop above: remote
/// bytes byte-identical to the local source AND `copy_blocks > 0`. The
/// latter is the live pin of the per-block seeding order (md4
/// data-then-seed via the builtin branch, sha1 seed-first via EVP): a
/// wrong order would leave the stock generator's signatures
/// unconfirmable and zero the delta. The stock receiver also recomputes
/// our whole-file trailer (`receiver.c` compares `file_sum1/2`), so a
/// seeded trailer would fail the transfer outright.
///
/// Run with `--test-threads=1`: the checksum advertisement override is a
/// process-global environment variable.
#[tokio::test]
#[ignore = "requires the Docker real-rsync SSH fixture + md4/sha1 overrides"]
async fn live_real_rsync_native_upload_completes_for_md4_and_sha1_peers() {
    if !real_lane_active() {
        return;
    }

    let algorithms = env::var("RSNP_TEST_ONLY_CSUM_ALGO")
        .map(|value| vec![value])
        .unwrap_or_else(|_| vec!["md4".to_string(), "sha1".to_string()]);
    for algorithm in algorithms {
        let (transport, remote, local, remote_bind) = real_rsync_delta_upload_inputs();
        let baseline = make_incompressible_payload(1024 * 1024, 0x1E9A_C15E_ED00_0001);
        let mut expected = baseline.clone();
        for byte in &mut expected[512 * 1024..512 * 1024 + 4096] {
            *byte ^= 0x3C;
        }
        write_bytes(&remote_bind, &baseline);
        write_bytes(&local, &expected);

        // SAFETY: ignored live test, documented and invoked with one test
        // thread. Clear the override before asserting on the result.
        unsafe {
            env::set_var("AEROFTP_RSYNC_CSUM_ALGOS", &algorithm);
        }
        let result = transport.upload(&local, &remote).await;
        unsafe {
            env::remove_var("AEROFTP_RSYNC_CSUM_ALGOS");
        }

        let stats = result.unwrap_or_else(|e| {
            panic!("native upload ({algorithm} peer) against real rsync failed: {e:?}")
        });
        let got = fs::read(&remote_bind).expect("read reconstructed remote target");
        assert_eq!(
            got, expected,
            "{algorithm}-peer reconstruction must match local source"
        );
        assert_eq!(stats.total_size, expected.len() as u64);
        eprintln!(
            "live real-rsync native upload ({algorithm}): total_size={} bytes_sent={} bytes_received={} copy_blocks={} speedup={:.2} duration_ms={}",
            stats.total_size,
            stats.bytes_sent,
            stats.bytes_received,
            stats.copy_blocks,
            stats.speedup,
            stats.duration_ms
        );
        assert!(
            stats.copy_blocks > 0,
            "{algorithm} upload must emit at least one CopyRun block"
        );
    }
}
