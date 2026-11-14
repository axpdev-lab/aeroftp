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
//! every `none`, password and public key attempt recorded in order.
//!
//! The same server pins what a later dial of the same session may send: a
//! pool worker or a reconnect after a `none` login, and the delta transport
//! of a session that logged in with a key, once the key file was replaced.

// Unix only, for the reason `tests/sftp_size_hint.rs` gives: the guard
// against writing into a real `known_hosts` is a redirected `HOME`, which
// no environment variable redirects on Windows.
#![cfg(unix)]

#[path = "support/scoped_env.rs"]
mod scoped_env;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ftp_client_gui_lib::providers::sftp::SftpProvider;
use ftp_client_gui_lib::providers::types::SftpConfig;
use ftp_client_gui_lib::providers::{ProviderError, StorageProvider};
use russh::keys::ssh_key::LineEnding;
use russh::keys::{PrivateKey, PublicKey};
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

/// How the server answers each method, and the attempts that reached it.
#[derive(Default)]
struct Policy {
    /// Lets the account in with no credential (OpenWrt's root with no
    /// password set).
    accept_none: bool,
    /// The one password it accepts. `None` refuses every password, as that
    /// Dropbear does for an account whose password is blank.
    password: Option<&'static str>,
    /// The one public key it accepts. `None` refuses every key.
    key: Option<PublicKey>,
    /// The one answer it accepts to its keyboard-interactive password
    /// prompt (servers that refuse the `password` method and ask this way).
    keyboard_interactive: Option<&'static str>,
    /// Every attempt in arrival order: `"none"`, `"password"` or
    /// `"publickey"` (a key whose signature checked out).
    attempts: Mutex<Vec<&'static str>>,
    /// Set while the server runs to refuse `none` from then on, whatever
    /// `accept_none` says.
    refuse_none: AtomicBool,
    /// Set while the server runs to refuse the `password` method from then
    /// on, whatever `password` says.
    refuse_password: AtomicBool,
    /// SHA-256 fingerprint of every public key named in a `publickey`
    /// request, with or without a signature, in arrival order.
    offered_keys: Mutex<Vec<String>>,
}

impl Policy {
    fn record(&self, method: &'static str) {
        self.attempts.lock().unwrap().push(method);
    }

