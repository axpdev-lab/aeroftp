//! A refused SSH host key reaches the user as what it is.
//!
//! russh turns every host key the client handler refuses into the same
//! `UnknownKey` error, so a key that differed from the saved one (a possible
//! man-in-the-middle) came out of the CLI as "Unknown server key (check
//! hostname/port and verify the service is running)", exit 1, retried twice.
//! The provider now says why it refused, in an error of its own that is not
//! retried: a key that changed, a host with no key saved, a known_hosts that
//! could not be read.
//!
//! The server is russh with a minimal hand-rolled SFTP v3 loop (the shape of
//! `tests/sftp_rename.rs`): INIT, REALPATH and STAT, enough for `connect`.

// Unix only, for the reason `tests/sftp_size_hint.rs` gives: known_hosts is
// redirected through `HOME`, which no environment variable redirects on
// Windows.
#![cfg(unix)]

#[path = "support/scoped_env.rs"]
mod scoped_env;

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use ftp_client_gui_lib::providers::sftp::SftpProvider;
use ftp_client_gui_lib::providers::types::SftpConfig;
use ftp_client_gui_lib::providers::{ProviderError, StorageProvider};
use russh::keys::{Algorithm, PrivateKey};
use russh::server::{Auth, ChannelOpenHandle, Handler, Msg, Server, Session};
use russh::{Channel, ChannelId};
use scoped_env::ScopedEnv;

const SSH_FXP_INIT: u8 = 1;
const SSH_FXP_VERSION: u8 = 2;
const SSH_FXP_LSTAT: u8 = 7;
const SSH_FXP_REALPATH: u8 = 16;
const SSH_FXP_STAT: u8 = 17;
const SSH_FXP_STATUS: u8 = 101;
const SSH_FXP_NAME: u8 = 104;
const SSH_FXP_ATTRS: u8 = 105;
const SSH_FX_FAILURE: u32 = 4;

fn w32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_be_bytes());
}

fn wstr(out: &mut Vec<u8>, s: &[u8]) {
    w32(out, s.len() as u32);
    out.extend_from_slice(s);
}

struct KeyServer;

struct KeyHandler {
    buf: Vec<u8>,
}

impl KeyHandler {
    fn process(data: &[u8]) -> Option<Vec<u8>> {
        let ptype = *data.first()?;
        if ptype == SSH_FXP_INIT {
            let mut r = vec![SSH_FXP_VERSION];
            w32(&mut r, 3);
            return Some(r);
        }
        let id = u32::from_be_bytes(data.get(1..5)?.try_into().ok()?);
        let mut r = Vec::new();
        match ptype {
            SSH_FXP_REALPATH => {
                r.push(SSH_FXP_NAME);
                w32(&mut r, id);
                w32(&mut r, 1);
                wstr(&mut r, b"/");
                wstr(&mut r, b"/");
                w32(&mut r, 0);
            }
            SSH_FXP_STAT | SSH_FXP_LSTAT => {
                r.push(SSH_FXP_ATTRS);
                w32(&mut r, id);
                w32(&mut r, 0);
            }
            _ => {
                r.push(SSH_FXP_STATUS);
                w32(&mut r, id);
                w32(&mut r, SSH_FX_FAILURE);
                wstr(&mut r, b"unsupported");
                wstr(&mut r, b"");
            }
        }
        Some(r)
    }
}

impl Server for KeyServer {
    type Handler = KeyHandler;
    fn new_client(&mut self, _: Option<SocketAddr>) -> KeyHandler {
        KeyHandler { buf: Vec::new() }
    }
}

impl Handler for KeyHandler {
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
            if let Some(reply) = Self::process(&pkt) {
                let mut framed = Vec::with_capacity(4 + reply.len());
                w32(&mut framed, reply.len() as u32);
                framed.extend_from_slice(&reply);
                let _ = session.data(channel, framed);
            }
        }
        Ok(())
    }
}

fn ed25519() -> PrivateKey {
    PrivateKey::random(&mut rand_010::rng(), Algorithm::Ed25519).expect("ed25519 key")
}

