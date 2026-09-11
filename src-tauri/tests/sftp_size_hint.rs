//! The per-file STAT/OPEN round trips of an SFTP download, overlapped.
//!
//! `SftpProvider::download` used to await SSH_FXP_STAT, then SSH_FXP_OPEN,
//! then the read(s), then CLOSE: four awaited exchanges per file. The batch
//! executor supplies a hint; hints of at most one buffer allow speculative
//! OPEN alongside STAT. The fresh STAT still drives termination, throttling,
//! progress and selection of any fast path. The hint never supplies an exact
//! size. A speculative handle is closed before a different path is entered.
//!
//! The server below is russh plus a minimal hand-rolled SFTP v3 packet loop
//! (the same shape as `serve sftp` in the CLI). Its STAT replies are DELAYED
//! by 250 ms and it records whether OPEN arrived while a STAT reply was
//! still pending: the overlap is asserted on the wire, timing-free for the
//! client. No Docker fixture.
//!
//! Covered:
//! - unhinted download: OPEN only after the STAT reply (control: the
//!   detector is live);
//! - hinted small download: OPEN during the STAT window (fail-first: the old
//!   sequential code cannot produce this), byte-exact payload;
//! - a stale SMALL hint still delivers the whole file (termination follows
//!   the fresh STAT, not the hint);
//! - a stale LARGE hint takes the sequential path and still delivers the
//!   exact bytes.

// Unix only, and the reason is the redirected HOME rather than portability
// taste: the guard against writing into a real `known_hosts` is a redirected
// `HOME`, and on Windows `russh` resolves that file through the user profile
// API, which no environment variable redirects. Left compiling there, helper
// items in this file are unused and `cargo check --all-targets` warns
// dead_code. The honest fix is to not compile the module rather than to
// pretend the guard holds.
#![cfg(unix)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use ftp_client_gui_lib::providers::sftp::SftpProvider;
use ftp_client_gui_lib::providers::types::SftpConfig;
use ftp_client_gui_lib::providers::StorageProvider;
use russh::server::{Auth, ChannelOpenHandle, Handler, Msg, Server, Session};
use russh::{Channel, ChannelId};

const SSH_FXP_INIT: u8 = 1;
const SSH_FXP_VERSION: u8 = 2;
const SSH_FXP_OPEN: u8 = 3;
const SSH_FXP_CLOSE: u8 = 4;
const SSH_FXP_READ: u8 = 5;
const SSH_FXP_WRITE: u8 = 6;
const SSH_FXF_CREAT: u32 = 0x00000008;
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

/// How long a STAT reply is held back. Long enough that a sequential
/// STAT-then-OPEN client cannot get its OPEN in under the wire, short
/// enough to keep the test fast.
const STAT_DELAY: std::time::Duration = std::time::Duration::from_millis(250);

#[derive(Default)]
struct WireCounts {
    stat: AtomicU32,
    handles: AtomicU32,
    reads: AtomicU32,
    /// Set when an OPEN arrives while a STAT reply is still pending:
    /// the signature of a client that pipelines the two requests.
    open_during_stat_window: AtomicBool,
    /// Number of STAT replies currently delayed in flight.
    stat_pending: AtomicU32,
    /// Milliseconds a CLOSE reply is held back, with the handle still counted
    /// until it leaves. Zero replies at once. Non-zero is what separates a
    /// client that awaits its close from one that only queues it: the first
    /// returns with the count at zero, the second returns before the server
    /// has let go of the handle.
    close_delay_ms: AtomicU32,
    /// Milliseconds an OPEN reply is held back. Zero replies at once.
    open_delay_ms: AtomicU32,
    /// This numbered READ (1-based) fails; 0 never fails. Later READs still
    /// succeed, so sibling readers can be left in-flight.
    fail_read_after: AtomicU32,
    /// Milliseconds a successful READ reply is held back. Zero replies at
    /// once. Combined with `fail_read_after`, this keeps sibling readers
    /// in-flight so dropping them cannot sneak a close in before the assert.
    read_delay_ms: AtomicU32,
}

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
    let v = u32::from_be_bytes(data.get(*pos..*pos + 4)?.try_into().ok()?);
    *pos += 4;
    Some(v)
}

