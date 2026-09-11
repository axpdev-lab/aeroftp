//! The CR/LF command-injection refusal, pinned end to end.
//!
//! A remote path containing CR or LF must never reach the control connection:
//! written verbatim it would smuggle a second command into the session. The
//! library (suppaftp) refuses to write such a line, and the provider types the
//! refusal as `InvalidPath` (decided on the io kind, not on the message text,
//! so the classification survives the library rephrasing its error).
//!
//! Until now the refusal was covered only by a manual reproduction. The
//! scripted server below records every line it receives, so the test asserts
//! on the wire itself: the injected verb is not there.

use std::io::Write as _;
use std::sync::{Arc, Mutex};

use ftp_client_gui_lib::providers::types::{FtpConfig, FtpTlsMode, ProviderError};
use ftp_client_gui_lib::providers::{FtpProvider, StorageProvider};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

/// Accept control connections until the listener is dropped: upload re-dials
/// a fresh control session from the retained connection spec, so the fake
/// must serve more than one. Each connection gets just enough FTP for
/// connect(): greeting, USER/PASS login, and a generic 200 for anything else.
/// Every received line is recorded verbatim for the assertion.
async fn serve(listener: TcpListener, commands: Arc<Mutex<Vec<String>>>) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let recorded = Arc::clone(&commands);
        tokio::spawn(async move { session(stream, recorded).await });
    }
}

async fn session(stream: tokio::net::TcpStream, commands: Arc<Mutex<Vec<String>>>) {
    let (read, mut write) = stream.into_split();
    if write.write_all(b"220 FakeFTP ready\r\n").await.is_err() {
        return;
    }
    let mut lines = BufReader::new(read).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        commands.lock().unwrap().push(line.clone());
        let verb = line.split_whitespace().next().unwrap_or("").to_uppercase();
        // PASV needs a real 227 with a listen address: the upload path asks
        // for the data channel before STOR, and the test asserts the STOR
        // with a CR/LF name is never written.
        if verb == "PASV" {
            let Ok(data) = TcpListener::bind("127.0.0.1:0").await else {
                if write.write_all(b"425 Cannot listen\r\n").await.is_err() {
                    return;
                }
                continue;
            };
            let port = data.local_addr().unwrap().port();
            let msg = format!(
                "227 Entering Passive Mode (127,0,0,1,{},{})\r\n",
                port / 256,
                port % 256
            );
            if write.write_all(msg.as_bytes()).await.is_err() {
                return;
            }
            continue;
        }
        let response = match verb.as_str() {
            "USER" => "331 Password required\r\n",
            "PASS" => "230 Logged in\r\n",
            "FEAT" => "211-Features:\r\n MFMT\r\n211 End\r\n",
            "PWD" => "257 \"/\"\r\n",
            "QUIT" => "221 Goodbye\r\n",
            _ => "200 Ok\r\n",
        };
        if write.write_all(response.as_bytes()).await.is_err() {
            return;
        }
        if verb == "QUIT" {
            return;
        }
    }
}

async fn connected_provider() -> (FtpProvider, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let commands = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&commands);
    tokio::spawn(serve(listener, commands));
    let config = FtpConfig {
        host: "127.0.0.1".to_string(),
        port,
        username: "testuser".to_string(),
        password: secrecy::SecretString::from("testpass".to_string()),
        tls_mode: FtpTlsMode::None,
        verify_cert: false,
        initial_path: Some("/".to_string()),
    };
    let mut provider = FtpProvider::new(config);
    provider.connect().await.expect("connect to fake FTP");
    (provider, recorded)
}

fn assert_refused_cleanly(result: Result<(), ProviderError>, commands: &[String]) {
    let err = result.expect_err("a CR/LF name must be refused");
    assert!(
        matches!(err, ProviderError::InvalidPath(_)),
        "the refusal must be typed InvalidPath, got: {err}"
    );
    assert!(
        !commands
            .iter()
            .any(|c| c.to_uppercase().starts_with("DELE")),
        "the injected verb must not cross the wire: {commands:?}"
    );
    assert!(
        !commands.iter().any(|c| c.contains("ledger")),
        "no fragment of the injected command may cross the wire: {commands:?}"
    );
}

/// The upload path is the one a crafted remote name travels on the way to
/// STOR; the refusal must fire before any byte of the command line is sent.
/// The CR/LF sits in the final component, so the CWD to the parent is clean
/// and only the STOR line would carry the injection.
#[tokio::test]
async fn upload_with_a_crlf_name_is_refused_and_never_reaches_the_wire() {
    let (mut provider, recorded) = connected_provider().await;
    let local = std::env::temp_dir().join(format!("kimi-crlf-{}.bin", std::process::id()));
    std::fs::File::create(&local)
        .unwrap()
        .write_all(b"x")
        .unwrap();
    let result = provider
        .upload(
            local.to_str().unwrap(),
            "/staging/report.bin\r\nDELE ledger.bin",
            None,
        )
        .await;
    provider.disconnect().await.ok();
    std::fs::remove_file(&local).ok();
    assert_refused_cleanly(result, &recorded.lock().unwrap());
}

/// Same refusal on the delete path, where the injected verb would be the
/// destructive one outright.
#[tokio::test]
async fn delete_with_a_crlf_name_is_refused_and_never_reaches_the_wire() {
    let (mut provider, recorded) = connected_provider().await;
    let result = provider.delete("/staging/old.bin\r\nDELE ledger.bin").await;
    provider.disconnect().await.ok();
    assert_refused_cleanly(result, &recorded.lock().unwrap());
}
