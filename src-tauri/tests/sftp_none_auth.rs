//! `SftpProvider::connect` against a server that lets the account in with
//! no credential, the way OpenWrt's Dropbear does for a root with no
//! password set.
//!
//! That Dropbear accepts only the SSH `none` authentication method for such
//! an account and refuses the password method outright for a blank
//! password, so a client that never sends `none` cannot get in at all,
//! whatever it puts in the password field. OpenSSH, PuTTY and WinSCP send
//! `none` before any credential (RFC 4252 section 5.2); the provider does
//! the same, and every other server refuses it and gets the configured
//! credential next.
//!
//! The server below is russh plus the smallest SFTP v3 loop `connect()`
//! needs (INIT and REALPATH), the shape of `tests/sftp_rename.rs`, with
//! every `none` and password attempt counted.

// Unix only, for the reason `tests/sftp_size_hint.rs` gives: the guard
// against writing into a real `known_hosts` is a redirected `HOME`, which
// no environment variable redirects on Windows.
#![cfg(unix)]

#[path = "support/scoped_env.rs"]
mod scoped_env;

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ftp_client_gui_lib::providers::sftp::SftpProvider;
use ftp_client_gui_lib::providers::types::SftpConfig;
use ftp_client_gui_lib::providers::{ProviderError, StorageProvider};
use russh::server::{Auth, ChannelOpenHandle, Handler, Msg, Server, Session};
use russh::{Channel, ChannelId};
use scoped_env::ScopedEnv;

const SSH_FXP_INIT: u8 = 1;
const SSH_FXP_VERSION: u8 = 2;
const SSH_FXP_REALPATH: u8 = 16;
const SSH_FXP_STATUS: u8 = 101;
const SSH_FXP_NAME: u8 = 104;

const SSH_FX_FAILURE: u32 = 4;

fn w32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_be_bytes());
}

fn wstr(out: &mut Vec<u8>, s: &[u8]) {
    w32(out, s.len() as u32);
    out.extend_from_slice(s);
}

fn r32(data: &[u8], pos: &mut usize) -> Option<u32> {
    let v = u32::from_be_bytes(data.get(*pos..*pos + 4)?.try_into().ok()?);
    *pos += 4;
    Some(v)
}

/// How the server answers each method, and how many attempts reached it.
#[derive(Default)]
struct Policy {
    /// Lets the account in with no credential (OpenWrt's root with no
    /// password set).
    accept_none: bool,
    /// The one password it accepts. `None` refuses every password, as that
    /// Dropbear does for an account whose password is blank.
    password: Option<&'static str>,
    nones: AtomicU32,
    passwords: AtomicU32,
}

struct AuthServer {
    policy: Arc<Policy>,
}

struct AuthHandler {
    policy: Arc<Policy>,
    buf: Vec<u8>,
}

impl AuthHandler {
    fn process(&mut self, data: &[u8]) -> Option<Vec<u8>> {
        let ptype = *data.first()?;
        let mut pos = 1usize;
        if ptype == SSH_FXP_INIT {
            let mut r = vec![SSH_FXP_VERSION];
            w32(&mut r, 3);
            return Some(r);
        }
        let id = r32(data, &mut pos)?;
        let reply = if ptype == SSH_FXP_REALPATH {
            let mut r = vec![SSH_FXP_NAME];
            w32(&mut r, id);
            w32(&mut r, 1);
            wstr(&mut r, b"/");
            wstr(&mut r, b"/");
            w32(&mut r, 0);
            r
        } else {
            let mut r = vec![SSH_FXP_STATUS];
            w32(&mut r, id);
            w32(&mut r, SSH_FX_FAILURE);
            wstr(&mut r, b"unsupported");
            wstr(&mut r, b"");
            r
        };
        Some(reply)
    }
}

impl Server for AuthServer {
    type Handler = AuthHandler;
    fn new_client(&mut self, _: Option<SocketAddr>) -> AuthHandler {
        AuthHandler {
            policy: self.policy.clone(),
            buf: Vec::new(),
        }
    }
}

impl Handler for AuthHandler {
    type Error = russh::Error;

    async fn auth_none(&mut self, _user: &str) -> Result<Auth, Self::Error> {
        self.policy.nones.fetch_add(1, Ordering::SeqCst);
        Ok(if self.policy.accept_none {
            Auth::Accept
        } else {
            Auth::reject()
        })
    }

