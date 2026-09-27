//! `SftpProvider::rename` against a server that refuses a taken name the
//! way OpenSSH's sftp-server does.
//!
//! SFTP v3 has no "already exists" status: OpenSSH renames a file with
//! link(2) then unlink(2), and the EEXIST of the link reaches the client as
//! the bare SSH_FX_FAILURE, which says nothing more. The provider turns that
//! refusal into AlreadyExists (the CLI's exit 9) only when the destination
//! is there, and does not send a rename onto the source's own path, which
//! OpenSSH refuses the same way although it is a no-op everywhere else.
//!
//! The server below is russh plus a minimal hand-rolled SFTP v3 packet loop
//! (the shape of `tests/sftp_size_hint.rs`): INIT, REALPATH, STAT/LSTAT and
//! RENAME, and OPENDIR/READDIR/CLOSE, over a set of paths, every RENAME
//! counted. A case-insensitive server matches names whatever their case and
//! refuses a rename that changes only the case; a case-sensitive one holds
//! both spellings.

// Unix only, for the reason `tests/sftp_size_hint.rs` gives: the guard
// against writing into a real `known_hosts` is a redirected `HOME`, which
// no environment variable redirects on Windows.
#![cfg(unix)]

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use ftp_client_gui_lib::providers::sftp::SftpProvider;
use ftp_client_gui_lib::providers::types::SftpConfig;
use ftp_client_gui_lib::providers::{ProviderError, StorageProvider};
use russh::server::{Auth, ChannelOpenHandle, Handler, Msg, Server, Session};
use russh::{Channel, ChannelId};

const SSH_FXP_INIT: u8 = 1;
const SSH_FXP_VERSION: u8 = 2;
const SSH_FXP_CLOSE: u8 = 4;
const SSH_FXP_LSTAT: u8 = 7;
const SSH_FXP_OPENDIR: u8 = 11;
const SSH_FXP_READDIR: u8 = 12;
const SSH_FXP_REALPATH: u8 = 16;
const SSH_FXP_STAT: u8 = 17;
const SSH_FXP_RENAME: u8 = 18;
const SSH_FXP_STATUS: u8 = 101;
const SSH_FXP_HANDLE: u8 = 102;
const SSH_FXP_NAME: u8 = 104;
const SSH_FXP_ATTRS: u8 = 105;

const SSH_FX_OK: u32 = 0;
const SSH_FX_EOF: u32 = 1;
const SSH_FX_NO_SUCH_FILE: u32 = 2;
const SSH_FX_PERMISSION_DENIED: u32 = 3;
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

fn rstr(data: &[u8], pos: &mut usize) -> Option<String> {
    let len = r32(data, pos)? as usize;
    let bytes = data.get(*pos..*pos + len)?;
    *pos += len;
    String::from_utf8(bytes.to_vec()).ok()
}

fn status(id: u32, code: u32, msg: &str) -> Vec<u8> {
    let mut r = vec![SSH_FXP_STATUS];
    w32(&mut r, id);
    w32(&mut r, code);
    wstr(&mut r, msg.as_bytes());
    wstr(&mut r, b"");
    r
}

/// What the server holds, and how many RENAMEs reached it. A
/// case-insensitive server matches names whatever their case and refuses a
/// rename that changes only the case; a case-sensitive one holds both
/// spellings as two files.
#[derive(Default)]
struct Store {
    paths: Mutex<HashSet<String>>,
    renames: AtomicU32,
    case_insensitive: bool,
    /// Refuses OPENDIR, as a server that allows no listing there.
    refuse_opendir: bool,
}

struct RenameServer {
    store: Arc<Store>,
}

struct RenameHandler {
    store: Arc<Store>,
    buf: Vec<u8>,
    /// Open directory handles: the folder, and whether its entries went out.
    dirs: HashMap<String, (String, bool)>,
}

