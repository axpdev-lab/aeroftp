//! `SftpProvider` transfers when the server goes away in the middle, over a
//! real SSH transport.
//!
//! russh-sftp 2.4 does not wake a request that waits for its reply when the
//! transport ends, and the acknowledgement of a pipelined WRITE has no timeout
//! of its own: an upload in flight when the server died or restarted waited
//! forever (live: `docker restart` of the server during `put -r`). The
//! provider now watches the SFTP channel's stream and ends every transfer
//! wait when it ends. The unit tests in `providers/sftp.rs` pin that logic
//! over an in-memory stream; this file pins the link the unit tests cannot
//! see, that russh ends the channel's stream when the TCP connection under it
//! goes away, so that a russh upgrade that stopped doing it would show here.
//!
//! The server is russh plus a minimal hand-rolled SFTP v3 packet loop (the
//! shape of `tests/sftp_size_hint.rs`) behind a TCP proxy. At the n-th WRITE
//! or READ the server stops answering, and a moment later the proxy drops
//! both sockets, as a server process that dies does. The moment lets the
//! client fill its pipeline, so every request in flight is waiting at the cut.

// Unix only, for the reason `tests/sftp_size_hint.rs` gives: the guard against
// writing into a real `known_hosts` is a redirected HOME, which russh does not
// read on Windows.
#![cfg(unix)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use ftp_client_gui_lib::providers::sftp::SftpProvider;
use ftp_client_gui_lib::providers::types::SftpConfig;
use ftp_client_gui_lib::providers::StorageProvider;
use russh::server::{Auth, ChannelOpenHandle, Handler, Msg, Server, Session};
use russh::{Channel, ChannelId};
use tokio::sync::Notify;

const SSH_FXP_INIT: u8 = 1;
const SSH_FXP_VERSION: u8 = 2;
const SSH_FXP_OPEN: u8 = 3;
const SSH_FXP_CLOSE: u8 = 4;
const SSH_FXP_READ: u8 = 5;
const SSH_FXP_WRITE: u8 = 6;
const SSH_FXP_LSTAT: u8 = 7;
const SSH_FXP_REALPATH: u8 = 16;
const SSH_FXP_STAT: u8 = 17;
const SSH_FXP_STATUS: u8 = 101;
const SSH_FXP_HANDLE: u8 = 102;
const SSH_FXP_DATA: u8 = 103;
const SSH_FXP_NAME: u8 = 104;
const SSH_FXP_ATTRS: u8 = 105;

const SSH_FX_OK: u32 = 0;
const SSH_FX_EOF: u32 = 1;
const SSH_FX_NO_SUCH_FILE: u32 = 2;
const SSH_FX_FAILURE: u32 = 4;
const SSH_FILEXFER_ATTR_SIZE: u32 = 0x00000001;
const SSH_FILEXFER_ATTR_PERMISSIONS: u32 = 0x00000004;

/// Longer than any of these transfers takes against the loopback server, so
/// running into it means the transfer hung.
const HUNG: std::time::Duration = std::time::Duration::from_secs(20);

fn w32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_be_bytes());
}

fn w64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_be_bytes());
}

fn wstr(out: &mut Vec<u8>, s: &[u8]) {
    w32(out, s.len() as u32);
    out.extend_from_slice(s);
}

fn r32(data: &[u8], pos: &mut usize) -> Option<u32> {
    let bytes = data.get(*pos..*pos + 4)?;
    *pos += 4;
    Some(u32::from_be_bytes(bytes.try_into().ok()?))
}

fn r64(data: &[u8], pos: &mut usize) -> Option<u64> {
    let bytes = data.get(*pos..*pos + 8)?;
    *pos += 8;
    Some(u64::from_be_bytes(bytes.try_into().ok()?))
}

fn rstr(data: &[u8], pos: &mut usize) -> Option<String> {
    let len = r32(data, pos)? as usize;
    let bytes = data.get(*pos..*pos + len)?;
    *pos += len;
    Some(String::from_utf8_lossy(bytes).into_owned())
}

fn status(id: u32, code: u32, msg: &str) -> Vec<u8> {
    let mut r = vec![SSH_FXP_STATUS];
    w32(&mut r, id);
    w32(&mut r, code);
    wstr(&mut r, msg.as_bytes());
    wstr(&mut r, b"");
    r
}

