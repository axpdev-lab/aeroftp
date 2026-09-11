//! Two FTP defects that need a server to show, against a scripted fake one.
//!
//! 1. `resume_upload` opens its data channel with `append_file`, which is the
//!    one transfer entry point never brought under the deadline that bounds
//!    every other open: a server that takes the `APPE` and never answers leaves
//!    the resume waiting for as long as the process lives.
//! 2. A server that refuses and then hangs up sends `550` and `421` in that
//!    order, and on one write they arrive together. The `550` answers the
//!    command; the `421` stays in the control reader, and the NEXT command
//!    reads it as its own reply, so an operation fails citing an answer caused
//!    by the command before it.
//!
//! Neither needs the Docker fixture: the server below is a scripted fake on
//! loopback, enough FTP for connect plus the one command each test drives.

use std::io::Write as _;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ftp_client_gui_lib::providers::types::{FtpConfig, FtpTlsMode};
use ftp_client_gui_lib::providers::{FtpProvider, StorageProvider};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

/// What the fake server does with the command each test is about.
#[derive(Clone, Copy)]
enum Script {
    /// Answer `PASV`, then take `APPE` and never say anything about it: the
    /// shape of a server that accepts the command and stops.
    SwallowAppe,
    /// Answer `RETR` with a refusal and a goodbye in ONE write, so both replies
    /// reach the client in the same segment.
    RefuseThenHangUp,
}

/// Every control command the fake server received, in order.
type Commands = Arc<Mutex<Vec<String>>>;

async fn serve(
    mut stop: tokio::sync::watch::Receiver<bool>,
    listener: TcpListener,
    script: Script,
    commands: Commands,
) {
    loop {
        let accepted = tokio::select! {
            _ = stop.changed() => return,
            a = listener.accept() => a,
        };
        let (stream, _) = match accepted {
            Ok(v) => v,
            Err(_) => return,
        };
        let commands = Arc::clone(&commands);
        tokio::spawn(async move { session(stream, script, commands).await });
    }
}

async fn session(stream: tokio::net::TcpStream, script: Script, commands: Commands) {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    if write.write_all(b"220 FakeFTP ready\r\n").await.is_err() {
        return;
    }
    // A PASV listener lives until the next data command consumes it. It is kept
    // alive deliberately in `SwallowAppe`: the client's connect succeeds and the
    // wait that follows is the one the test is about.
    let mut pasv: Option<TcpListener> = None;
    while let Ok(Some(line)) = lines.next_line().await {
        commands.lock().unwrap().push(line.clone());
        let verb = line.split_whitespace().next().unwrap_or("").to_uppercase();
        match verb.as_str() {
            "USER" => reply(&mut write, b"331 Password required\r\n").await,
            "PASS" => reply(&mut write, b"230 Logged in\r\n").await,
            "TYPE" => reply(&mut write, b"200 Type set\r\n").await,
            "FEAT" => reply(&mut write, b"211-Features:\r\n MLST\r\n211 End\r\n").await,
            "PWD" => reply(&mut write, b"257 \"/\"\r\n").await,
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
            "APPE" => match script {
                // Not a reply, not a close: the command is taken and nothing
                // comes back, which is what leaves an unbounded open waiting.
                Script::SwallowAppe => {
                    // The PASV listener stays alive on purpose: the client's
                    // connect succeeds, so what it waits on is the reply to
                    // APPE, which never comes. Dropping the listener here would
                    // refuse the connection and end the wait for the wrong
                    // reason.
                    continue;
                }
                Script::RefuseThenHangUp => reply(&mut write, b"550 Not allowed\r\n").await,
            },
            "RETR" => match script {
                Script::RefuseThenHangUp => {
                    // ONE write, so the refusal and the goodbye arrive in the
                    // same segment: a second peek at the socket would see
                    // nothing, which is why the reader's own buffer is what has
                    // to be asked.
                    reply(
                        &mut write,
                        b"550 Failed to open file\r\n421 Service not available, closing control connection\r\n",
                    )
                    .await;
                }
                Script::SwallowAppe => reply(&mut write, b"550 Not found\r\n").await,
            },
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
    commands: Commands,
    stop: tokio::sync::watch::Sender<bool>,
}

async fn start_fake_ftp(script: Script) -> FakeFtp {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let commands: Commands = Arc::new(Mutex::new(Vec::new()));
    let (stop, rx) = tokio::sync::watch::channel(false);
    tokio::spawn(serve(rx, listener, script, Arc::clone(&commands)));
    FakeFtp {
        port,
        commands,
        stop,
    }
}

impl Drop for FakeFtp {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
    }
}

async fn connected(port: u16) -> FtpProvider {
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
    provider
        .connect()
        .await
        .expect("connect to the fake server");
    provider
}

/// A resume whose `APPE` is never answered has to end by itself.
///
/// Every other data open on this provider is bounded; `resume_upload` reached
/// the server through `append_file`, which opens and transfers in one call with
/// no cap of its own, so this wait had no end. The assertion is that the call
/// returns at all: the error it returns is the server's business, the hang is
/// ours. The outer timeout is the test's own limit and is far above the
/// provider's, so it only fires when nothing bounds the wait.
#[tokio::test]
async fn a_resume_whose_append_is_never_answered_gives_up() {
    let server = start_fake_ftp(Script::SwallowAppe).await;
    let mut provider = connected(server.port).await;

    let local = std::env::temp_dir().join(format!("aeroftp-resume-{}.bin", std::process::id()));
    std::fs::File::create(&local)
        .unwrap()
        .write_all(&vec![b'x'; 4096])
        .unwrap();

    let outcome = tokio::time::timeout(
        Duration::from_secs(90),
        provider.resume_upload(local.to_str().unwrap(), "/resume.bin", 1024, None),
    )
    .await;
    let _ = std::fs::remove_file(&local);

    let ended = outcome.expect(
        "the resume never came back: an APPE the server does not answer must be bounded like \
         every other data open on this provider",
    );
    assert!(
        ended.is_err(),
        "a resume the server never answered must fail, not report success"
    );
    let commands = server.commands.lock().unwrap().clone();
    assert!(
        commands
            .iter()
            .any(|c| c.to_uppercase().starts_with("APPE")),
        "the test drove the path it is about: {:?}",
        commands
    );
}

/// A refusal followed by a goodbye must not answer the next command.
///
/// The server sends `550` and `421` in one write. The `550` answers the `RETR`;
/// the `421` is then sitting in the control reader, and a session kept in that
/// state hands the next command somebody else's reply. What the next command
/// may do is fail or reconnect; what it may not do is come back carrying the
/// goodbye that belonged to the command before it.
#[tokio::test]
async fn a_refusal_followed_by_a_goodbye_does_not_answer_the_next_command() {
    let server = start_fake_ftp(Script::RefuseThenHangUp).await;
    let mut provider = connected(server.port).await;

    let local = std::env::temp_dir().join(format!("aeroftp-421-{}.bin", std::process::id()));
    let refused = provider
        .download("/missing.bin", local.to_str().unwrap(), None)
        .await;
    let _ = std::fs::remove_file(&local);
    assert!(refused.is_err(), "the server refused the download");

    let next = provider.pwd().await;
    let text = match &next {
        Ok(dir) => dir.clone(),
        Err(err) => err.to_string(),
    };
    assert!(
        !text.contains("421") && !text.to_lowercase().contains("service not available"),
        "the next command was answered with the goodbye owed to the refused one: {:?}",
        next
    );
}