    async fn auth_password(&mut self, _user: &str, password: &str) -> Result<Auth, Self::Error> {
        self.policy.passwords.fetch_add(1, Ordering::SeqCst);
        Ok(if self.policy.password == Some(password) {
            Auth::Accept
        } else {
            Auth::reject()
        })
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
            if let Some(reply) = self.process(&pkt) {
                let mut framed = Vec::with_capacity(4 + reply.len());
                w32(&mut framed, reply.len() as u32);
                framed.extend_from_slice(&reply);
                let _ = session.data(channel, framed);
            }
        }
        Ok(())
    }
}

/// Start the server on loopback with `policy`; returns its port and policy.
async fn start_server(policy: Policy) -> (u16, Arc<Policy>) {
    let policy = Arc::new(policy);
    let server = AuthServer {
        policy: policy.clone(),
    };
    let key =
        russh::keys::PrivateKey::random(&mut rand_010::rng(), russh::keys::Algorithm::Ed25519)
            .expect("ed25519 key");
    let config = Arc::new(russh::server::Config {
        methods: russh::MethodSet::from(&[russh::MethodKind::Password][..]),
        // A refused `none` is answered at once, as OpenSSH and Dropbear do;
        // russh's default would hold it a second.
        auth_rejection_time_initial: Some(Duration::ZERO),
        keys: vec![key],
        ..Default::default()
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let mut server = server;
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
    (port, policy)
}

async fn connect(port: u16, password: Option<&str>) -> Result<SftpProvider, ProviderError> {
    let mut p = SftpProvider::new(SftpConfig {
        host: "127.0.0.1".to_string(),
        port,
        username: "root".to_string(),
        password: password.map(|pw| secrecy::SecretString::from(pw.to_string())),
        private_key_path: None,
        key_passphrase: None,
        initial_path: Some("/".to_string()),
        timeout_secs: 30,
        trust_unknown_hosts: true,
    });
    tokio::time::timeout(Duration::from_secs(15), p.connect())
        .await
        .expect("connect timed out")?;
    Ok(p)
}

/// One test, sequential phases: HOME is process-wide.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connect_sends_none_before_any_credential() {
    let home = std::env::temp_dir().join(format!("sftp-none-auth-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    // Guards the real known_hosts from the trust_unknown_hosts accept-and-save
    // path.
    let _home_env = ScopedEnv::set("HOME", &home);

    // 1. OpenWrt's root with no password set, the password field left
    // empty: `none` lets it in and no password is sent.
    let (port, policy) = start_server(Policy {
        accept_none: true,
        ..Policy::default()
    })
    .await;
    let mut provider = connect(port, None).await.expect("in through none");
    assert!(provider.is_connected());
    assert_eq!(policy.nones.load(Ordering::SeqCst), 1);
    assert_eq!(policy.passwords.load(Ordering::SeqCst), 0);
    provider.disconnect().await.unwrap();

    // 2. The same device with an empty string saved as the password: that
    // Dropbear refuses a blank password, so only `none` gets in.
    let (port, policy) = start_server(Policy {
        accept_none: true,
        ..Policy::default()
    })
    .await;
    let mut provider = connect(port, Some("")).await.expect("in through none");
    assert_eq!(policy.nones.load(Ordering::SeqCst), 1);
    assert_eq!(policy.passwords.load(Ordering::SeqCst), 0);
    provider.disconnect().await.unwrap();

    // 3. Every other server refuses `none`, and the password follows on the
    // same connection.
    let (port, policy) = start_server(Policy {
        password: Some("testpass"),
        ..Policy::default()
    })
    .await;
    let mut provider = connect(port, Some("testpass"))
        .await
        .expect("in through the password after a refused none");
    assert_eq!(policy.nones.load(Ordering::SeqCst), 1);
    assert_eq!(policy.passwords.load(Ordering::SeqCst), 1);
    provider.disconnect().await.unwrap();

    // 4. A refused `none` and nothing configured to follow it is an
    // authentication failure that says what is missing.
    let (port, _policy) = start_server(Policy {
        password: Some("testpass"),
        ..Policy::default()
    })
    .await;
    match connect(port, None).await {
        Err(ProviderError::AuthenticationFailed(msg)) => {
            assert!(msg.contains("password or a private key"), "{msg}")
        }
        Err(other) => panic!("expected AuthenticationFailed, got {other:?}"),
        Ok(_) => panic!("a server that refuses none let in an account with no credential"),
    }

    let _ = std::fs::remove_dir_all(&home);
}
