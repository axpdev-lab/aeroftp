//! The fixed floor every FTP transfer used to pay, tested where it was paid.
//!
//! Two defects, both fixed costs on a path where the transfer of 1 MiB is
//! measured in milliseconds (the DAG engine review put the transfer at
//! 0.012 s/MiB on the lab link and the floor at 2.35 s):
//!
//! 1. `cmd_put` slept 500 ms after EVERY successful upload, for every
//!    provider, because russh buffers SFTP writes. FTP uploads already block
//!    on the server's 226, so for FTP the sleep was dead time.
//! 2. Seven per-transfer sites re-issued `TYPE I` on sessions that
//!    `connect()` had already put in binary mode: one dead control round trip
//!    per file, upload AND download (on a folder of N files, N round trips).
//!
//! Neither needs the Docker fixture: the server below is a scripted fake on
//! loopback, enough FTP for connect + PASV + STOR + RETR + MFMT + QUIT, and
//! it records every command, with the moment it arrived, so the wire itself
//! is asserted on, not a constant.

use std::collections::HashMap;
use std::io::Write as _;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ftp_client_gui_lib::providers::types::{FtpConfig, FtpTlsMode};
use ftp_client_gui_lib::providers::{FtpProvider, StorageProvider};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

/// One control command as the fake server received it.
#[derive(Clone, Debug)]
struct Received {
    line: String,
    at: Instant,
}

impl Received {
    fn verb(&self) -> String {
        self.line
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_uppercase()
    }
}

/// What the fake server saw (and holds), asserted on by the tests.
#[derive(Clone, Default)]
struct Wire {
    /// Every control command, with the moment it arrived.
    commands: Arc<Mutex<Vec<Received>>>,
    stored: Arc<Mutex<Vec<u8>>>,
    files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
}

/// Accept control connections until the shutdown channel fires; each one runs
/// a minimal FTP session that records commands and stores STOR payloads.
async fn serve(mut stop: tokio::sync::watch::Receiver<bool>, listener: TcpListener, wire: Wire) {
    loop {
        let accepted = tokio::select! {
            _ = stop.changed() => return,
            a = listener.accept() => a,
        };
        let (stream, _) = match accepted {
            Ok(v) => v,
            Err(_) => return,
        };
        let wire = wire.clone();
        tokio::spawn(async move { session(stream, wire).await });
    }
}

