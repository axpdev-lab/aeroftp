//! A russh server for the SCP tests: exec channels run `/bin/sh -c <command>`
//! of this machine in a folder, the way a router's Dropbear runs BusyBox, and
//! the `sftp` subsystem is answered the way the test asks.

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use russh::keys::{PrivateKey, PublicKey};
use russh::server::{Auth, ChannelOpenHandle, Handler, Msg, Session};
use russh::{Channel, ChannelId};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{ChildStdin, Command};
use tokio::sync::Mutex;

/// How the server answers a request for the `sftp` subsystem.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // Not every test binary uses every mode.
pub enum Sftp {
    /// `SSH_MSG_CHANNEL_FAILURE`, as a server with no `Subsystem sftp` line.
    Refuse,
    /// Accept, then end the channel with exit status 127, as OpenWrt's
    /// Dropbear does when `/usr/libexec/sftp-server` is missing.
    EndAtOnce,
    /// The smallest SFTP v3 a connection needs: INIT and REALPATH.
    Serve,
    /// End the whole connection instead of answering, as a server that drops
    /// it while the client waits for the subsystem.
    DropConnection,
    /// Accept the subsystem, then end the whole connection before the SFTP
    /// VERSION, as a server that drops it during the handshake.
    DropAfterAccept,
}

pub struct Options {
    pub cwd: PathBuf,
    /// `PATH` for the commands.
    pub path: String,
    pub sftp: Sftp,
    pub accept_none: bool,
    pub password: Option<String>,
    pub key: Option<PublicKey>,
    pub host_key: PrivateKey,
    /// Every exec is answered with this on stderr and exit status 1, as an
    /// rssh or scponly account answers a command it does not allow.
    pub restricted: Option<String>,
    /// The SFTP answer for every connection after the first, when it differs:
    /// a server whose subsystem goes away between a session and its workers.
    pub sftp_after_first: Option<Sftp>,
    /// The SFTP answer for every request on a connection after its first:
    /// a server that serves one SFTP session per connection and refuses the
    /// second one a client opens beside it.
    pub sftp_later_channels: Option<Sftp>,
}

/// What reached the server.
#[derive(Default)]
pub struct Seen {
    pub execs: AtomicUsize,
    pub subsystems: AtomicUsize,
    pub connections: AtomicUsize,
    /// Channels the client closed while their command still ran here: a
    /// channel the server ended first does not count, whatever the order the
    /// two closes cross in.
    pub closes: AtomicUsize,
    /// The running sessions, for [`Seen::drop_connections`].
    sessions: std::sync::Mutex<Vec<russh::server::Handle>>,
}

#[allow(dead_code)]
impl Seen {
    pub fn execs(&self) -> usize {
        self.execs.load(Ordering::SeqCst)
    }

    pub fn subsystems(&self) -> usize {
        self.subsystems.load(Ordering::SeqCst)
    }

    pub fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }

    pub fn closes(&self) -> usize {
        self.closes.load(Ordering::SeqCst)
    }

    /// End every connection from the server's side, as a server that drops
    /// an idle session does.
    pub async fn drop_connections(&self) {
        let sessions = std::mem::take(&mut *self.sessions.lock().unwrap());
        for session in sessions {
            let _ = session
                .disconnect(
                    russh::Disconnect::ByApplication,
                    "dropped".to_string(),
                    String::new(),
                )
                .await;
        }
    }
}

struct Conn {
    /// Connections accepted before this one.
    nth: usize,
    opts: Arc<Options>,
    seen: Arc<Seen>,
    stdins: Arc<Mutex<HashMap<ChannelId, ChildStdin>>>,
    /// Channels this side has ended (exit status sent, or refused).
    ended: Arc<std::sync::Mutex<HashSet<ChannelId>>>,
    sftp_channels: HashSet<ChannelId>,
    sftp_buf: Vec<u8>,
    /// SFTP requests on this connection so far.
    sftp_asked: usize,
}