impl RenameHandler {
    fn process(&mut self, data: &[u8]) -> Option<Vec<u8>> {
        let ptype = *data.first()?;
        let mut pos = 1usize;
        if ptype == SSH_FXP_INIT {
            let mut r = vec![SSH_FXP_VERSION];
            w32(&mut r, 3);
            return Some(r);
        }
        let id = r32(data, &mut pos)?;
        let reply = match ptype {
            SSH_FXP_REALPATH => {
                let path = rstr(data, &mut pos).unwrap_or_else(|| "/".into());
                let mut r = vec![SSH_FXP_NAME];
                w32(&mut r, id);
                w32(&mut r, 1);
                wstr(&mut r, path.as_bytes());
                wstr(&mut r, path.as_bytes());
                w32(&mut r, 0);
                r
            }
            SSH_FXP_STAT | SSH_FXP_LSTAT => {
                let path = rstr(data, &mut pos).unwrap_or_default();
                let insensitive = self.store.case_insensitive;
                let found = self
                    .store
                    .paths
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|p| *p == path || (insensitive && p.eq_ignore_ascii_case(&path)));
                if found {
                    let mut r = vec![SSH_FXP_ATTRS];
                    w32(&mut r, id);
                    w32(&mut r, 0);
                    r
                } else {
                    status(id, SSH_FX_NO_SUCH_FILE, "No such file")
                }
            }
            SSH_FXP_RENAME => {
                self.store.renames.fetch_add(1, Ordering::SeqCst);
                let from = rstr(data, &mut pos).unwrap_or_default();
                let to = rstr(data, &mut pos).unwrap_or_default();
                let mut paths = self.store.paths.lock().unwrap();
                // A case-insensitive server finds the source whatever the
                // spelling it is asked for.
                let from = if self.store.case_insensitive {
                    paths
                        .iter()
                        .find(|p| p.eq_ignore_ascii_case(&from))
                        .cloned()
                        .unwrap_or(from)
                } else {
                    from
                };
                if !paths.contains(&from) {
                    status(id, SSH_FX_NO_SUCH_FILE, "No such file")
                } else if self.store.case_insensitive
                    && to != from
                    && to.eq_ignore_ascii_case(&from)
                {
                    // A server that refuses a rename changing only the case.
                    status(id, SSH_FX_FAILURE, "Failure")
                } else if to.starts_with("/locked/") {
                    status(id, SSH_FX_PERMISSION_DENIED, "Permission denied")
                } else if paths.contains(&to) || to.starts_with("/full/") {
                    // link(2) fails with EEXIST (or the disk is full), which
                    // OpenSSH reports as the bare SSH_FX_FAILURE.
                    status(id, SSH_FX_FAILURE, "Failure")
                } else {
                    paths.remove(&from);
                    paths.insert(to);
                    status(id, SSH_FX_OK, "")
                }
            }
            SSH_FXP_OPENDIR if self.store.refuse_opendir => {
                status(id, SSH_FX_PERMISSION_DENIED, "Permission denied")
            }
            SSH_FXP_OPENDIR => {
                let dir = rstr(data, &mut pos).unwrap_or_default();
                let handle = format!("d{}", self.dirs.len());
                self.dirs.insert(handle.clone(), (dir, false));
                let mut r = vec![SSH_FXP_HANDLE];
                w32(&mut r, id);
                wstr(&mut r, handle.as_bytes());
                r
            }
            SSH_FXP_READDIR => {
                let handle = rstr(data, &mut pos).unwrap_or_default();
                match self.dirs.get_mut(&handle) {
                    Some((dir, served)) if !*served => {
                        *served = true;
                        let prefix = format!("{}/", dir.trim_end_matches('/'));
                        let names: Vec<String> = self
                            .store
                            .paths
                            .lock()
                            .unwrap()
                            .iter()
                            .filter_map(|p| p.strip_prefix(&prefix))
                            .filter(|name| !name.contains('/'))
                            .map(str::to_string)
                            .collect();
                        let mut r = vec![SSH_FXP_NAME];
                        w32(&mut r, id);
                        w32(&mut r, names.len() as u32);
                        for name in names {
                            wstr(&mut r, name.as_bytes());
                            wstr(&mut r, name.as_bytes());
                            w32(&mut r, 0);
                        }
                        r
                    }
                    _ => status(id, SSH_FX_EOF, "eof"),
                }
            }
            SSH_FXP_CLOSE => {
                let handle = rstr(data, &mut pos).unwrap_or_default();
                self.dirs.remove(&handle);
                status(id, SSH_FX_OK, "")
            }
            _ => status(id, SSH_FX_FAILURE, "unsupported"),
        };
        Some(reply)
    }
}

impl Server for RenameServer {
    type Handler = RenameHandler;
    fn new_client(&mut self, _: Option<SocketAddr>) -> RenameHandler {
        RenameHandler {
            store: self.store.clone(),
            buf: Vec::new(),
            dirs: HashMap::new(),
        }
    }
}