async fn session(stream: tokio::net::TcpStream, wire: Wire) {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    if write.write_all(b"220 FakeFTP ready\r\n").await.is_err() {
        return;
    }
    // A PASV listener lives until the next data command consumes it.
    let mut pasv: Option<TcpListener> = None;
    while let Ok(Some(line)) = lines.next_line().await {
        wire.commands.lock().unwrap().push(Received {
            line: line.clone(),
            at: Instant::now(),
        });
        let verb = line.split_whitespace().next().unwrap_or("").to_uppercase();
        let arg = line
            .split_once(' ')
            .map(|(_, a)| a)
            .unwrap_or("")
            .to_string();
        match verb.as_str() {
            "USER" => reply(&mut write, b"331 Password required\r\n").await,
            "PASS" => reply(&mut write, b"230 Logged in\r\n").await,
            "TYPE" => reply(&mut write, b"200 Type set\r\n").await,
            "FEAT" => {
                reply(
                    &mut write,
                    b"211-Features:\r\n MFMT\r\n MLST\r\n211 End\r\n",
                )
                .await
            }
            "PWD" => reply(&mut write, b"257 \"/\"\r\n").await,
            "SIZE" => {
                let len = wire
                    .files
                    .lock()
                    .unwrap()
                    .get(&arg)
                    .map(|v| v.len())
                    .unwrap_or(0);
                reply(&mut write, format!("213 {}\r\n", len).as_bytes()).await;
            }
            "PASV" => {
                let listener = match TcpListener::bind("127.0.0.1:0").await {
                    Ok(l) => l,
                    Err(_) => {
                        reply(&mut write, b"425 Cannot listen\r\n").await;
                        continue;
                    }
                };
                let port = listener.local_addr().unwrap().port();
                pasv = Some(listener);
                let msg = format!(
                    "227 Entering Passive Mode (127,0,0,1,{},{})\r\n",
                    port / 256,
                    port % 256
                );
                reply(&mut write, msg.as_bytes()).await;
            }
            "STOR" => {
                reply(&mut write, b"150 Ok to send data\r\n").await;
                let Some(listener) = pasv.take() else {
                    reply(&mut write, b"425 No PASV\r\n").await;
                    continue;
                };
                // The client ends the transfer with FIN after its last byte
                // and waits for us to close before it reads the 226 (#769):
                // read to EOF, drop the socket, then confirm on control.
                let accepted =
                    tokio::time::timeout(Duration::from_secs(5), listener.accept()).await;
                let Ok(Ok((mut data, _))) = accepted else {
                    reply(&mut write, b"425 No data connection\r\n").await;
                    continue;
                };
                let mut buf = Vec::new();
                let mut chunk = [0u8; 65536];
                loop {
                    match data.read(&mut chunk).await {
                        Ok(0) => break,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        Err(_) => break,
                    }
                }
                drop(data);
                wire.stored.lock().unwrap().extend_from_slice(&buf);
                wire.files.lock().unwrap().insert(arg, buf);
                reply(&mut write, b"226 Transfer complete\r\n").await;
            }
            "RETR" => {
                let payload = wire.files.lock().unwrap().get(&arg).cloned();
                let Some(payload) = payload else {
                    reply(&mut write, b"550 Not found\r\n").await;
                    continue;
                };
                reply(&mut write, b"150 Sending data\r\n").await;
                let Some(listener) = pasv.take() else {
                    reply(&mut write, b"425 No PASV\r\n").await;
                    continue;
                };
                let accepted =
                    tokio::time::timeout(Duration::from_secs(5), listener.accept()).await;
                let Ok(Ok((mut data, _))) = accepted else {
                    reply(&mut write, b"425 No data connection\r\n").await;
                    continue;
                };
                // Send, then close: the EOF is the client's end-of-data.
                let _ = data.write_all(&payload).await;
                drop(data);
                reply(&mut write, b"226 Transfer complete\r\n").await;
            }
            "MFMT" => reply(&mut write, b"213 Modify\r\n").await,
            "QUIT" => {
                reply(&mut write, b"221 Goodbye\r\n").await;
                return;
            }
            _ => reply(&mut write, b"502 Not implemented\r\n").await,
        }
    }
}

async fn reply(write: &mut tokio::net::tcp::OwnedWriteHalf, bytes: &[u8]) {
    let _ = write.write_all(bytes).await;
}

struct FakeFtp {
    port: u16,
    wire: Wire,
    stop: tokio::sync::watch::Sender<bool>,
}

async fn start_fake_ftp() -> FakeFtp {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let wire = Wire::default();
    let (stop, rx) = tokio::sync::watch::channel(false);
    tokio::spawn(serve(rx, listener, wire.clone()));
    FakeFtp { port, wire, stop }
}

impl Drop for FakeFtp {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
    }
}

/// connect() sets the session binary once; seven per-transfer sites (upload,
/// download, both resume paths, in-memory read, range read, pooled range
/// download) used to say it again on every file. Against a recording server
/// the whole session's command list is visible, so the assertion is on the
/// wire: one upload AND one download through one session, one TYPE total,
/// and it is TYPE I. On the lab link each dead TYPE is a 47 ms round trip
/// per file, which on a folder is N times that, not once.
#[tokio::test]
async fn transfer_session_sends_type_exactly_once() {
    let server = start_fake_ftp().await;
    let config = FtpConfig {
        host: "127.0.0.1".to_string(),
        port: server.port,
        username: "testuser".to_string(),
        password: secrecy::SecretString::from("testpass".to_string()),
        tls_mode: FtpTlsMode::None,
        verify_cert: false,
        initial_path: Some("/".to_string()),
    };
    let mut provider = FtpProvider::new(config);
    provider.connect().await.expect("connect to fake FTP");

    let payload: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
    let up_local = std::env::temp_dir().join(format!("kimi-type-up-{}.bin", std::process::id()));
    std::fs::File::create(&up_local)
        .unwrap()
        .write_all(&payload)
        .unwrap();
    provider
        .upload(up_local.to_str().unwrap(), "/type-once.bin", None)
        .await
        .expect("upload to fake FTP");

    let down_local =
        std::env::temp_dir().join(format!("kimi-type-down-{}.bin", std::process::id()));
    provider
        .download("/type-once.bin", down_local.to_str().unwrap(), None)
        .await
        .expect("download from fake FTP");
    provider.disconnect().await.ok();

    assert_eq!(std::fs::read(&down_local).unwrap(), payload);
    let commands = server.wire.commands.lock().unwrap();
    let lines: Vec<&str> = commands.iter().map(|c| c.line.as_str()).collect();
    let types: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|c| c.to_uppercase().starts_with("TYPE"))
        .collect();
    assert_eq!(
        types.len(),
        1,
        "the session must pay TYPE once (at connect), not once per transfer: {:?}",
        lines
    );
    assert_eq!(types[0].to_uppercase(), "TYPE I");
    std::fs::remove_file(&up_local).ok();
    std::fs::remove_file(&down_local).ok();
}