/// Where the server goes away: at the n-th WRITE or the n-th READ.
#[derive(Clone, Copy)]
enum GoAwayAt {
    Write(u32),
    Read(u32),
}

/// Only the first connection goes away: a client that dials again finds the
/// server back, as after a restart.
struct GoingAwayServer {
    source: Arc<Vec<u8>>,
    at: GoAwayAt,
    cut: Arc<Notify>,
    connections: u32,
}

struct GoingAwayHandler {
    source: Arc<Vec<u8>>,
    at: GoAwayAt,
    cut: Arc<Notify>,
    first_connection: bool,
    buf: Vec<u8>,
    writes: u32,
    reads: u32,
    gone: bool,
    /// The size of what each path was written up to on this connection, so
    /// an upload's size check finds it.
    written: HashMap<String, u64>,
}

impl GoingAwayHandler {
    /// Answer nothing more, and have the proxy drop both sockets a moment
    /// later.
    fn go_away(&mut self) {
        self.gone = true;
        let cut = self.cut.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            cut.notify_one();
        });
    }

    fn process(&mut self, data: &[u8], channel: ChannelId, session: &mut Session) {
        if data.is_empty() || self.gone {
            return;
        }
        let ptype = data[0];
        let mut pos = 1usize;
        match ptype {
            SSH_FXP_INIT => {
                let mut r = vec![SSH_FXP_VERSION];
                w32(&mut r, 3);
                self.send(channel, r, session);
            }
            SSH_FXP_REALPATH => {
                let Some(id) = r32(data, &mut pos) else {
                    return;
                };
                let mut r = vec![SSH_FXP_NAME];
                w32(&mut r, id);
                w32(&mut r, 1);
                wstr(&mut r, b"/");
                wstr(&mut r, b"/");
                w32(&mut r, 0);
                self.send(channel, r, session);
            }
            SSH_FXP_STAT | SSH_FXP_LSTAT => {
                let Some(id) = r32(data, &mut pos) else {
                    return;
                };
                let path = rstr(data, &mut pos).unwrap_or_default();
                let size = if path == "/source.bin" {
                    Some(self.source.len() as u64)
                } else {
                    self.written.get(&path).copied()
                };
                if let Some(size) = size {
                    let mut r = vec![SSH_FXP_ATTRS];
                    w32(&mut r, id);
                    w32(
                        &mut r,
                        SSH_FILEXFER_ATTR_SIZE | SSH_FILEXFER_ATTR_PERMISSIONS,
                    );
                    w64(&mut r, size);
                    w32(&mut r, 0o100644);
                    self.send(channel, r, session);
                } else {
                    self.send(
                        channel,
                        status(id, SSH_FX_NO_SUCH_FILE, "no such file"),
                        session,
                    );
                }
            }
            SSH_FXP_OPEN => {
                let Some(id) = r32(data, &mut pos) else {
                    return;
                };
                let path = rstr(data, &mut pos).unwrap_or_default();
                if path != "/source.bin" {
                    self.written.insert(path.clone(), 0);
                }
                let mut r = vec![SSH_FXP_HANDLE];
                w32(&mut r, id);
                wstr(&mut r, path.as_bytes());
                self.send(channel, r, session);
            }
            SSH_FXP_WRITE => {
                let Some(id) = r32(data, &mut pos) else {
                    return;
                };
                let handle = rstr(data, &mut pos).unwrap_or_default();
                let offset = r64(data, &mut pos).unwrap_or(0);
                let len = r32(data, &mut pos).unwrap_or(0) as u64;
                let end = self.written.entry(handle).or_insert(0);
                *end = (*end).max(offset + len);
                self.writes += 1;
                if self.first_connection
                    && matches!(self.at, GoAwayAt::Write(n) if n == self.writes)
                {
                    self.go_away();
                    return;
                }
                self.send(channel, status(id, SSH_FX_OK, ""), session);
            }
            SSH_FXP_READ => {
                let Some(id) = r32(data, &mut pos) else {
                    return;
                };
                let _handle = rstr(data, &mut pos);
                let offset = r64(data, &mut pos).unwrap_or(0) as usize;
                let len = r32(data, &mut pos).unwrap_or(0) as usize;
                self.reads += 1;
                if self.first_connection && matches!(self.at, GoAwayAt::Read(n) if n == self.reads)
                {
                    self.go_away();
                    return;
                }
                if offset >= self.source.len() {
                    self.send(channel, status(id, SSH_FX_EOF, "eof"), session);
                    return;
                }
                let end = (offset + len).min(self.source.len());
                let mut r = vec![SSH_FXP_DATA];
                w32(&mut r, id);
                wstr(&mut r, &self.source[offset..end]);
                self.send(channel, r, session);
            }
            SSH_FXP_CLOSE => {
                let Some(id) = r32(data, &mut pos) else {
                    return;
                };
                self.send(channel, status(id, SSH_FX_OK, ""), session);
            }
            _ => {
                let id = r32(data, &mut pos).unwrap_or(0);
                self.send(channel, status(id, SSH_FX_FAILURE, "unsupported"), session);
            }
        }
    }

    fn send(&self, channel: ChannelId, payload: Vec<u8>, session: &mut Session) {
        let mut framed = Vec::with_capacity(4 + payload.len());
        w32(&mut framed, payload.len() as u32);
        framed.extend_from_slice(&payload);
        let _ = session.data(channel, framed);
    }
}