impl Handler for RenameHandler {
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

/// Start the server on loopback holding `paths`; returns its port and store.
async fn start_server(paths: &[&str], case_insensitive: bool) -> (u16, Arc<Store>) {
    start_server_with(
        paths,
        Store {
            case_insensitive,
            ..Store::default()
        },
    )
    .await
}

/// [`start_server`] with a given store (its paths are added).
async fn start_server_with(paths: &[&str], store: Store) -> (u16, Arc<Store>) {
    let store = Arc::new(store);
    store
        .paths
        .lock()
        .unwrap()
        .extend(paths.iter().map(|p| p.to_string()));
    let server = RenameServer {
        store: store.clone(),
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
    (port, store)
}

async fn connect(port: u16) -> SftpProvider {
    let mut p = SftpProvider::new(SftpConfig {
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
    tokio::time::timeout(std::time::Duration::from_secs(15), p.connect())
        .await
        .expect("connect timed out")
        .expect("connect to fake SFTP");
    p
}

/// One test, sequential phases: HOME is process-wide.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rename_reports_a_taken_name_and_skips_its_own_path() {
    let home = std::env::temp_dir().join(format!("sftp-rename-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    // Safety: test process only; guards the real known_hosts from the
    // trust_unknown_hosts accept-and-save path.
    unsafe { std::env::set_var("HOME", &home) };

    let (port, store) = start_server(&["/a.txt", "/b.txt"], true).await;
    let mut provider = connect(port).await;

    // 1. Onto an existing file OpenSSH answers the bare SSH_FX_FAILURE,
    // which became a generic server error. With the destination there it is
    // AlreadyExists.
    let outcome = provider.rename("/a.txt", "/b.txt").await;
    assert!(
        matches!(outcome, Err(ProviderError::AlreadyExists(_))),
        "{outcome:?}"
    );

    // 2. The same bare failure with nothing at the destination (here: a full
    // disk) is the server's refusal, not a taken name.
    let outcome = provider.rename("/a.txt", "/full/c.txt").await;
    assert!(
        outcome.is_err() && !matches!(outcome, Err(ProviderError::AlreadyExists(_))),
        "{outcome:?}"
    );

    // 3. Any other refusal is passed on as it came.
    let outcome = provider.rename("/a.txt", "/locked/c.txt").await;
    assert!(
        outcome.is_err() && !matches!(outcome, Err(ProviderError::AlreadyExists(_))),
        "{outcome:?}"
    );

    // 4. Onto its own path OpenSSH refuses the same way; it is a no-op
    // everywhere else, and nothing is sent.
    let before = store.renames.load(Ordering::SeqCst);
    provider
        .rename("/a.txt", "/a.txt")
        .await
        .expect("a rename onto its own path is a no-op");
    assert_eq!(store.renames.load(Ordering::SeqCst), before);

    // 5. A rename that only changes the case, refused by a case-insensitive
    // server with the bare failure, is that refusal and not a taken name:
    // the look finds the source itself under the new spelling.
    let outcome = provider.rename("/a.txt", "/A.txt").await;
    assert!(
        outcome.is_err() && !matches!(outcome, Err(ProviderError::AlreadyExists(_))),
        "{outcome:?}"
    );

    // 6. A free name still renames.
    provider
        .rename("/a.txt", "/c.txt")
        .await
        .expect("free name");
    assert!(store.paths.lock().unwrap().contains("/c.txt"));

    provider.disconnect().await.ok();

    // 7. On a case-sensitive server `x.txt` and `X.txt` are two files, and a
    // rename of one onto the other is refused with the bare failure (link(2)
    // meets EEXIST): the parent listing shows `X.txt`, so it is a taken name.
    let (port, _) = start_server(&["/x.txt", "/X.txt"], false).await;
    let mut provider = connect(port).await;
    let outcome = provider.rename("/x.txt", "/X.txt").await;
    assert!(
        matches!(outcome, Err(ProviderError::AlreadyExists(_))),
        "{outcome:?}"
    );
    provider.disconnect().await.ok();

    // 8. A case-insensitive server that stores `A.txt`, asked to rename
    // `a.txt` to `A.txt`, refuses: the listing holds one entry, the source
    // itself, so this is no taken name.
    let (port, _) = start_server(&["/A.txt"], true).await;
    let mut provider = connect(port).await;
    let outcome = provider.rename("/a.txt", "/A.txt").await;
    assert!(
        outcome.is_err() && !matches!(outcome, Err(ProviderError::AlreadyExists(_))),
        "{outcome:?}"
    );
    provider.disconnect().await.ok();

    // 9. A listing the server refuses leaves its refusal as it came: the
    // look cannot tell, and does not guess a taken name.
    let (port, _) = start_server_with(
        &["/x.txt", "/X.txt"],
        Store {
            refuse_opendir: true,
            ..Store::default()
        },
    )
    .await;
    let mut provider = connect(port).await;
    let outcome = provider.rename("/x.txt", "/X.txt").await;
    assert!(
        outcome.is_err() && !matches!(outcome, Err(ProviderError::AlreadyExists(_))),
        "{outcome:?}"
    );
    provider.disconnect().await.ok();

    // 10. On a case-sensitive server `/Dir` and `/dir` are two folders: a
    // rename of `/Dir/a.txt` onto the `/dir/A.txt` there is a taken name,
    // although the source's spelling is in another folder.
    let (port, _) = start_server(&["/Dir/a.txt", "/dir/A.txt"], false).await;
    let mut provider = connect(port).await;
    let outcome = provider.rename("/Dir/a.txt", "/dir/A.txt").await;
    assert!(
        matches!(outcome, Err(ProviderError::AlreadyExists(_))),
        "{outcome:?}"
    );
    provider.disconnect().await.ok();
    std::fs::remove_dir_all(&home).ok();
}