fn w32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_be_bytes());
}

fn wstr(out: &mut Vec<u8>, s: &[u8]) {
    w32(out, s.len() as u32);
    out.extend_from_slice(s);
}

impl Conn {
    fn sftp_reply(&self, packet: &[u8]) -> Option<Vec<u8>> {
        let kind = *packet.first()?;
        if kind == 1 {
            // INIT -> VERSION 3
            let mut r = vec![2];
            w32(&mut r, 3);
            return Some(r);
        }
        let id = u32::from_be_bytes(packet.get(1..5)?.try_into().ok()?);
        let mut r;
        if kind == 16 {
            // REALPATH -> NAME with the folder
            let home = self.opts.cwd.as_os_str().as_bytes();
            r = vec![104];
            w32(&mut r, id);
            w32(&mut r, 1);
            wstr(&mut r, home);
            wstr(&mut r, home);
            w32(&mut r, 0);
        } else {
            r = vec![101];
            w32(&mut r, id);
            w32(&mut r, 4);
            wstr(&mut r, b"unsupported");
            wstr(&mut r, b"");
        }
        Some(r)
    }
}

impl Handler for Conn {
    type Error = russh::Error;

    async fn auth_none(&mut self, _user: &str) -> Result<Auth, Self::Error> {
        Ok(if self.opts.accept_none {
            Auth::Accept
        } else {
            Auth::reject()
        })
    }

    async fn auth_password(&mut self, _user: &str, password: &str) -> Result<Auth, Self::Error> {
        Ok(if self.opts.password.as_deref() == Some(password) {
            Auth::Accept
        } else {
            Auth::reject()
        })
    }

    async fn auth_publickey(
        &mut self,
        _user: &str,
        key: russh::server::VerifiedPublicKey<'_>,
    ) -> Result<Auth, Self::Error> {
        Ok(
            if self.opts.key.as_ref().map(PublicKey::key_data) == Some(key.public_key.key_data()) {
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

    async fn subsystem_request(
        &mut self,
        channel: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.seen.subsystems.fetch_add(1, Ordering::SeqCst);
        let asked = self.sftp_asked;
        self.sftp_asked += 1;
        let sftp = match (self.opts.sftp_after_first, self.opts.sftp_later_channels) {
            (_, Some(later)) if asked > 0 => later,
            (Some(later), _) if self.nth > 0 => later,
            _ => self.opts.sftp,
        };
        match (name, sftp) {
            ("sftp", Sftp::Serve) => {
                self.sftp_channels.insert(channel);
                let _ = session.channel_success(channel);
            }
            ("sftp", Sftp::EndAtOnce) => {
                self.ended.lock().unwrap().insert(channel);
                let _ = session.channel_success(channel);
                let _ = session.extended_data(
                    channel,
                    1,
                    b"ash: /usr/libexec/sftp-server: not found\n".to_vec(),
                );
                let _ = session.exit_status_request(channel, 127);
                let _ = session.eof(channel);
                let _ = session.close(channel);
            }
            ("sftp", Sftp::DropConnection) => {
                let _ = session.disconnect(russh::Disconnect::ByApplication, "going away", "");
            }
            ("sftp", Sftp::DropAfterAccept) => {
                let _ = session.channel_success(channel);
                let _ = session.disconnect(russh::Disconnect::ByApplication, "going away", "");
            }
            _ => {
                let _ = session.channel_failure(channel);
            }
        }
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.seen.execs.fetch_add(1, Ordering::SeqCst);
        let _ = session.channel_success(channel);
        if let Some(message) = &self.opts.restricted {
            self.ended.lock().unwrap().insert(channel);
            let _ = session.extended_data(channel, 1, format!("{message}\n").into_bytes());
            let _ = session.exit_status_request(channel, 1);
            let _ = session.eof(channel);
            let _ = session.close(channel);
            return Ok(());
        }
        // By absolute path: `PATH` below is the server's, which a test narrows.
        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg(OsStr::from_bytes(data))
            .current_dir(&self.opts.cwd)
            .env("PATH", &self.opts.path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn sh");
        self.stdins
            .lock()
            .await
            .insert(channel, child.stdin.take().unwrap());
        let handle = session.handle();
        let ended = self.ended.clone();
        let mut stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        tokio::spawn(async move {
            let h = handle.clone();
            let err_task = tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                while let Ok(n) = stderr.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    let _ = h.extended_data(channel, 1, buf[..n].to_vec()).await;
                }
            });
            let mut buf = vec![0u8; 32768];
            while let Ok(n) = stdout.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                let _ = handle.data(channel, buf[..n].to_vec()).await;
            }
            let _ = err_task.await;
            let status = child
                .wait()
                .await
                .map(|s| s.code().unwrap_or(255))
                .unwrap_or(255);
            ended.lock().unwrap().insert(channel);
            let _ = handle.exit_status_request(channel, status as u32).await;
            let _ = handle.eof(channel).await;
            let _ = handle.close(channel).await;
        });
        Ok(())
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if self.sftp_channels.contains(&channel) {
            self.sftp_buf.extend_from_slice(data);
            while self.sftp_buf.len() >= 4 {
                let len = u32::from_be_bytes(self.sftp_buf[..4].try_into().unwrap()) as usize;
                if self.sftp_buf.len() < 4 + len {
                    break;
                }
                let packet: Vec<u8> = self.sftp_buf.drain(..4 + len).skip(4).collect();
                if let Some(reply) = self.sftp_reply(&packet) {
                    let mut framed = Vec::new();
                    wstr(&mut framed, &reply);
                    let _ = session.data(channel, framed);
                }
            }
            return Ok(());
        }
        if let Some(stdin) = self.stdins.lock().await.get_mut(&channel) {
            let _ = stdin.write_all(data).await;
        }
        Ok(())
    }