/// Start the server on loopback with `key` as its host key; returns its port.
async fn start_server(key: PrivateKey) -> u16 {
    let config = Arc::new(russh::server::Config {
        methods: russh::MethodSet::from(&[russh::MethodKind::Password][..]),
        keys: vec![key],
        ..Default::default()
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let mut server = KeyServer;
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let cfg = config.clone();
            let handler = server.new_client(None);
            tokio::spawn(async move {
                let _ = russh::server::run_stream(cfg, stream, handler).await;
            });
        }
    });
    port
}

async fn connect(port: u16, trust_unknown_hosts: bool) -> Result<SftpProvider, ProviderError> {
    let mut p = SftpProvider::new(SftpConfig {
        host: "127.0.0.1".to_string(),
        port,
        username: "testuser".to_string(),
        password: Some(secrecy::SecretString::from("testpass".to_string())),
        private_key_path: None,
        key_passphrase: None,
        initial_path: Some("/".to_string()),
        timeout_secs: 30,
        trust_unknown_hosts,
    });
    tokio::time::timeout(std::time::Duration::from_secs(15), p.connect())
        .await
        .expect("connect timed out")?;
    Ok(p)
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// One test, sequential phases: HOME is process-wide.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_host_key_is_named_and_not_taken_for_a_server_that_is_down() {
    let home = std::env::temp_dir().join(format!("sftp-host-key-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join(".ssh")).unwrap();
    let _home_env = ScopedEnv::set("HOME", &home);
    let known_hosts = home.join(".ssh").join("known_hosts");

    let server_key = ed25519();
    let port = start_server(server_key.clone()).await;

    // 1. No key saved and unknown keys not trusted: refused, and the error
    // says the host has no key saved, not that the server is down.
    let refused = connect(port, false).await.err();
    match &refused {
        Some(ProviderError::HostKeyRejected(why)) => {
            assert!(why.contains("has no key saved"), "{why}");
            assert!(why.contains("--trust-host-key"), "{why}");
        }
        other => panic!("a host with no key saved was refused as {other:?}"),
    }

    // 2. The control: the same server is reachable and accepted once its key
    // is trusted, which saves it, and a later connect needs no trust.
    let mut provider = connect(port, true).await.expect("a trusted key connects");
    provider.disconnect().await.unwrap();
    assert!(read(&known_hosts).contains(&format!("[127.0.0.1]:{port}")));
    let mut provider = connect(port, false)
        .await
        .expect("a saved key connects without trust");
    provider.disconnect().await.unwrap();

    // 3. A different key of the same type saved for the host: refused even
    // with unknown keys trusted, known_hosts left as it was, and the error
    // names the line and the command that removes the old key.
    std::fs::remove_file(&known_hosts).unwrap();
    russh::keys::known_hosts::learn_known_hosts_path(
        "127.0.0.1",
        port,
        ed25519().public_key(),
        &known_hosts,
    )
    .unwrap();
    let saved = read(&known_hosts);
    // The line that holds the key (russh writes a newline first into a file
    // that does not end with one, an empty file included).
    let line = saved
        .lines()
        .position(|l| l.starts_with(&format!("[127.0.0.1]:{port} ")))
        .expect("the saved key")
        + 1;
    let refused = connect(port, true).await.err();
    match &refused {
        Some(ProviderError::HostKeyRejected(why)) => {
            assert!(why.contains("different key"), "{why}");
            assert!(why.contains(&format!("(line {line})")), "{why}");
            assert!(
                why.contains(&format!("ssh-keygen -R '[127.0.0.1]:{port}'")),
                "{why}"
            );
        }
        other => panic!("a changed key was refused as {other:?}"),
    }
    assert_eq!(read(&known_hosts), saved, "a refused key must not be saved");

    // 4. A known_hosts that cannot be read: refused, saying so.
    std::fs::remove_file(&known_hosts).unwrap();
    std::fs::create_dir(&known_hosts).unwrap();
    let refused = connect(port, true).await.err();
    match &refused {
        Some(ProviderError::HostKeyRejected(why)) => {
            assert!(why.contains("could not be checked"), "{why}");
        }
        other => panic!("an unreadable known_hosts was refused as {other:?}"),
    }

    drop(_home_env);
    let _ = std::fs::remove_dir_all(&home);
}
