//! The dead `TYPE I` every FTP transfer used to pay, tested on the wire.
//!
//! Seven per-transfer sites re-issued `TYPE I` on sessions that `connect()`
//! had already put in binary mode: one dead control round trip per file,
//! upload AND download (on a folder of N files, N round trips; on the lab
//! link 47 ms each). The transfer of 1 MiB is 0.012 s on that link, so the
//! round trip dwarfs the payload it precedes.
//!
//! No Docker fixture needed: the server below is a scripted fake on
//! loopback, enough FTP for connect + PASV + STOR + RETR + MFMT + QUIT, and
//! it records every command so the wire itself is asserted on, not a
//! constant.

use std::collections::HashMap;
use std::io::Write as _;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ftp_client_gui_lib::providers::types::{FtpConfig, FtpTlsMode};
use ftp_client_gui_lib::providers::{FtpProvider, StorageProvider};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

/// What the fake server saw (and holds), asserted on by the tests.
#[derive(Clone, Default)]
struct Wire {
    commands: Arc<Mutex<Vec<String>>>,
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
        wire.commands.lock().unwrap().push(line.clone());
        let verb = line.split_whitespace().next().unwrap_or("").to_uppercase();
        let arg = line.split_once(' ').map(|(_, a)| a).unwrap_or("").to_string();
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
    let types: Vec<&String> = commands
        .iter()
        .filter(|c| c.to_uppercase().starts_with("TYPE"))
        .collect();
    assert_eq!(
        types.len(),
        1,
        "the session must pay TYPE once (at connect), not once per transfer: {:?}",
        commands.as_slice()
    );
    assert_eq!(types[0].to_uppercase(), "TYPE I");
    std::fs::remove_file(&up_local).ok();
    std::fs::remove_file(&down_local).ok();
}