    fn attempts(&self) -> Vec<&'static str> {
        self.attempts.lock().unwrap().clone()
    }

    fn offered_keys(&self) -> Vec<String> {
        self.offered_keys.lock().unwrap().clone()
    }
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
        self.policy.record("none");
        let accept = self.policy.accept_none && !self.policy.refuse_none.load(Ordering::SeqCst);
        Ok(if accept { Auth::Accept } else { Auth::reject() })
    }

    async fn auth_password(&mut self, _user: &str, password: &str) -> Result<Auth, Self::Error> {
        self.policy.record("password");
        Ok(
            if self.policy.password == Some(password)
                && !self.policy.refuse_password.load(Ordering::SeqCst)
            {
                Auth::Accept
            } else {
                Auth::reject()
            },
        )
    }

    async fn auth_keyboard_interactive<'a>(
        &'a mut self,
        _user: &str,
        _submethods: &str,
        response: Option<russh::server::Response<'a>>,
    ) -> Result<Auth, Self::Error> {
        let Some(expected) = self.policy.keyboard_interactive else {
            return Ok(Auth::reject());
        };
        match response {
            None => Ok(Auth::Partial {
                name: "".into(),
                instructions: "".into(),
                prompts: vec![("Password: ".into(), false)].into(),
            }),
            Some(mut answers) => {
                let accepted = answers
                    .next()
                    .is_some_and(|answer| answer.as_ref() == expected.as_bytes());
                if accepted {
                    self.policy.record("keyboard-interactive");
                }
                Ok(if accepted {
                    Auth::Accept
                } else {
                    Auth::reject()
                })
            }
        }
    }

    /// Every key a client names, before any signature is checked.
    async fn auth_publickey_offered(
        &mut self,
        _user: &str,
        offer: russh::server::PublicKeyOffer<'_>,
    ) -> Result<Auth, Self::Error> {
        self.policy
            .offered_keys
            .lock()
            .unwrap()
            .push(fingerprint(offer.public_key));
        Ok(Auth::Accept)
    }

    /// Reached only once the client has proved it holds the key.
    async fn auth_publickey(
        &mut self,
        _user: &str,
        key: russh::server::VerifiedPublicKey<'_>,
    ) -> Result<Auth, Self::Error> {
        self.policy.record("publickey");
        Ok(
            if self.policy.key.as_ref().map(PublicKey::key_data) == Some(key.public_key.key_data())
            {
                Auth::Accept
            } else {
                Auth::reject()
            },
        )
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

    /// No command runs here: say so at once, so a client that asks for one
    /// (the SCP probe, `rsync --version`) gets an answer instead of waiting.
    async fn exec_request(
        &mut self,
        channel: ChannelId,
        _data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let _ = session.channel_failure(channel);
        let _ = session.close(channel);
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
        methods: russh::MethodSet::from(
            &[russh::MethodKind::Password, russh::MethodKind::PublicKey][..],
        ),
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

/// The credential a profile carries into `connect`.
enum Credential<'a> {
    Nothing,
    Password(&'a str),
    Key(&'a PathBuf),
}

/// Connect a provider to the loopback server on `port` as `root`, carrying
/// `credential`, with a 15 s cap so a hang fails the test.
async fn connect(port: u16, credential: Credential<'_>) -> Result<SftpProvider, ProviderError> {
    let (password, private_key_path) = match credential {
        Credential::Nothing => (None, None),
        Credential::Password(pw) => (Some(secrecy::SecretString::from(pw.to_string())), None),
        Credential::Key(path) => (None, Some(path.to_string_lossy().into_owned())),
    };
    let mut p = SftpProvider::new(SftpConfig {
        host: "127.0.0.1".to_string(),
        port,
        username: "root".to_string(),
        password,
        private_key_path,
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

fn fingerprint(key: &PublicKey) -> String {
    key.fingerprint(russh::keys::HashAlg::Sha256).to_string()
}

fn random_key() -> PrivateKey {
    PrivateKey::random(&mut rand_010::rng(), russh::keys::Algorithm::Ed25519).expect("ed25519 key")
}

/// Write `key` unencrypted at `path`, mode 0600, replacing what is there.
fn write_key(path: &std::path::Path, key: &PrivateKey) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(
        path,
        key.to_openssh(LineEnding::LF)
            .expect("openssh key")
            .as_bytes(),
    )
    .unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

/// HOME (and the settings root under it) is process-wide: the tests of this
/// file hold this while they redirect it.
static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// One test, sequential phases: HOME is process-wide.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connect_sends_none_before_any_credential() {
    let _env = ENV_LOCK.lock().await;
    let home = std::env::temp_dir().join(format!("sftp-none-auth-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    // Guards the real known_hosts from the trust_unknown_hosts accept-and-save
    // path.
    let _home_env = ScopedEnv::set("HOME", &home);

    // An unencrypted Ed25519 key, at an absolute path the provider loads
    // as given.
    let client_key = PrivateKey::random(&mut rand_010::rng(), russh::keys::Algorithm::Ed25519)
        .expect("ed25519 key");
    let key_path = home.join("id_ed25519");
    std::fs::write(
        &key_path,
        client_key
            .to_openssh(LineEnding::LF)
            .expect("openssh key")
            .as_bytes(),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    // 1. OpenWrt's root with no password set, the password field left
    // empty: `none` lets it in and no password is sent.
    let (port, policy) = start_server(Policy {
        accept_none: true,
        ..Policy::default()
    })
    .await;
    let mut provider = connect(port, Credential::Nothing)
        .await
        .expect("in through none");
    assert!(provider.is_connected());
    assert_eq!(policy.attempts(), ["none"]);
    provider.disconnect().await.unwrap();

    // 2. The same device with an empty string saved as the password: that
    // Dropbear refuses a blank password, so only `none` gets in.
    let (port, policy) = start_server(Policy {
        accept_none: true,
        ..Policy::default()
    })
    .await;
    let mut provider = connect(port, Credential::Password(""))
        .await
        .expect("in through none");
    assert_eq!(policy.attempts(), ["none"]);
    provider.disconnect().await.unwrap();

    // 3. Every other server refuses `none`, and the password follows on the
    // same connection.
    let (port, policy) = start_server(Policy {
        password: Some("testpass"),
        ..Policy::default()
    })
    .await;
    let mut provider = connect(port, Credential::Password("testpass"))
        .await
        .expect("in through the password after a refused none");
    assert_eq!(policy.attempts(), ["none", "password"]);
    provider.disconnect().await.unwrap();

    // 4. A refused `none` and nothing configured to follow it is an
    // authentication failure that says what is missing.
    let (port, policy) = start_server(Policy {
        password: Some("testpass"),
        ..Policy::default()
    })
    .await;
    match connect(port, Credential::Nothing).await {
        Err(ProviderError::AuthenticationFailed(msg)) => {
            assert!(msg.contains("password or a private key"), "{msg}")
        }
        Err(other) => panic!("expected AuthenticationFailed, got {other:?}"),
        Ok(_) => panic!("a server that refuses none let in an account with no credential"),
    }
    assert_eq!(policy.attempts(), ["none"]);

    // 5. A profile with a private key on a server that lets the account in
    // through `none`: the key is not offered at all.
    let (port, policy) = start_server(Policy {
        accept_none: true,
        key: Some(client_key.public_key().clone()),
        ..Policy::default()
    })
    .await;
    let mut provider = connect(port, Credential::Key(&key_path))
        .await
        .expect("in through none");
    assert_eq!(policy.attempts(), ["none"]);
    provider.disconnect().await.unwrap();

    // 6. The same profile on a server that refuses `none`: the key follows
    // on the same connection.
    let (port, policy) = start_server(Policy {
        key: Some(client_key.public_key().clone()),
        ..Policy::default()
    })
    .await;
    let mut provider = connect(port, Credential::Key(&key_path))
        .await
        .expect("in through the key after a refused none");
    assert_eq!(policy.attempts(), ["none", "publickey"]);
    provider.disconnect().await.unwrap();

    let _ = std::fs::remove_dir_all(&home);
}

/// A session that got in through `none` while a private key was configured,
/// then the key file is replaced and the server stops accepting `none`.
/// Every later dial of that session (a pool worker's first use, a pool
/// worker connected outright, the primary's reconnect) ends in
/// `TargetChanged`, and the server is never shown a public key: not the
/// replacement, which it would accept, and not the original.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn redial_after_a_none_login_never_offers_the_replaced_key() {
    let _env = ENV_LOCK.lock().await;
    let home = tempfile::tempdir().unwrap();
    let _home_env = ScopedEnv::set("HOME", home.path());

    let original = random_key();
    let replacement = random_key();
    let key_path = home.path().join("id_ed25519");
    write_key(&key_path, &original);

    // The server would let the replacement key in: only the client's pin
    // stands between the replaced file and a login as that key's owner.
    let (port, policy) = start_server(Policy {
        accept_none: true,
        key: Some(replacement.public_key().clone()),
        ..Policy::default()
    })
    .await;
    let mut provider = connect(port, Credential::Key(&key_path))
        .await
        .expect("in through none");
    assert_eq!(policy.attempts(), ["none"]);
    assert!(policy.offered_keys().is_empty());

    write_key(&key_path, &replacement);
    policy.refuse_none.store(true, Ordering::SeqCst);

    // A pool worker dials lazily, on its first operation.
    let mut lazy_worker = provider.clone_for_transfer().expect("pool worker");
    match tokio::time::timeout(Duration::from_secs(15), lazy_worker.list("/"))
        .await
        .expect("worker dial timed out")
    {
        Err(ProviderError::TargetChanged) => {}
        Err(other) => panic!("expected TargetChanged from the lazy worker, got {other:?}"),
        Ok(_) => panic!("a pool worker got in after the server stopped accepting none"),
    }

    // A pool worker connected outright.
    let mut worker = provider.clone_for_transfer().expect("pool worker");
    match tokio::time::timeout(Duration::from_secs(15), worker.connect())
        .await
        .expect("worker connect timed out")
    {
        Err(ProviderError::TargetChanged) => {}
        Err(other) => panic!("expected TargetChanged from the worker, got {other:?}"),
        Ok(()) => panic!("a pool worker got in after the server stopped accepting none"),
    }

    // The primary's own reconnect.
    match tokio::time::timeout(Duration::from_secs(15), provider.connect())
        .await
        .expect("reconnect timed out")
    {
        Err(ProviderError::TargetChanged) => {}
        Err(other) => panic!("expected TargetChanged from the reconnect, got {other:?}"),
        Ok(()) => panic!("the reconnect got in after the server stopped accepting none"),
    }

    // Each of the three dials asked for `none` and nothing else: zero
    // publickey attempts after the switch.
    assert_eq!(policy.attempts(), ["none", "none", "none", "none"]);
    assert_eq!(
        policy
            .attempts()
            .iter()
            .filter(|method| **method == "publickey")
            .count(),
        0
    );
    assert!(
        policy.offered_keys().is_empty(),
        "a public key was offered after a none login: {:?}",
        policy.offered_keys()
    );
}

/// The SCP provider dials through the same login. This server runs no
/// command, so the SCP session itself never starts; the login before it is
/// accepted through `none`, and that is what a second dial is held to.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scp_redial_after_a_none_login_never_offers_the_replaced_key() {
    use ftp_client_gui_lib::providers::scp::provider::ScpProvider;

    let _env = ENV_LOCK.lock().await;
    let home = tempfile::tempdir().unwrap();
    let _home_env = ScopedEnv::set("HOME", home.path());

    let original = random_key();
    let replacement = random_key();
    let key_path = home.path().join("id_ed25519");
    write_key(&key_path, &original);

    let (port, policy) = start_server(Policy {
        accept_none: true,
        key: Some(replacement.public_key().clone()),
        ..Policy::default()
    })
    .await;
    let mut provider = ScpProvider::new(SftpConfig {
        host: "127.0.0.1".to_string(),
        port,
        username: "root".to_string(),
        password: None,
        private_key_path: Some(key_path.to_string_lossy().into_owned()),
        key_passphrase: None,
        initial_path: Some("/".to_string()),
        timeout_secs: 5,
        trust_unknown_hosts: true,
    });
    let first = tokio::time::timeout(Duration::from_secs(15), provider.connect())
        .await
        .expect("first connect timed out");
    assert!(
        !matches!(
            first,
            Ok(()) | Err(ProviderError::TargetChanged | ProviderError::AuthenticationFailed(_))
        ),
        "the first login is accepted and only the SCP probe fails, got {first:?}"
    );
    assert_eq!(policy.attempts(), ["none"]);

    write_key(&key_path, &replacement);
    policy.refuse_none.store(true, Ordering::SeqCst);

    match tokio::time::timeout(Duration::from_secs(15), provider.connect())
        .await
        .expect("second connect timed out")
    {
        Err(ProviderError::TargetChanged) => {}
        Err(other) => panic!("expected TargetChanged from the SCP re-dial, got {other:?}"),
        Ok(()) => panic!("the SCP re-dial got in after the server stopped accepting none"),
    }
    assert_eq!(policy.attempts(), ["none", "none"]);
    assert!(
        policy.offered_keys().is_empty(),
        "a public key was offered after a none login: {:?}",
        policy.offered_keys()
    );
}

/// P1: a key the server refused never becomes the session's pin. Key A is
/// refused; its file is replaced by key B, which the server accepts; the SAME
/// provider then gets in with B. SFTP, and the SCP provider on the same login
/// (its SCP probe fails on this server afterwards, its login does not).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_key_is_not_pinned_and_its_replacement_gets_in() {
    use ftp_client_gui_lib::providers::scp::provider::ScpProvider;

    let _env = ENV_LOCK.lock().await;
    let home = tempfile::tempdir().unwrap();
    let _home_env = ScopedEnv::set("HOME", home.path());

    let refused = random_key();
    let accepted = random_key();
    let key_path = home.path().join("id_ed25519");
    let (port, policy) = start_server(Policy {
        key: Some(accepted.public_key().clone()),
        ..Policy::default()
    })
    .await;
    let config = || SftpConfig {
        host: "127.0.0.1".to_string(),
        port,
        username: "root".to_string(),
        password: None,
        private_key_path: Some(key_path.to_string_lossy().into_owned()),
        key_passphrase: None,
        initial_path: Some("/".to_string()),
        timeout_secs: 5,
        trust_unknown_hosts: true,
    };
    async fn dial<F: std::future::Future>(connect: F) -> F::Output {
        tokio::time::timeout(Duration::from_secs(15), connect)
            .await
            .expect("connect timed out")
    }

    write_key(&key_path, &refused);
    let mut sftp = SftpProvider::new(config());
    let first = dial(sftp.connect()).await;
    assert!(
        matches!(first, Err(ProviderError::AuthenticationFailed(_))),
        "key A is refused: {first:?}"
    );
    write_key(&key_path, &accepted);
    dial(sftp.connect())
        .await
        .expect("the same SFTP provider gets in with the replacement key");
    assert!(
        policy
            .offered_keys()
            .contains(&fingerprint(accepted.public_key())),
        "{:?}",
        policy.offered_keys()
    );

    write_key(&key_path, &refused);
    let mut scp = ScpProvider::new(config());
    let first = dial(scp.connect()).await;
    assert!(
        matches!(first, Err(ProviderError::AuthenticationFailed(_))),
        "key A is refused over SCP: {first:?}"
    );
    write_key(&key_path, &accepted);
    let offers_before = policy
        .offered_keys()
        .iter()
        .filter(|offered| **offered == fingerprint(accepted.public_key()))
        .count();
    let second = dial(scp.connect()).await;
    assert!(
        !matches!(
            second,
            Err(ProviderError::AuthenticationFailed(_) | ProviderError::TargetChanged)
        ),
        "the same SCP provider gets in with the replacement key: {second:?}"
    );
    let offers_after = policy
        .offered_keys()
        .iter()
        .filter(|offered| **offered == fingerprint(accepted.public_key()))
        .count();
    assert!(offers_after > offers_before, "{:?}", policy.offered_keys());
}

/// A session that logged in with a key, whose key file is then replaced by
/// another valid key. The delta transport it hands out signs with the login
/// key: the server sees `publickey` requests for that key only, never the
/// replacement and never `none`. With the classic setting, where only the
/// `rsync` binary could serve (it runs `ssh -i <path>`), there is no delta
/// transport at all. A session that logged in through `none` has none
/// either.
#[cfg(feature = "aerorsync")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delta_transport_of_a_key_session_never_offers_the_replaced_key() {
    use ftp_client_gui_lib::settings::{set_native_rsync_mode, NativeRsyncMode};

    let _env = ENV_LOCK.lock().await;
    let home = tempfile::tempdir().unwrap();
    let _home_env = ScopedEnv::set("HOME", home.path());
    // The native rsync setting is read from the config root: keep the real
    // one out of reach on Linux (elsewhere it already follows HOME).
    let _config_env = ScopedEnv::set("XDG_CONFIG_HOME", home.path().join("config"));

    let login_key = random_key();
    let replacement = random_key();
    let key_path = home.path().join("id_ed25519");
    write_key(&key_path, &login_key);

    // `none` is refused for the login, so the key is what gets in; the
    // server accepts no other key.
    let (port, policy) = start_server(Policy {
        accept_none: true,
        key: Some(login_key.public_key().clone()),
        ..Policy::default()
    })
    .await;
    policy.refuse_none.store(true, Ordering::SeqCst);
    let mut provider = connect(port, Credential::Key(&key_path))
        .await
        .expect("in through the key");
    assert_eq!(policy.attempts(), ["none", "publickey"]);

    write_key(&key_path, &replacement);
    // From here on the server would let anyone in through `none`: a delta
    // login that sent it would succeed and be recorded.
    policy.refuse_none.store(false, Ordering::SeqCst);

    // Classic: the binary is the only candidate and it is not handed out.
    set_native_rsync_mode(NativeRsyncMode::Classic).unwrap();
    assert!(provider.delta_transport().is_none());

    for mode in [NativeRsyncMode::Auto, NativeRsyncMode::Native] {
        set_native_rsync_mode(mode).unwrap();
        let transport = provider
            .delta_transport()
            .expect("a key session keeps native delta");
        assert_ne!(transport.name(), "rsync-binary-over-ssh");
        // The probe is the transport's own SSH login followed by
        // `rsync --version`, which this server refuses to run.
        let probe = tokio::time::timeout(Duration::from_secs(30), transport.probe_remote())
            .await
            .expect("probe timed out");
        assert!(probe.is_err(), "this server runs no rsync");
    }

    let login_fingerprint = fingerprint(login_key.public_key());
    let offered = policy.offered_keys();
    assert!(
        offered.iter().all(|f| *f == login_fingerprint),
        "only the login key may be offered, got {offered:?}"
    );
    assert!(!offered.contains(&fingerprint(replacement.public_key())));
    // One signed login for the session and one per probe, and no `none`
    // after the session's own.
    assert_eq!(
        policy.attempts(),
        ["none", "publickey", "publickey", "publickey"]
    );
    provider.disconnect().await.unwrap();

    // A session that logged in through `none` gets no delta transport,
    // whatever the setting.
    let (port, policy) = start_server(Policy {
        accept_none: true,
        key: Some(replacement.public_key().clone()),
        ..Policy::default()
    })
    .await;
    let mut provider = connect(port, Credential::Key(&key_path))
        .await
        .expect("in through none");
    assert_eq!(policy.attempts(), ["none"]);
    for mode in [
        NativeRsyncMode::Classic,
        NativeRsyncMode::Auto,
        NativeRsyncMode::Native,
    ] {
        set_native_rsync_mode(mode).unwrap();
        assert!(provider.delta_transport().is_none());
    }
    assert!(policy.offered_keys().is_empty());
    provider.disconnect().await.unwrap();
}

/// A session that logged in with a password, after which the server starts
/// accepting `none` (an account whose root depends on how it logged in).
/// Every delta login repeats the password: the server sees `password`
/// requests only, never `none` and never a key. With the classic setting,
/// where only the `rsync` binary could serve (OpenSSH always sends `none`
/// first), there is no delta transport: the sync uses plain SFTP.
#[cfg(feature = "aerorsync")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delta_transport_of_a_password_session_never_sends_none() {
    use ftp_client_gui_lib::settings::{set_native_rsync_mode, NativeRsyncMode};

    let _env = ENV_LOCK.lock().await;
    let home = tempfile::tempdir().unwrap();
    let _home_env = ScopedEnv::set("HOME", home.path());
    let _config_env = ScopedEnv::set("XDG_CONFIG_HOME", home.path().join("config"));

    let (port, policy) = start_server(Policy {
        accept_none: true,
        password: Some("session-password"),
        ..Policy::default()
    })
    .await;
    policy.refuse_none.store(true, Ordering::SeqCst);
    let mut provider = connect(port, Credential::Password("session-password"))
        .await
        .expect("in through the password");
    assert_eq!(policy.attempts(), ["none", "password"]);

    // From here on `none` would get in, and would be recorded.
    policy.refuse_none.store(false, Ordering::SeqCst);

    // Classic: only the `rsync` binary could serve, and it cannot hold the
    // pin (it would send `none` first): no delta transport, so the sync
    // uses plain SFTP, the safe outcome, and nothing reaches the server.
    set_native_rsync_mode(NativeRsyncMode::Classic).unwrap();
    assert!(provider.delta_transport().is_none());
    assert_eq!(policy.attempts(), ["none", "password"]);

    let mut logins = 0;
    for mode in [NativeRsyncMode::Auto, NativeRsyncMode::Native] {
        set_native_rsync_mode(mode).unwrap();
        let transport = provider
            .delta_transport()
            .expect("a password session keeps native delta");
        assert_ne!(transport.name(), "rsync-binary-over-ssh");
        let probe = tokio::time::timeout(Duration::from_secs(30), transport.probe_remote())
            .await
            .expect("probe timed out");
        assert!(probe.is_err(), "this server runs no rsync");
        logins += 1;
    }

    let mut expected = vec!["none", "password"];
    expected.extend(std::iter::repeat_n("password", logins));
    assert_eq!(
        policy.attempts(),
        expected,
        "no `none` after the session's own"
    );
    assert!(policy.offered_keys().is_empty());
    provider.disconnect().await.unwrap();
}

/// A server that refuses the `password` method and takes the password as the
/// answer to a keyboard-interactive prompt. The session gets in that way,
/// and records it: the delta login can only repeat `password`, so no delta
/// transport is handed out in any mode (the sync uses plain SFTP), and none
/// of the delta logins that could fail is attempted.
#[cfg(feature = "aerorsync")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_keyboard_interactive_password_session_gets_no_delta_transport() {
    use ftp_client_gui_lib::settings::{set_native_rsync_mode, NativeRsyncMode};

    let _env = ENV_LOCK.lock().await;
    let home = tempfile::tempdir().unwrap();
    let _home_env = ScopedEnv::set("HOME", home.path());
    let _config_env = ScopedEnv::set("XDG_CONFIG_HOME", home.path().join("config"));

    let (port, policy) = start_server(Policy {
        keyboard_interactive: Some("session-password"),
        ..Policy::default()
    })
    .await;
    let mut provider = connect(port, Credential::Password("session-password"))
        .await
        .expect("in through keyboard-interactive");
    assert_eq!(
        policy.attempts(),
        ["none", "password", "keyboard-interactive"]
    );
    for mode in [
        NativeRsyncMode::Classic,
        NativeRsyncMode::Auto,
        NativeRsyncMode::Native,
    ] {
        set_native_rsync_mode(mode).unwrap();
        assert!(provider.delta_transport().is_none(), "{mode:?}");
    }
    assert_eq!(
        policy.attempts(),
        ["none", "password", "keyboard-interactive"],
        "no delta login was attempted"
    );
    provider.disconnect().await.unwrap();
}

/// D2: a session that logged in with the `password` method; the server then
/// takes the password only as a keyboard-interactive answer. Every redial (a
/// lazy pool worker, a worker connected outright, the primary's reconnect)
/// would get in another way than the session did: refused as a changed
/// target, never accepted under the method recorded first.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_redial_that_gets_in_another_way_is_a_changed_target() {
    let _env = ENV_LOCK.lock().await;
    let home = tempfile::tempdir().unwrap();
    let _home_env = ScopedEnv::set("HOME", home.path());
    let (port, policy) = start_server(Policy {
        password: Some("session-password"),
        keyboard_interactive: Some("session-password"),
        ..Policy::default()
    })
    .await;
    let mut provider = connect(port, Credential::Password("session-password"))
        .await
        .expect("in through the password");
    assert_eq!(policy.attempts(), ["none", "password"]);
    policy.refuse_password.store(true, Ordering::SeqCst);

    let mut lazy_worker = provider.clone_for_transfer().expect("pool worker");
    match tokio::time::timeout(Duration::from_secs(15), lazy_worker.list("/"))
        .await
        .expect("worker dial timed out")
    {
        Err(ProviderError::TargetChanged) => {}
        other => panic!(
            "expected TargetChanged from the lazy worker, got {:?}",
            other.map(|_| ())
        ),
    }
    let mut worker = provider.clone_for_transfer().expect("pool worker");
    match tokio::time::timeout(Duration::from_secs(15), worker.connect())
        .await
        .expect("worker connect timed out")
    {
        Err(ProviderError::TargetChanged) => {}
        other => panic!("expected TargetChanged from the worker, got {other:?}"),
    }
    // The server did let each redial in through keyboard-interactive: the
    // refusal is the method check, not a failed login.
    assert!(
        policy
            .attempts()
            .iter()
            .filter(|method| **method == "keyboard-interactive")
            .count()
            >= 2
    );
    provider.disconnect().await.unwrap();
}