impl Server for GoingAwayServer {
    type Handler = GoingAwayHandler;
    fn new_client(&mut self, _: Option<SocketAddr>) -> GoingAwayHandler {
        self.connections += 1;
        GoingAwayHandler {
            source: self.source.clone(),
            at: self.at,
            cut: self.cut.clone(),
            first_connection: self.connections == 1,
            buf: Vec::new(),
            writes: 0,
            reads: 0,
            gone: false,
            written: HashMap::new(),
        }
    }
}

impl Handler for GoingAwayHandler {
    type Error = russh::Error;

    async fn auth_password(&mut self, _user: &str, _password: &str) -> Result<Auth, Self::Error> {
        Ok(Auth::Accept)
    }

    async fn channel_open_session(
        &mut self,
        _channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if name == "sftp" {
            let _ = session.channel_success(channel);
        } else {
            let _ = session.channel_failure(channel);
        }
        Ok(())
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.buf.extend_from_slice(data);
        while self.buf.len() >= 4 {
            let pkt_len = u32::from_be_bytes(self.buf[0..4].try_into().unwrap()) as usize;
            if self.buf.len() < 4 + pkt_len {
                break;
            }
            let pkt = self.buf[4..4 + pkt_len].to_vec();
            self.buf.drain(..4 + pkt_len);
            self.process(&pkt, channel, session);
        }
        Ok(())
    }
}

/// Start the server and the proxy in front of it; returns the proxy's port.
/// When the server goes away the proxy drops both sockets of the connection.
async fn start_server(source: Vec<u8>, at: GoAwayAt) -> u16 {
    let cut = Arc::new(Notify::new());
    let server = GoingAwayServer {
        source: Arc::new(source),
        at,
        cut: cut.clone(),
        connections: 0,
    };
    let key =
        russh::keys::PrivateKey::random(&mut rand_010::rng(), russh::keys::Algorithm::Ed25519)
            .expect("ed25519 key");
    let config = Arc::new(russh::server::Config {
        methods: russh::MethodSet::from(&[russh::MethodKind::Password][..]),
        keys: vec![key],
        ..Default::default()
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server_addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let mut server = server;
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let cfg = config.clone();
            let handler = server.new_client(None);
            tokio::spawn(async move {
                if let Ok(running) = russh::server::run_stream(cfg, stream, handler).await {
                    let _ = running.await;
                }
            });
        }
    });

    let proxy = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_port = proxy.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut client, _)) = proxy.accept().await else {
                return;
            };
            let Ok(mut upstream) = tokio::net::TcpStream::connect(server_addr).await else {
                return;
            };
            let cut = cut.clone();
            tokio::spawn(async move {
                tokio::select! {
                    _ = tokio::io::copy_bidirectional(&mut client, &mut upstream) => {}
                    _ = cut.notified() => {}
                }
                // Both sockets drop here: the client's connection is gone.
            });
        }
    });
    proxy_port
}