/// The commands the fake server has received once it has seen QUIT, or when
/// five seconds have passed without it.
async fn commands_through_quit(wire: &Wire) -> Vec<Received> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let commands = wire.commands.lock().unwrap().clone();
        if commands.iter().any(|c| c.verb() == "QUIT") || Instant::now() >= deadline {
            return commands;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// The 500 ms post-upload settle exists for russh's buffered SFTP writes; it
/// used to run for every provider. This drives the COMMAND itself
/// (`aeroftp-cli put` against a URL, no profile, no vault) and measures the
/// interval where that sleep sits: `cmd_put` settles after the upload and
/// before it disconnects, so the fake server times the gap between the last
/// command it receives before QUIT (MFMT, or the STOR itself when there is no
/// MFMT) and QUIT. On loopback that gap is a few milliseconds; with the
/// settle on the FTP path it is at least 500 ms. 300 ms tells them apart.
///
/// Neither the process wall clock nor the CLI's own number can carry this.
/// The wall clock includes starting a debug binary, which on a loaded CI
/// runner can take longer than any threshold below the 500 ms signal.
/// `cmd_put` closes its `elapsed_secs` before the settle sleep, so that
/// number is the same with and without the sleep.
#[tokio::test]
async fn cli_put_to_ftp_has_no_half_second_settle() {
    let server = start_fake_ftp().await;
    let payload: Vec<u8> = (0..64_000u32).map(|i| (i % 251) as u8).collect();
    let local = std::env::temp_dir().join(format!("kimi-put-floor-{}.bin", std::process::id()));
    std::fs::File::create(&local)
        .unwrap()
        .write_all(&payload)
        .unwrap();

    let url = format!("ftp://testuser:testpass@127.0.0.1:{}/", server.port);
    // tokio::process, not std: the fake server lives on this same runtime, so
    // a blocking wait would starve the very tasks the child is talking to.
    let child = tokio::process::Command::new(env!("CARGO_BIN_EXE_aeroftp-cli"))
        .args([
            "--quiet",
            "put",
            &url,
            local.to_str().unwrap(),
            "/floor.bin",
        ])
        .output();
    let output = match tokio::time::timeout(Duration::from_secs(30), child).await {
        Ok(o) => o.expect("spawn aeroftp-cli put"),
        Err(_) => panic!("aeroftp-cli put to a loopback server did not finish in 30 s"),
    };

    assert!(
        output.status.success(),
        "put failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // The child can exit before the server task has read its QUIT.
    let commands = commands_through_quit(&server.wire).await;
    let lines: Vec<&str> = commands.iter().map(|c| c.line.as_str()).collect();
    assert_eq!(server.wire.stored.lock().unwrap().as_slice(), payload);
    let quit = commands
        .iter()
        .position(|c| c.verb() == "QUIT")
        .unwrap_or_else(|| panic!("the put never sent QUIT: {:?}", lines));
    assert!(
        commands[..quit].iter().any(|c| c.verb() == "STOR"),
        "QUIT has to follow the upload: {:?}",
        lines
    );
    let before_quit = &commands[quit - 1];
    let gap = commands[quit].at.duration_since(before_quit.at);
    assert!(
        gap < Duration::from_millis(300),
        "{:?} passed between `{}` and QUIT; the SFTP-only settle sleep is back on the FTP path",
        gap,
        before_quit.line
    );
    std::fs::remove_file(&local).ok();
}