    async fn channel_eof(
        &mut self,
        channel: ChannelId,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        // Dropping the pipe is the process's end of input.
        self.stdins.lock().await.remove(&channel);
        Ok(())
    }

    async fn channel_close(
        &mut self,
        channel: ChannelId,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if !self.ended.lock().unwrap().contains(&channel) {
            self.seen.closes.fetch_add(1, Ordering::SeqCst);
        }
        self.stdins.lock().await.remove(&channel);
        Ok(())
    }
}

/// Start a server on loopback; returns its port and what reaches it.
pub async fn start(opts: Options) -> (u16, Arc<Seen>) {
    let config = Arc::new(russh::server::Config {
        // A refused `none` is answered at once, as OpenSSH and Dropbear do.
        auth_rejection_time_initial: Some(Duration::ZERO),
        auth_rejection_time: Duration::ZERO,
        keys: vec![opts.host_key.clone()],
        ..Default::default()
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let opts = Arc::new(opts);
    let seen = Arc::new(Seen::default());
    let seen_out = seen.clone();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let nth = seen.connections.fetch_add(1, Ordering::SeqCst);
            let handler = Conn {
                nth,
                opts: opts.clone(),
                seen: seen.clone(),
                stdins: Arc::default(),
                ended: Arc::default(),
                sftp_channels: HashSet::new(),
                sftp_buf: Vec::new(),
                sftp_asked: 0,
            };
            let cfg = config.clone();
            let seen = seen.clone();
            tokio::spawn(async move {
                if let Ok(running) = russh::server::run_stream(cfg, stream, handler).await {
                    seen.sessions.lock().unwrap().push(running.handle());
                    let _ = running.await;
                }
            });
        }
    });
    (port, seen_out)
}

pub fn ed25519() -> PrivateKey {
    PrivateKey::random(&mut rand_010::rng(), russh::keys::Algorithm::Ed25519).expect("ed25519")
}