/// HOME is process-wide (`known_hosts`), so the tests take turns.
static HOME_GUARD: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn connect(port: u16, label: &str) -> (SftpProvider, std::path::PathBuf) {
    let home =
        std::env::temp_dir().join(format!("sftp-goes-away-{}-{}", label, std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    // Safety: test process only, under HOME_GUARD; keeps the accept of the
    // ephemeral host key out of the real known_hosts.
    unsafe { std::env::set_var("HOME", &home) };
    let mut provider = SftpProvider::new(SftpConfig {
        host: "127.0.0.1".to_string(),
        port,
        username: "testuser".to_string(),
        password: Some(secrecy::SecretString::from("testpass".to_string())),
        private_key_path: None,
        key_passphrase: None,
        initial_path: Some("/".to_string()),
        timeout_secs: 30,
        trust_unknown_hosts: true,
    });
    tokio::time::timeout(std::time::Duration::from_secs(15), provider.connect())
        .await
        .expect("connect timed out")
        .expect("connect to the fake SFTP server");
    (provider, home)
}

/// An upload with writes in flight when the server goes away fails as a lost
/// connection, where it waited forever.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_upload_fails_when_the_server_goes_away() {
    let _home_guard = HOME_GUARD.lock().await;
    let port = start_server(Vec::new(), GoAwayAt::Write(3)).await;
    let (mut provider, home) = connect(port, "upload").await;
    let local = home.join("local.bin");
    std::fs::write(&local, vec![7u8; 8 * 1024 * 1024]).unwrap();
    let outcome = tokio::time::timeout(
        HUNG,
        provider.upload(&local.to_string_lossy(), "/uploaded.bin", None),
    )
    .await
    .expect("the upload hung after the server went away");
    let err = outcome.expect_err("an upload cut in the middle cannot succeed");
    assert!(err.is_connection_lost(), "{err:?}");
    assert!(!provider.is_connected(), "the session is gone, and says so");
    std::fs::remove_dir_all(&home).ok();
}

/// A download with reads in flight when the server goes away fails at once
/// as a lost connection, where it waited out russh-sftp's 10 s request
/// timeout and then reported a transfer failure.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_download_fails_at_once_when_the_server_goes_away() {
    let _home_guard = HOME_GUARD.lock().await;
    let source: Vec<u8> = (0..8 * 1024 * 1024u32).map(|i| (i % 251) as u8).collect();
    let port = start_server(source, GoAwayAt::Read(3)).await;
    let (mut provider, home) = connect(port, "download").await;
    let local = home.join("downloaded.bin");
    let started = std::time::Instant::now();
    let outcome = tokio::time::timeout(
        HUNG,
        provider.download("/source.bin", &local.to_string_lossy(), None),
    )
    .await
    .expect("the download hung after the server went away");
    let err = outcome.expect_err("a download cut in the middle cannot succeed");
    assert!(err.is_connection_lost(), "{err:?}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "took {:?}: it waited for replies that could no longer come",
        started.elapsed()
    );
    std::fs::remove_dir_all(&home).ok();
}

/// After the transport ended, the next transfer on the same provider dials
/// again (`ensure_connected`) and succeeds against the server that came back,
/// instead of failing on the dead session.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_transfer_after_the_server_came_back_dials_again() {
    let _home_guard = HOME_GUARD.lock().await;
    let port = start_server(Vec::new(), GoAwayAt::Write(3)).await;
    let (mut provider, home) = connect(port, "redial").await;
    let big = home.join("big.bin");
    std::fs::write(&big, vec![7u8; 8 * 1024 * 1024]).unwrap();
    let first = tokio::time::timeout(
        HUNG,
        provider.upload(&big.to_string_lossy(), "/first.bin", None),
    )
    .await
    .expect("the first upload hung");
    assert!(
        first.as_ref().is_err_and(|e| e.is_connection_lost()),
        "{first:?}"
    );
    let small = home.join("small.bin");
    std::fs::write(&small, vec![9u8; 64 * 1024]).unwrap();
    let second = tokio::time::timeout(
        HUNG,
        provider.upload(&small.to_string_lossy(), "/second.bin", None),
    )
    .await
    .expect("the second upload hung");
    second.expect("a new connection to the server that came back");
    assert!(provider.is_connected());
    std::fs::remove_dir_all(&home).ok();
}