fn r64(data: &[u8], pos: &mut usize) -> Option<u64> {
    let v = u64::from_be_bytes(data.get(*pos..*pos + 8)?.try_into().ok()?);
    *pos += 8;
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

struct TestSftpServer {
    files: Arc<HashMap<String, Vec<u8>>>,
    counts: Arc<WireCounts>,
}

struct TestSftpHandler {
    files: Arc<HashMap<String, Vec<u8>>>,
    counts: Arc<WireCounts>,
    buf: Vec<u8>,
    handles: HashMap<String, String>,
    next_handle: u32,
}

impl TestSftpHandler {
    /// Processes one packet. The reply is sent through `session`; a STAT
    /// reply is deferred by STAT_DELAY on a spawned task so a pipelined
    /// client's OPEN is observed while the reply is still pending.
    fn process(&mut self, data: &[u8], channel: ChannelId, session: &mut Session) {
        if data.is_empty() {
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
                let path = rstr(data, &mut pos).unwrap_or_else(|| "/".into());
                let mut r = vec![SSH_FXP_NAME];
                w32(&mut r, id);
                w32(&mut r, 1);
                wstr(&mut r, path.as_bytes());
                wstr(&mut r, path.as_bytes());
                w32(&mut r, 0); // empty attrs
                self.send(channel, r, session);
            }
            SSH_FXP_STAT | SSH_FXP_LSTAT => {
                self.counts.stat.fetch_add(1, Ordering::SeqCst);
                let Some(id) = r32(data, &mut pos) else {
                    return;
                };
                let path = rstr(data, &mut pos).unwrap_or_default();
                let reply = match self.files.get(&path).filter(|_| path != "/stat-denied.bin") {
                    Some(content) => {
                        let mut r = vec![SSH_FXP_ATTRS];
                        w32(&mut r, id);
                        w32(&mut r, 0x1); // size flag
                                          // `/under-size.bin` lies about size so the client opens
                                          // and then hits the in-loop cap of download_to_bytes_capped.
                        let size = if path == "/under-size.bin" {
                            1
                        } else {
                            content.len() as u64
                        };
                        w64(&mut r, size);
                        r
                    }
                    None => status(id, SSH_FX_NO_SUCH_FILE, "not found"),
                };
                // Deferred reply: the window in which a pipelined OPEN shows.
                self.counts.stat_pending.fetch_add(1, Ordering::SeqCst);
                let handle = session.handle();
                let counts = self.counts.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(STAT_DELAY).await;
                    let mut framed = Vec::with_capacity(4 + reply.len());
                    w32(&mut framed, reply.len() as u32);
                    framed.extend_from_slice(&reply);
                    counts.stat_pending.fetch_sub(1, Ordering::SeqCst);
                    let _ = handle.data(channel, framed).await;
                });
            }
            SSH_FXP_OPEN => {
                if self.counts.stat_pending.load(Ordering::SeqCst) > 0 {
                    self.counts
                        .open_during_stat_window
                        .store(true, Ordering::SeqCst);
                }
                let Some(id) = r32(data, &mut pos) else {
                    return;
                };
                let path = rstr(data, &mut pos).unwrap_or_default();
                let pflags = r32(data, &mut pos).unwrap_or(0);
                if !self.files.contains_key(&path) && (pflags & SSH_FXF_CREAT) == 0 {
                    self.send(
                        channel,
                        status(id, SSH_FX_NO_SUCH_FILE, "not found"),
                        session,
                    );
                    return;
                }
                self.next_handle += 1;
                let h = format!("h{}", self.next_handle);
                self.handles.insert(h.clone(), path);
                self.counts.handles.fetch_add(1, Ordering::SeqCst);
                let mut r = vec![SSH_FXP_HANDLE];
                w32(&mut r, id);
                wstr(&mut r, h.as_bytes());
                let delay = self.counts.open_delay_ms.load(Ordering::SeqCst);
                if delay == 0 {
                    self.send(channel, r, session);
                } else {
                    let session_handle = session.handle();
                    tokio::spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_millis(u64::from(delay)))
                            .await;
                        let mut framed = Vec::with_capacity(4 + r.len());
                        w32(&mut framed, r.len() as u32);
                        framed.extend_from_slice(&r);
                        let _ = session_handle.data(channel, framed).await;
                    });
                }
            }
            SSH_FXP_READ => {
                let n = self.counts.reads.fetch_add(1, Ordering::SeqCst) + 1;
                let fail_after = self.counts.fail_read_after.load(Ordering::SeqCst);
                let Some(id) = r32(data, &mut pos) else {
                    return;
                };
                if fail_after > 0 && n == fail_after {
                    self.send(channel, status(id, SSH_FX_FAILURE, "read refused"), session);
                    return;
                }
                let handle = rstr(data, &mut pos).unwrap_or_default();
                let Some(offset) = r64(data, &mut pos) else {
                    return;
                };
                let Some(len) = r32(data, &mut pos) else {
                    return;
                };
                let content = self.handles.get(&handle).and_then(|p| self.files.get(p));
                let Some(content) = content else {
                    self.send(channel, status(id, SSH_FX_FAILURE, "bad handle"), session);
                    return;
                };
                let offset = offset as usize;
                if offset >= content.len() {
                    self.send(channel, status(id, SSH_FX_EOF, "eof"), session);
                    return;
                }
                // Deliberately short DATA replies before EOF: the client must
                // continue reading even when the caller supplied a tiny hint.
                let end = (offset + (len as usize).min(32768)).min(content.len());
                let mut r = vec![SSH_FXP_DATA];
                w32(&mut r, id);
                wstr(&mut r, &content[offset..end]);
                let delay = self.counts.read_delay_ms.load(Ordering::SeqCst);
                if delay == 0 {
                    self.send(channel, r, session);
                } else {
                    let session_handle = session.handle();
                    tokio::spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_millis(u64::from(delay)))
                            .await;
                        let mut framed = Vec::with_capacity(4 + r.len());
                        w32(&mut framed, r.len() as u32);
                        framed.extend_from_slice(&r);
                        let _ = session_handle.data(channel, framed).await;
                    });
                }
            }
            SSH_FXP_WRITE => {
                // Present so an upload can open a handle and then fail the
                // write: that is the early exit that used to skip shutdown.
                let Some(id) = r32(data, &mut pos) else {
                    return;
                };
                self.send(
                    channel,
                    status(id, SSH_FX_FAILURE, "write refused"),
                    session,
                );
            }
            SSH_FXP_CLOSE => {
                let Some(id) = r32(data, &mut pos) else {
                    return;
                };
                let handle = rstr(data, &mut pos).unwrap_or_default();
                assert!(
                    self.handles.remove(&handle).is_some(),
                    "close a live handle"
                );
                let delay = self.counts.close_delay_ms.load(Ordering::SeqCst);
                if delay == 0 {
                    self.counts.handles.fetch_sub(1, Ordering::SeqCst);
                    self.send(channel, status(id, SSH_FX_OK, ""), session);
                } else {
                    // Deferred close: the handle stays counted until the reply leaves.
                    let session_handle = session.handle();
                    let counts = self.counts.clone();
                    let reply = status(id, SSH_FX_OK, "");
                    tokio::spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_millis(u64::from(delay)))
                            .await;
                        counts.handles.fetch_sub(1, Ordering::SeqCst);
                        let mut framed = Vec::with_capacity(4 + reply.len());
                        w32(&mut framed, reply.len() as u32);
                        framed.extend_from_slice(&reply);
                        let _ = session_handle.data(channel, framed).await;
                    });
                }
            }
            _ => {
                let id = if data.len() >= 5 {
                    r32(data, &mut 1usize).unwrap_or(0)
                } else {
                    0
                };
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

impl Server for TestSftpServer {
    type Handler = TestSftpHandler;
    fn new_client(&mut self, _: Option<SocketAddr>) -> TestSftpHandler {
        TestSftpHandler {
            files: self.files.clone(),
            counts: self.counts.clone(),
            buf: Vec::new(),
            handles: HashMap::new(),
            next_handle: 0,
        }
    }
}

impl Handler for TestSftpHandler {
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

/// Start the fake on loopback; returns (port, counts). The host key is
/// ephemeral, which is why the test client runs with trust_unknown_hosts and
/// a scratch HOME (so the accept cannot touch the real known_hosts).
async fn start_server(files: HashMap<String, Vec<u8>>) -> (u16, Arc<WireCounts>) {
    let counts = Arc::new(WireCounts::default());
    let server = TestSftpServer {
        files: Arc::new(files),
        counts: counts.clone(),
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
    (port, counts)
}

fn test_config(port: u16) -> SftpConfig {
    SftpConfig {
        host: "127.0.0.1".to_string(),
        port,
        username: "testuser".to_string(),
        password: Some(secrecy::SecretString::from("testpass".to_string())),
        private_key_path: None,
        key_passphrase: None,
        initial_path: Some("/".to_string()),
        timeout_secs: 30,
        trust_unknown_hosts: true,
    }
}

async fn connect(port: u16) -> SftpProvider {
    let mut p = SftpProvider::new(test_config(port));
    tokio::time::timeout(std::time::Duration::from_secs(15), p.connect())
        .await
        .expect("connect timed out")
        .expect("connect to fake SFTP");
    p
}

/// HOME is process-wide (`known_hosts`). Tests that redirect it take this
/// lock so they cannot clobber each other.
static HOME_GUARD: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct ReadaheadCloseHarness {
    _home_guard: tokio::sync::MutexGuard<'static, ()>,
    home: PathBuf,
    provider: SftpProvider,
    counts: Arc<WireCounts>,
}

/// Delayed-CLOSE server, large.bin, read-ahead of 4. Shared by the three
/// leak tests so each path can fail independently of the others.
async fn readahead_close_harness(label: &str) -> ReadaheadCloseHarness {
    let home_guard = HOME_GUARD.lock().await;
    let home =
        std::env::temp_dir().join(format!("sftp-readahead-{}-{}", label, std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    unsafe { std::env::set_var("HOME", &home) };
    let large_payload: Vec<u8> = (0..524288u32).map(|i| (i % 239) as u8).collect();
    let mut files = HashMap::new();
    files.insert("/large.bin".to_string(), large_payload);
    let (port, counts) = start_server(files).await;
    counts.close_delay_ms.store(300, Ordering::SeqCst);
    let mut provider = connect(port).await;
    provider.set_sftp_readahead(Some(4));
    ReadaheadCloseHarness {
        _home_guard: home_guard,
        home,
        provider,
        counts,
    }
}

impl ReadaheadCloseHarness {
    async fn shutdown(mut self) {
        self.counts.close_delay_ms.store(0, Ordering::SeqCst);
        self.provider.disconnect().await.ok();
        std::fs::remove_dir_all(&self.home).ok();
    }
}

/// One test, sequential phases: HOME is process-wide and the detector flag
/// is reset between phases, so nothing can race itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hinted_download_overlaps_stat_and_open() {
    let _home_guard = HOME_GUARD.lock().await;
    let home = std::env::temp_dir().join(format!("kimi-sftp-hint-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    // Safety: test process only; guards the real known_hosts from the
    // trust_unknown_hosts accept-and-save path.
    unsafe { std::env::set_var("HOME", &home) };

    // 64 KiB: under two 256 KiB buffer chunks, so no read-ahead window can
    // engage and the serial loop is the path, exactly the bench cell's 4 KiB
    // shape.
    let payload: Vec<u8> = (0..64_000u32).map(|i| (i % 251) as u8).collect();
    let mut files = HashMap::new();
    files.insert("/file.bin".to_string(), payload.clone());
    let large_payload: Vec<u8> = (0..524288u32).map(|i| (i % 239) as u8).collect();
    files.insert("/large.bin".to_string(), large_payload.clone());
    files.insert("/stat-denied.bin".to_string(), payload.clone());
    files.insert("/empty.bin".to_string(), Vec::new());
    files.insert("/under-size.bin".to_string(), payload.clone());
    let (port, counts) = start_server(files).await;
    let mut provider = connect(port).await;

    // 1. Control: the unhinted path stays sequential. OPEN must NOT arrive
    // during the STAT window; this proves the detector can tell the two
    // orderings apart.
    counts
        .open_during_stat_window
        .store(false, Ordering::SeqCst);
    let stat_before = counts.stat.load(Ordering::SeqCst);
    let local = home.join("out-control.bin");
    provider
        .download("/file.bin", local.to_str().unwrap(), None)
        .await
        .expect("unhinted download");
    assert_eq!(std::fs::read(&local).unwrap(), payload);
    assert_eq!(counts.stat.load(Ordering::SeqCst), stat_before + 1);
    assert!(
        !counts.open_during_stat_window.load(Ordering::SeqCst),
        "unhinted download must stay sequential (control: the detector works)"
    );

    // 2. Hinted small download: STAT and OPEN fly together. On the old code
    // (sequential STAT then OPEN) this flag cannot become true.
    counts
        .open_during_stat_window
        .store(false, Ordering::SeqCst);
    let stat_before = counts.stat.load(Ordering::SeqCst);
    let local = home.join("out-hinted.bin");
    tokio::time::timeout(
        std::time::Duration::from_secs(15),
        provider.download_with_size_hint(
            "/file.bin",
            local.to_str().unwrap(),
            Some(payload.len() as u64),
            None,
        ),
    )
    .await
    .expect("hinted download timed out")
    .expect("hinted download");
    assert_eq!(std::fs::read(&local).unwrap(), payload);
    assert_eq!(
        counts.stat.load(Ordering::SeqCst),
        stat_before + 1,
        "the STAT still happens; it is overlapped, not skipped"
    );
    assert!(
        counts.open_during_stat_window.load(Ordering::SeqCst),
        "hinted small download: OPEN must fly while the STAT reply is pending"
    );

    // 3. A stale SMALL hint: the fresh STAT (overlapped) drives termination,
    // so the whole file arrives even though the hint says 4 KiB.
    let local = home.join("out-stale-small.bin");
    provider
        .download_with_size_hint("/file.bin", local.to_str().unwrap(), Some(4096), None)
        .await
        .expect("stale-small hinted download");
    assert_eq!(
        std::fs::read(&local).unwrap(),
        payload,
        "a hint smaller than the file must not truncate the download"
    );

    // 4. A stale LARGE hint: above the serial-only threshold, so no overlap
    // is attempted; the fresh sequential STAT drives, and the exact bytes
    // arrive.
    counts
        .open_during_stat_window
        .store(false, Ordering::SeqCst);
    let local = home.join("out-stale-large.bin");
    provider
        .download_with_size_hint(
            "/file.bin",
            local.to_str().unwrap(),
            Some(8 * 1024 * 1024),
            None,
        )
        .await
        .expect("stale-large hinted download");
    assert_eq!(std::fs::read(&local).unwrap(), payload);

    assert!(
        !counts.open_during_stat_window.load(Ordering::SeqCst),
        "large hints keep the sequential ordering"
    );

    // 5. The v1 truncation case: a 512 KiB file, 4 KiB hint, short
    // replies, serial path. Progress totals must come from STAT, too.
    provider.set_sftp_readahead(None);
    let progress = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = Arc::clone(&progress);
    let local = home.join("out-large-serial.bin");
    provider
        .download_with_size_hint(
            "/large.bin",
            local.to_str().unwrap(),
            Some(4096),
            Some(Box::new(move |done, total| {
                observed.lock().unwrap().push((done, total))
            })),
        )
        .await
        .expect("large serial stale-hint download");
    assert_eq!(std::fs::read(&local).unwrap(), large_payload);
    {
        let progress = progress.lock().unwrap();
        assert!(!progress.is_empty());
        assert!(progress
            .iter()
            .all(|(done, total)| *total == 524288 && done <= total));
        assert_eq!(progress.last(), Some(&(524288, 524288)));
    }
    assert_eq!(counts.handles.load(Ordering::SeqCst), 0);

    // 6. Fresh STAT selects read-ahead despite a small hint. The speculative
    // handle must be closed along with every read-ahead handle.
    provider.set_sftp_readahead(Some(4));
    let local = home.join("out-large-readahead.bin");
    provider
        .download_with_size_hint("/large.bin", local.to_str().unwrap(), Some(4096), None)
        .await
        .expect("stale hint selecting read-ahead");
    assert_eq!(std::fs::read(&local).unwrap(), large_payload);
    assert_eq!(counts.handles.load(Ordering::SeqCst), 0);

    // 7. Metadata failure must still await CLOSE of the successful OPEN.
    let local = home.join("out-stat-denied.bin");
    assert!(provider
        .download_with_size_hint(
            "/stat-denied.bin",
            local.to_str().unwrap(),
            Some(4096),
            None
        )
        .await
        .is_err());
    assert!(!local.exists());
    assert_eq!(counts.handles.load(Ordering::SeqCst), 0);

    // 8. Zero hint and empty remote use the fresh size and do not hang.
    let local = home.join("out-zero-hint.bin");
    provider
        .download_with_size_hint("/file.bin", local.to_str().unwrap(), Some(0), None)
        .await
        .expect("zero hint");
    assert_eq!(std::fs::read(&local).unwrap(), payload);
    let local = home.join("out-empty.bin");
    provider
        .download_with_size_hint("/empty.bin", local.to_str().unwrap(), Some(4096), None)
        .await
        .expect("empty remote");
    assert!(std::fs::read(&local).unwrap().is_empty());
    assert_eq!(counts.handles.load(Ordering::SeqCst), 0);

    // 9. A partial whose length equals the hint must resume against STAT,
    // not finalize early on the caller's old size.
    provider.set_sftp_readahead(None);
    let local = home.join("out-resume.bin");
    std::fs::write(home.join("out-resume.bin.aerotmp"), &payload[..4096]).unwrap();
    provider
        .download_with_size_hint("/file.bin", local.to_str().unwrap(), Some(4096), None)
        .await
        .expect("resume with stale hint");
    assert_eq!(std::fs::read(&local).unwrap(), payload);
    assert_eq!(counts.handles.load(Ordering::SeqCst), 0);
    assert!(counts.reads.load(Ordering::SeqCst) > 0);

    // 10. An exit after the open must close the handle and wait for the
    // reply, not leave it to Drop. The server now holds every CLOSE reply
    // back, so a client that only queues its close returns while the handle
    // is still counted. The other phases cannot see this: with an immediate
    // reply a queued close usually lands before the assertion does.
    counts.close_delay_ms.store(300, Ordering::SeqCst);

    // 10a. A partial that already holds the whole file returns before any read.
    let local = home.join("out-complete.bin");
    std::fs::write(home.join("out-complete.bin.aerotmp"), &payload).unwrap();
    provider
        .download_with_size_hint("/file.bin", local.to_str().unwrap(), Some(4096), None)
        .await
        .expect("partial already complete");
    assert_eq!(std::fs::read(&local).unwrap(), payload);
    assert_eq!(
        counts.handles.load(Ordering::SeqCst),
        0,
        "the early return for a complete partial left its handle to Drop"
    );

    // 10b. A local file that cannot be created fails after the open.
    let blocker = home.join("not-a-directory");
    std::fs::write(&blocker, b"x").unwrap();
    let local = blocker.join("out.bin");
    let err = provider
        .download_with_size_hint("/file.bin", local.to_str().unwrap(), Some(4096), None)
        .await
        .expect_err("a path under a regular file cannot be created");
    assert!(err.to_string().contains("local file"), "{err}");
    assert_eq!(
        counts.handles.load(Ordering::SeqCst),
        0,
        "a failed local create left the handle to Drop"
    );

    // 11. download_to_bytes_capped: STAT under-reports, the loop opens, then
    // the cap fires. That return used to skip the awaited close.
    let err = provider
        .download_to_bytes_capped("/under-size.bin", 100)
        .await
        .expect_err("capped download must refuse the under-reported body");
    assert!(
        err.to_string().contains("cap") || err.to_string().contains("under-reported"),
        "{err}"
    );
    assert_eq!(
        counts.handles.load(Ordering::SeqCst),
        0,
        "download_to_bytes_capped left the handle to Drop on the cap exit"
    );

    // 12. read_range rejects an oversized length after the open.
    let err = provider
        .read_range("/file.bin", 0, 100 * 1024 * 1024 + 1)
        .await
        .expect_err("oversized read_range must fail");
    assert!(err.to_string().contains("exceeds maximum"), "{err}");
    assert_eq!(
        counts.handles.load(Ordering::SeqCst),
        0,
        "read_range left the handle to Drop on the oversized-length exit"
    );

    // 13. Read-ahead success used Drop even on the happy path (N handles).
    provider.set_sftp_readahead(Some(4));
    let local = home.join("out-readahead-close.bin");
    provider
        .download_with_size_hint("/large.bin", local.to_str().unwrap(), Some(4096), None)
        .await
        .expect("readahead download under delayed CLOSE");
    assert_eq!(std::fs::read(&local).unwrap(), large_payload);
    assert_eq!(
        counts.handles.load(Ordering::SeqCst),
        0,
        "read-ahead left handles to Drop"
    );
    provider.set_sftp_readahead(None);

    // 14. Pipelined download, same: N handles, no awaited close.
    unsafe { std::env::set_var("AEROFTP_SFTP_READ_PIPELINE", "4") };
    let local = home.join("out-pipeline-close.bin");
    provider
        .download_with_size_hint(
            "/large.bin",
            local.to_str().unwrap(),
            Some(8 * 1024 * 1024),
            None,
        )
        .await
        .expect("pipelined download under delayed CLOSE");
    assert_eq!(std::fs::read(&local).unwrap(), large_payload);
    assert_eq!(
        counts.handles.load(Ordering::SeqCst),
        0,
        "pipelined download left handles to Drop"
    );
    unsafe { std::env::remove_var("AEROFTP_SFTP_READ_PIPELINE") };

    // 15. Upload opens a write handle then fails the WRITE. shutdown used
    // to run only after a successful copy.
    let src = home.join("to-upload.bin");
    std::fs::write(&src, b"hello-upload").unwrap();
    let err = provider
        .upload(src.to_str().unwrap(), "/upload-new.bin", None)
        .await
        .expect_err("fake server refuses WRITE");
    assert!(
        err.to_string().to_lowercase().contains("write")
            || err.to_string().to_lowercase().contains("remote"),
        "{err}"
    );
    assert_eq!(
        counts.handles.load(Ordering::SeqCst),
        0,
        "upload left the handle to Drop after a failed write"
    );

    counts.close_delay_ms.store(0, Ordering::SeqCst);

    provider.disconnect().await.ok();
    std::fs::remove_dir_all(&home).ok();
}

/// Cancel during the read-ahead OPEN fan-out: the join_all future used to be
/// dropped, so already-open File values only queued close_nowait.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn readahead_cancel_during_open_fanout_closes_handles() {
    let mut h = readahead_close_harness("cancel-fanout").await;
    h.counts.open_delay_ms.store(400, Ordering::SeqCst);
    let local = h.home.join("out-readahead-cancel.bin");
    let dest = local.to_string_lossy().into_owned();
    let slot = h.provider.transfer_cancel_slot();
    let err = {
        let fut =
            h.provider
                .download_with_size_hint("/large.bin", &dest, Some(8 * 1024 * 1024), None);
        tokio::pin!(fut);
        tokio::select! {
            r = &mut fut => r,
            _ = async {
                // Large hint: no speculative OPEN. STAT is delayed 250 ms,
                // then the fan-out waits 400 ms for OPEN replies.
                tokio::time::sleep(std::time::Duration::from_millis(320)).await;
                slot.lock().expect("cancel slot").cancel();
                std::future::pending::<()>().await;
            } => unreachable!(),
        }
    };
    let err = err.expect_err("cancel during open fan-out");
    assert!(err.to_string().to_lowercase().contains("cancel"), "{err}");
    assert_eq!(
        h.counts.handles.load(Ordering::SeqCst),
        0,
        "cancel during read-ahead OPEN fan-out left handles to Drop"
    );
    h.shutdown().await;
}

/// A reader error after the opens: try_join_all dropped the other readers
/// before they awaited close.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn readahead_reader_error_closes_sibling_handles() {
    let mut h = readahead_close_harness("reader-err").await;
    h.counts.fail_read_after.store(1, Ordering::SeqCst);
    // Keep the other readers blocked in READ so try_join_all dropping them
    // cannot close before the handle count is observed.
    h.counts.read_delay_ms.store(400, Ordering::SeqCst);
    let local = h.home.join("out-readahead-reader.bin");
    let err = h
        .provider
        .download_with_size_hint("/large.bin", local.to_str().unwrap(), Some(4096), None)
        .await
        .expect_err("read-ahead reader failure");
    assert!(
        err.to_string().to_lowercase().contains("read")
            || err.to_string().to_lowercase().contains("fail"),
        "{err}"
    );
    assert_eq!(
        h.counts.handles.load(Ordering::SeqCst),
        0,
        "read-ahead reader error left handles to Drop"
    );
    h.shutdown().await;
}

/// A writer error after the opens: try_join dropped the readers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn readahead_writer_error_closes_reader_handles() {
    let mut h = readahead_close_harness("writer-err").await;
    h.provider.set_fail_readahead_write(true);
    let local = h.home.join("out-readahead-writer.bin");
    let err = h
        .provider
        .download_with_size_hint("/large.bin", local.to_str().unwrap(), Some(4096), None)
        .await
        .expect_err("read-ahead writer failure");
    assert!(
        err.to_string().to_lowercase().contains("write")
            || err.to_string().to_lowercase().contains("injected"),
        "{err}"
    );
    assert_eq!(
        h.counts.handles.load(Ordering::SeqCst),
        0,
        "read-ahead writer error left handles to Drop"
    );
    h.provider.set_fail_readahead_write(false);
    h.shutdown().await;
}
