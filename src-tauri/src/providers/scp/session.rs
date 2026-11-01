//! SCP mode on one authenticated SSH connection.
//!
//! Everything runs as exec channels on the connection the caller already
//! authenticated, never on a second one: the libssh2 SCP leg removed in #73
//! dialled again and tripped "host key changed" on servers with several
//! keys. Transfers are the legacy SCP protocol (`scp -t` to write, `scp -f`
//! to read, see [`super::codec`]); every other operation is one POSIX shell
//! command with each path single-quoted ([`shell_quote`]), run under
//! `LC_ALL=C`, its exit status the verdict and its stderr the message.
//!
//! [`ScpSession::open`] probes the server once (one exec) for the tools the
//! operations need, so a missing tool is reported by name instead of as a
//! shell error halfway through an operation.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use russh::client::{Handle, Handler};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot, Mutex as TokioMutex, Semaphore};

use super::codec::{self, Control, Reply};
use super::listing::{self, Entry};
use crate::providers::shell_quote::{shell_quote, shell_quote_bytes};
use crate::providers::types::is_session_closed_error_message;
use crate::providers::ProviderError;
use crate::ssh_exec::{ssh_exec, ExecSession};

/// Cap on what one command may print (a listing of about 500 000 entries).
const MAX_OUTPUT: usize = 64 * 1024 * 1024;

/// Chunk size for the data of an upload.
const UPLOAD_CHUNK: usize = 32 * 1024;

/// Commands one connection runs at once, whatever number of workers share
/// it. OpenSSH allows 10 sessions per connection by default (`MaxSessions`),
/// and a closed channel can take a moment to leave the server's count.
pub const CHANNELS_PER_CONNECTION: usize = 8;

/// What the probe found on the server.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Probe {
    /// `uname -s` (`Linux` on OpenWrt).
    pub system: String,
    /// The login directory, from `pwd` before any `cd`.
    pub home: String,
    /// Tools found by `command -v`, plus `ls-full-time` when `ls` takes
    /// `--full-time`.
    pub tools: Vec<String>,
}

impl Probe {
    pub fn has(&self, tool: &str) -> bool {
        self.tools.iter().any(|t| t == tool)
    }
}

/// The tools the probe asks about. `scp`, `ls-full-time` and `find` are what
/// SCP mode cannot work without; the rest each enable one operation.
const PROBED_TOOLS: &[&str] = &[
    "scp",
    "find",
    "mkdir",
    "rm",
    "rmdir",
    "mv",
    "chmod",
    "touch",
    "sha256sum",
    "md5sum",
    "df",
    "dd",
    "head",
    "cat",
];

/// The output of one command.
#[derive(Debug, Default)]
pub struct Output {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub status: u32,
}

impl Output {
    fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).trim().to_string()
    }
}

/// SCP mode over an authenticated SSH handle.
pub struct ScpSession<H: Handler> {
    handle: Arc<TokioMutex<Handle<H>>>,
    /// Places for commands running at once on the connection, shared by
    /// every [`Self::worker`] of it.
    channels: Arc<Semaphore>,
    probe: Probe,
    /// The longest wait for the next bytes from the server.
    io_timeout: Duration,
    /// Transfer rates in bytes per second, 0 for no limit.
    upload_limit_bps: AtomicU64,
    download_limit_bps: AtomicU64,
}

/// Bytes from a channel's stdout, read a byte, a line or a length at a time.
struct Incoming {
    rx: mpsc::Receiver<Vec<u8>>,
    buf: Vec<u8>,
    pos: usize,
    io_timeout: Duration,
}

impl Incoming {
    async fn fill(&mut self) -> Result<(), ProviderError> {
        while self.pos == self.buf.len() {
            let chunk = tokio::time::timeout(self.io_timeout, self.rx.recv())
                .await
                .map_err(|_| ProviderError::Timeout)?
                .ok_or_else(|| {
                    ProviderError::TransferFailed("the server closed the SCP channel".to_string())
                })?;
            self.buf = chunk;
            self.pos = 0;
        }
        Ok(())
    }

    async fn byte(&mut self) -> Result<u8, ProviderError> {
        self.fill().await?;
        let b = self.buf[self.pos];
        self.pos += 1;
        Ok(b)
    }

    /// The bytes up to the next newline, which is consumed and not returned.
    async fn line(&mut self) -> Result<Vec<u8>, ProviderError> {
        let mut line = Vec::new();
        loop {
            let b = self.byte().await?;
            if b == b'\n' {
                return Ok(line);
            }
            if line.len() == codec::MAX_LINE {
                return Err(ProviderError::ParseError(
                    "SCP protocol: line too long".to_string(),
                ));
            }
            line.push(b);
        }
    }

    /// Exactly `n` bytes into `sink`, calling `progress` with the bytes so far
    /// and `n`, at no more than `limit_bps` (0 for no limit).
    async fn copy_to<W: AsyncWrite + Unpin>(
        &mut self,
        n: u64,
        sink: &mut W,
        progress: &mut (dyn FnMut(u64, u64) + Send),
        limit_bps: u64,
    ) -> Result<(), ProviderError> {
        let mut done = 0u64;
        let start = Instant::now();
        while done < n {
            self.fill().await?;
            let avail = (self.buf.len() - self.pos) as u64;
            let take = avail.min(n - done) as usize;
            sink.write_all(&self.buf[self.pos..self.pos + take])
                .await
                .map_err(|e| ProviderError::TransferFailed(format!("writing the download: {e}")))?;
            self.pos += take;
            done += take as u64;
            progress(done, n);
            crate::throttle_transfer(done, start.elapsed(), limit_bps).await;
        }
        Ok(())
    }

    /// The exit status, reading on past any stdout the server still sends:
    /// stdout is a bounded queue, and a full one would hold the status back.
    async fn exit_status(
        &mut self,
        exit_rx: &mut oneshot::Receiver<u32>,
    ) -> Result<u32, ProviderError> {
        let mut open = true;
        tokio::time::timeout(self.io_timeout, async {
            loop {
                tokio::select! {
                    status = &mut *exit_rx => {
                        return status.unwrap_or(crate::ssh_exec::EXIT_ABNORMAL);
                    }
                    chunk = self.rx.recv(), if open => open = chunk.is_some(),
                }
            }
        })
        .await
        .map_err(|_| ProviderError::Timeout)
    }

    /// One SCP status reply: `\0`, or `\1`/`\2` and a message line.
    async fn reply(&mut self) -> Result<Reply, ProviderError> {
        let status = self.byte().await?;
        if status == 0 {
            return Ok(Reply::Ok);
        }
        let message = self.line().await?;
        codec::reply(status, &message)
    }
}

/// The last component of `path`, which SCP carries on its `C` line.
fn basename(path: &str) -> Result<&str, ProviderError> {
    let name = path.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    codec::validate_name(name)?;
    Ok(name)
}

/// A failed command as the error the caller can act on.
fn command_error(what: &str, path: &str, out: &Output) -> ProviderError {
    let msg = out.stderr_text();
    // `Permission denied` first: an unreadable operand can print both, and
    // the denial is the one the user can act on.
    if msg.contains("Permission denied") {
        ProviderError::PermissionDenied(format!("{what} {path}: {msg}"))
    } else if msg.contains("No such file or directory") {
        ProviderError::NotFound(path.to_string())
    } else if msg.contains("File exists") {
        ProviderError::AlreadyExists(path.to_string())
    } else if msg.contains("Directory not empty") {
        ProviderError::DirectoryNotEmpty(path.to_string())
    } else {
        ProviderError::ServerError(format!("{what} {path}: {msg} (exit status {})", out.status))
    }
}

/// The error an SCP `\1` or `\2` message stands for.
fn refusal(message: &str, path: &str) -> ProviderError {
    if message.contains("Permission denied") {
        ProviderError::PermissionDenied(format!("{path}: {message}"))
    } else if message.contains("No such file or directory") {
        ProviderError::NotFound(path.to_string())
    } else if message.contains("Is a directory") {
        ProviderError::InvalidPath(format!("{path} is a directory"))
    } else {
        ProviderError::TransferFailed(format!("{path}: {message}"))
    }
}

/// Turns an SCP reply into the result of the step that asked for it.
fn reply_ok(reply: Reply, path: &str) -> Result<(), ProviderError> {
    match reply {
        Reply::Ok => Ok(()),
        Reply::Warning(m) | Reply::Fatal(m) => Err(refusal(&m, path)),
    }
}

impl<H> ScpSession<H>
where
    H: Handler<Error = russh::Error> + Send + Sync + 'static,
{
    /// Probe the server on `handle` and refuse it, naming what is missing,
    /// when it lacks what SCP mode needs.
    pub async fn open(
        handle: Arc<TokioMutex<Handle<H>>>,
        io_timeout: Duration,
    ) -> Result<Self, ProviderError> {
        let mut session = Self {
            handle,
            channels: Arc::new(Semaphore::new(CHANNELS_PER_CONNECTION)),
            probe: Probe::default(),
            io_timeout,
            upload_limit_bps: AtomicU64::new(0),
            download_limit_bps: AtomicU64::new(0),
        };
        let mut script = String::from(
            "printf 'system=%s\\0' \"$(uname -s)\"; printf 'home=%s\\0' \"$(pwd)\"; for t in",
        );
        for tool in PROBED_TOOLS {
            script.push(' ');
            script.push_str(tool);
        }
        script.push_str(
            "; do command -v \"$t\" >/dev/null 2>&1 && printf 'has=%s\\0' \"$t\"; done; \
             ls -land --full-time -- / >/dev/null 2>&1 && printf 'has=ls-full-time\\0'; \
             find / -mindepth 0 -maxdepth 0 -print0 >/dev/null 2>&1 && printf 'has=find-print0\\0'",
        );
        let out = session.run(script.as_bytes()).await?;
        for record in out.stdout.split(|&b| b == 0) {
            let record = String::from_utf8_lossy(record);
            if let Some(v) = record.strip_prefix("system=") {
                session.probe.system = v.to_string();
            } else if let Some(v) = record.strip_prefix("home=") {
                session.probe.home = v.to_string();
            } else if let Some(v) = record.strip_prefix("has=") {
                session.probe.tools.push(v.to_string());
            }
        }
        // A restricted account (`ForceCommand`, rssh, scponly, a menu shell)
        // runs nothing of the probe: say so, with the server's own words.
        if session.probe.system.is_empty() && session.probe.tools.is_empty() {
            let said = out.stderr_text();
            let said = said.lines().next().unwrap_or("").chars().take(200);
            let said: String = said.collect();
            return Err(ProviderError::NotSupported(if said.is_empty() {
                "the server ran no shell command for this account (a restricted shell or a \
                 forced command), so SCP mode cannot work"
                    .to_string()
            } else {
                format!(
                    "the server ran no shell command for this account (a restricted shell or \
                     a forced command), so SCP mode cannot work; it said: {said}"
                )
            }));
        }
        // `find` alone is not enough: the listing needs its `-mindepth`,
        // `-maxdepth` and `-print0`, which a minimal BusyBox may leave out.
        let missing: Vec<&str> = ["scp", "ls-full-time", "find-print0"]
            .into_iter()
            .filter(|t| !session.probe.has(t))
            .collect();
        if !missing.is_empty() {
            return Err(ProviderError::NotSupported(format!(
                "SCP mode needs {} on the server, which it lacks",
                missing.join(", ")
            )));
        }
        Ok(session)
    }

    pub fn probe(&self) -> &Probe {
        &self.probe
    }

    /// Start `command` on its own channel. The channel holds its place among
    /// the connection's [`CHANNELS_PER_CONNECTION`] until the session is
    /// dropped and the channel's close has been sent, so a command abandoned
    /// part way gives its place back only once the server lets go of it too.
    /// A session never starts a command while it holds another, so waiting
    /// for a place cannot deadlock.
    async fn exec(&self, command: &[u8]) -> Result<ExecSession, ProviderError> {
        let place = self
            .channels
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| ProviderError::NotConnected)?;
        self.exec_in(place, command).await
    }

    /// [`exec`](Self::exec) in a place already taken.
    async fn exec_in(
        &self,
        place: tokio::sync::OwnedSemaphorePermit,
        command: &[u8],
    ) -> Result<ExecSession, ProviderError> {
        let mut exec = match ssh_exec(self.handle.clone(), command).await {
            Ok(exec) => exec,
            // A connection the server ended (an idle timeout, a restart) is
            // lost, so a caller may dial again; a channel refused on a live
            // connection is not.
            Err(e)
                if self.handle.lock().await.is_closed() || is_session_closed_error_message(&e) =>
            {
                return Err(ProviderError::ConnectionLost(e))
            }
            Err(e) => return Err(ProviderError::ConnectionFailed(e)),
        };
        exec.hold_until_closed(place);
        Ok(exec)
    }

    /// Whether the connection still answers, with a `:` on a channel of its
    /// own. When all [`CHANNELS_PER_CONNECTION`] are taken the connection is
    /// carrying commands, which is its traffic: the tick is skipped rather
    /// than queued behind them, so a keepalive never waits for a transfer.
    pub async fn keep_alive(&self) -> Result<(), ProviderError> {
        if self.handle.lock().await.is_closed() {
            return Err(ProviderError::ConnectionLost(
                "the SSH connection has ended".to_string(),
            ));
        }
        let Ok(place) = self.channels.clone().try_acquire_owned() else {
            return Ok(());
        };
        let exec = self.exec_in(place, b":").await?;
        let out = self.collect(exec).await?;
        if out.status != 0 {
            return Err(ProviderError::ConnectionLost(format!(
                "exit status {}",
                out.status
            )));
        }
        Ok(())
    }

    /// Run `command` under `LC_ALL=C` and collect its output. A command that
    /// prints more than [`MAX_OUTPUT`] is an error, never a silently cut
    /// result.
    pub async fn run(&self, command: &[u8]) -> Result<Output, ProviderError> {
        let mut full = b"LC_ALL=C; export LC_ALL; ".to_vec();
        full.extend_from_slice(command);
        let exec = self.exec(&full).await?;
        self.collect(exec).await
    }

    /// What a started command prints, and its exit status.
    async fn collect(&self, mut exec: ExecSession) -> Result<Output, ProviderError> {
        let mut out = Output::default();
        let mut stderr_open = true;
        loop {
            let next = tokio::time::timeout(self.io_timeout, async {
                tokio::select! {
                    chunk = exec.stdout_rx.recv() => (true, chunk),
                    chunk = exec.stderr_rx.recv(), if stderr_open => (false, chunk),
                }
            })
            .await
            .map_err(|_| ProviderError::Timeout)?;
            match next {
                (true, Some(chunk)) => {
                    if out.stdout.len() + chunk.len() > MAX_OUTPUT {
                        return Err(ProviderError::ServerError(format!(
                            "the command printed more than {} MiB",
                            MAX_OUTPUT / (1024 * 1024)
                        )));
                    }
                    out.stdout.extend_from_slice(&chunk);
                }
                (true, None) => break,
                (false, Some(chunk)) => {
                    if out.stderr.len() < codec::MAX_LINE {
                        out.stderr.extend_from_slice(&chunk);
                    }
                }
                (false, None) => stderr_open = false,
            }
        }
        while let Ok(chunk) = exec.stderr_rx.try_recv() {
            if out.stderr.len() < codec::MAX_LINE {
                out.stderr.extend_from_slice(&chunk);
            }
        }
        out.status = tokio::time::timeout(self.io_timeout, &mut exec.exit_rx)
            .await
            .map_err(|_| ProviderError::Timeout)?
            .unwrap_or(crate::ssh_exec::EXIT_ABNORMAL);
        Ok(out)
    }

    /// `run` for a command that must succeed; its failure names `what` and
    /// `path`.
    async fn run_ok(
        &self,
        what: &str,
        path: &str,
        command: &[u8],
    ) -> Result<Output, ProviderError> {
        let out = self.run(command).await?;
        if out.status != 0 {
            return Err(command_error(what, path, &out));
        }
        Ok(out)
    }

    /// The entries of the folder `dir`, from `ls` and `find` in one exec plus
    /// one more for the names `ls` cannot carry ([`listing`]).
    pub async fn list(&self, dir: &str) -> Result<Vec<Entry>, ProviderError> {
        // `/.` lists the folder a symlink points to, not the link.
        let target = format!("{}/.", dir.trim_end_matches('/'));
        let q = shell_quote(&target)?;
        let script = format!(
            "ls -lan --full-time -- {q} || exit; printf '\\0'; \
             find {q} -mindepth 1 -maxdepth 1 -print0"
        );
        let out = self.run_ok("list", dir, script.as_bytes()).await?;
        // `ls` never prints a NUL, so the first one ends its part.
        let split = out.stdout.iter().position(|&b| b == 0).ok_or_else(|| {
            ProviderError::ParseError(format!("list {dir}: the listing has no separator"))
        })?;
        let (ls_out, find_out) = (&out.stdout[..split], &out.stdout[split + 1..]);
        let mut merged = listing::merge(find_out, ls_out);
        if !merged.unresolved.is_empty() {
            let resolved = self
                .resolve("list", dir, target.as_bytes(), &merged.unresolved, false)
                .await?;
            merged.entries.extend(resolved);
        }
        Ok(merged.entries)
    }

    /// The metadata of each of `names` in `dir`, symlinks followed with
    /// `follow`, from [`listing::resolve_script`] in batches of
    /// [`listing::resolve_batches`]. A name not there is left out; one the
    /// server could not read fails the whole call, except with `follow`,
    /// where a target that cannot be read is left out too.
    pub async fn resolve(
        &self,
        what: &str,
        shown: &str,
        dir: &[u8],
        names: &[Vec<u8>],
        follow: bool,
    ) -> Result<Vec<Entry>, ProviderError> {
        let mut entries = Vec::with_capacity(names.len());
        for run in listing::resolve_batches(dir, names, listing::RESOLVE_BUDGET)? {
            let batch = &names[run];
            let script = listing::resolve_script(dir, batch, follow)?;
            let out = self.run_ok(what, shown, &script).await?;
            entries.extend(listing::parse_resolved(batch, &out.stdout));
        }
        Ok(entries)
    }

    /// The metadata of `path` itself (a symlink is not followed).
    pub async fn stat(&self, path: &str) -> Result<listing::Meta, ProviderError> {
        let name = path.as_bytes().to_vec();
        let mut script = b"ls -land --full-time -- ".to_vec();
        script.extend_from_slice(&shell_quote_bytes(&name)?);
        let out = self.run_ok("stat", path, &script).await?;
        let mut chunk = out.stdout;
        chunk.push(0);
        listing::parse_resolved(&[name], &chunk)
            .pop()
            .map(|e| e.meta)
            .ok_or_else(|| ProviderError::ParseError(format!("stat {path}: unreadable ls line")))
    }

    pub async fn mkdir(&self, path: &str) -> Result<(), ProviderError> {
        let cmd = format!("mkdir -- {}", shell_quote(path)?);
        self.run_ok("mkdir", path, cmd.as_bytes()).await.map(drop)
    }

    pub async fn remove_file(&self, path: &str) -> Result<(), ProviderError> {
        // `-f` would hide a missing file; a directory is refused by `rm`.
        let cmd = format!("rm -- {}", shell_quote(path)?);
        self.run_ok("delete", path, cmd.as_bytes()).await.map(drop)
    }

    pub async fn remove_dir(&self, path: &str) -> Result<(), ProviderError> {
        let cmd = format!("rmdir -- {}", shell_quote(path)?);
        self.run_ok("rmdir", path, cmd.as_bytes()).await.map(drop)
    }

    pub async fn remove_dir_recursive(&self, path: &str) -> Result<(), ProviderError> {
        let trimmed = path.trim_end_matches('/');
        if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
            return Err(ProviderError::InvalidPath(format!(
                "refusing to remove {path:?} recursively"
            )));
        }
        let cmd = format!("rm -r -- {}", shell_quote(path)?);
        self.run_ok("rmdir -r", path, cmd.as_bytes())
            .await
            .map(drop)
    }

    /// Rename `from` to `to`, refusing a `to` that exists, as SFTP's rename
    /// does. `mv` alone would replace a file, and would move `from` inside
    /// `to` when that is a folder, both without a word: the check refuses
    /// both. A file that appears at `to` after the check is not replaced
    /// either: `mv -n` leaves `from` where it is, and `from` and `to` both
    /// being there, with no diagnostic but a `not replacing`, is the
    /// refusal. A skipped `mv -n` says nothing and exits 1 on GNU 9.4, 0 on
    /// GNU 9.7 and BusyBox (measured), so its status alone decides nothing;
    /// any other diagnostic is passed on with its status. What
    /// stays open is a folder created at `to` in that instant, which takes
    /// `from` inside it: the same window OpenSSH's `sftp-server` has when it
    /// renames a folder (it stats, then renames).
    pub async fn rename(&self, from: &str, to: &str) -> Result<(), ProviderError> {
        let (f, t) = (shell_quote(from)?, shell_quote(to)?);
        let cmd = format!(
            "if [ -e {t} ] || [ -L {t} ]; then echo 'mv: target: File exists' >&2; exit 1; fi; \
             err=$(mv -n -- {f} {t} 2>&1); rc=$?; \
             if [ -e {f} ] || [ -L {f} ]; then \
               if {{ [ -e {t} ] || [ -L {t} ]; }} && \
                  {{ [ -z \"$err\" ] || [ \"${{err#*not replacing}}\" != \"$err\" ]; }}; then \
                 echo 'mv: target: File exists' >&2; exit 1; \
               fi; \
               [ -n \"$err\" ] || err='mv: the source is still in place'; \
               [ \"$rc\" -ne 0 ] || rc=1; \
             fi; \
             [ -z \"$err\" ] || printf '%s\\n' \"$err\" >&2; exit \"$rc\""
        );
        self.run_ok("rename", to, cmd.as_bytes()).await.map(drop)
    }

    pub async fn chmod(&self, path: &str, mode: u32) -> Result<(), ProviderError> {
        let cmd = format!("chmod {:o} -- {}", mode & 0o7777, shell_quote(path)?);
        self.run_ok("chmod", path, cmd.as_bytes()).await.map(drop)
    }

    /// Write `size` bytes from `src` to the file `path` with `scp -t`,
    /// stamping `mtime` (seconds) when given. `progress` gets the bytes sent
    /// so far and `size`.
    pub async fn upload<R: AsyncRead + Unpin>(
        &self,
        path: &str,
        src: &mut R,
        size: u64,
        mode: u32,
        mtime: Option<u64>,
        progress: &mut (dyn FnMut(u64, u64) + Send),
    ) -> Result<(), ProviderError> {
        let name = basename(path)?;
        let flags = if mtime.is_some() { "-p -t" } else { "-t" };
        // A sink given a folder writes into it under the `C` line's name, so
        // an upload to `/x/dir` would land in `/x/dir/dir` and succeed. The
        // check runs in the same exec and refuses in the sink's own words.
        let p = shell_quote(path)?;
        let cmd = format!(
            "if [ -d {p} ]; then printf '\\001scp: Is a directory\\n'; exit 1; fi; \
             LC_ALL=C scp {flags} -- {p}"
        );
        let mut exec = self.exec(cmd.as_bytes()).await?;
        let mut incoming = Incoming {
            rx: std::mem::replace(&mut exec.stdout_rx, mpsc::channel(1).1),
            buf: Vec::new(),
            pos: 0,
            io_timeout: self.io_timeout,
        };
        let io = |e: std::io::Error| ProviderError::TransferFailed(format!("{path}: {e}"));
        // The sink speaks first: one status byte once it is ready.
        reply_ok(incoming.reply().await?, path)?;
        if let Some(mtime) = mtime {
            exec.stdin
                .write_all(&codec::times_line(mtime, mtime))
                .await
                .map_err(io)?;
            reply_ok(incoming.reply().await?, path)?;
        }
        exec.stdin
            .write_all(&codec::file_line(mode, size, name)?)
            .await
            .map_err(io)?;
        reply_ok(incoming.reply().await?, path)?;
        let mut buf = vec![0u8; UPLOAD_CHUNK];
        let mut sent = 0u64;
        let start = Instant::now();
        let limit_bps = self.upload_limit_bps.load(Ordering::Relaxed);
        while sent < size {
            let want = (size - sent).min(UPLOAD_CHUNK as u64) as usize;
            let n = src.read(&mut buf[..want]).await.map_err(io)?;
            if n == 0 {
                // Closing the channel without the final status byte makes the
                // sink drop the partial file's success; nothing claims it.
                return Err(ProviderError::TransferFailed(format!(
                    "{path}: the source ended after {sent} of {size} bytes"
                )));
            }
            exec.stdin.write_all(&buf[..n]).await.map_err(io)?;
            sent += n as u64;
            progress(sent, size);
            crate::throttle_transfer(sent, start.elapsed(), limit_bps).await;
        }
        exec.stdin.write_all(&[0]).await.map_err(io)?;
        reply_ok(incoming.reply().await?, path)?;
        exec.stdin.shutdown().await.map_err(io)?;
        let status = incoming.exit_status(&mut exec.exit_rx).await?;
        if status != 0 {
            return Err(ProviderError::TransferFailed(format!(
                "{path}: scp exited with status {status}"
            )));
        }
        Ok(())
    }

    /// Read the file `path` into `sink` with `scp -f`. Returns its size and
    /// the mtime the server sent; `progress` gets the bytes received and the
    /// size. The server's `C` line must name the file that was asked for
    /// (CVE-2019-6111).
    pub async fn download<W: AsyncWrite + Unpin>(
        &self,
        path: &str,
        sink: &mut W,
        progress: &mut (dyn FnMut(u64, u64) + Send),
    ) -> Result<(u64, Option<u64>), ProviderError> {
        let name = basename(path)?;
        let cmd = format!("LC_ALL=C scp -p -f -- {}", shell_quote(path)?);
        let mut exec = self.exec(cmd.as_bytes()).await?;
        let mut incoming = Incoming {
            rx: std::mem::replace(&mut exec.stdout_rx, mpsc::channel(1).1),
            buf: Vec::new(),
            pos: 0,
            io_timeout: self.io_timeout,
        };
        let io = |e: std::io::Error| ProviderError::TransferFailed(format!("{path}: {e}"));
        exec.stdin.write_all(&[0]).await.map_err(io)?;
        let mut mtime = None;
        let size = loop {
            let first = incoming.byte().await?;
            if first == 1 || first == 2 {
                let message = incoming.line().await?;
                return Err(refusal(&String::from_utf8_lossy(&message), path));
            }
            let mut line = vec![first];
            line.extend_from_slice(&incoming.line().await?);
            match codec::parse_control(&line)? {
                Control::Times { mtime: m, .. } => mtime = Some(m),
                Control::File {
                    size, name: got, ..
                } => {
                    if got != name {
                        return Err(ProviderError::TransferFailed(format!(
                            "{path}: the server sent {got:?}, not the file asked for"
                        )));
                    }
                    break size;
                }
                Control::Dir { .. } | Control::EndDir => {
                    return Err(ProviderError::InvalidPath(format!("{path} is a directory")));
                }
            }
            exec.stdin.write_all(&[0]).await.map_err(io)?;
        };
        exec.stdin.write_all(&[0]).await.map_err(io)?;
        let limit_bps = self.download_limit_bps.load(Ordering::Relaxed);
        incoming.copy_to(size, sink, progress, limit_bps).await?;
        reply_ok(incoming.reply().await?, path)?;
        exec.stdin.write_all(&[0]).await.map_err(io)?;
        sink.flush().await.map_err(io)?;
        exec.stdin.shutdown().await.map_err(io)?;
        let status = incoming.exit_status(&mut exec.exit_rx).await?;
        if status != 0 {
            return Err(ProviderError::TransferFailed(format!(
                "{path}: scp exited with status {status}"
            )));
        }
        Ok((size, mtime))
    }

    /// Pace the transfers that follow to these rates, in bytes per second
    /// (0 for no limit).
    pub fn set_speed_limits(&self, upload_bps: u64, download_bps: u64) {
        self.upload_limit_bps.store(upload_bps, Ordering::Relaxed);
        self.download_limit_bps
            .store(download_bps, Ordering::Relaxed);
    }

    /// A second session on the same connection, for a parallel transfer:
    /// every command opens its own exec channel, so sessions that share a
    /// handle run side by side. It keeps this session's probe and rates.
    pub fn worker(&self) -> Self {
        Self {
            handle: self.handle.clone(),
            channels: self.channels.clone(),
            probe: self.probe.clone(),
            io_timeout: self.io_timeout,
            upload_limit_bps: AtomicU64::new(self.upload_limit_bps.load(Ordering::Relaxed)),
            download_limit_bps: AtomicU64::new(self.download_limit_bps.load(Ordering::Relaxed)),
        }
    }

    /// The metadata of what `path` names, a symlink followed.
    pub async fn stat_followed(&self, path: &str) -> Result<listing::Meta, ProviderError> {
        let name = path.as_bytes().to_vec();
        let mut script = b"ls -lanLd --full-time -- ".to_vec();
        script.extend_from_slice(&shell_quote_bytes(&name)?);
        let out = self.run_ok("stat", path, &script).await?;
        let mut chunk = out.stdout;
        chunk.push(0);
        listing::parse_resolved(&[name], &chunk)
            .pop()
            .map(|e| e.meta)
            .ok_or_else(|| ProviderError::ParseError(format!("stat {path}: unreadable ls line")))
    }

    /// The metadata of each of the absolute `paths` (symlinks not
    /// followed), in the batches of [`listing::resolve_batches`]. A path
    /// gone in the meantime is left out; one the server could not read is
    /// an error, so a search never comes back short without saying so.
    pub async fn stat_many(&self, paths: &[Vec<u8>]) -> Result<Vec<Entry>, ProviderError> {
        // `resolve_script` joins a folder and a name: the root and each
        // absolute path without its first `/`.
        let relative: Vec<Vec<u8>> = paths
            .iter()
            .map(|p| p.strip_prefix(b"/").unwrap_or(p).to_vec())
            .collect();
        let mut entries = Vec::with_capacity(paths.len());
        for run in listing::resolve_batches(b"/", &relative, listing::RESOLVE_BUDGET)? {
            let script = listing::resolve_script(b"/", &relative[run.clone()], false)?;
            let batch = &paths[run];
            let shown = String::from_utf8_lossy(&batch[0]).into_owned();
            let out = self.run_ok("stat", &shown, &script).await?;
            entries.extend(listing::parse_resolved(batch, &out.stdout));
        }
        Ok(entries)
    }

    /// Put `from` in place of `to`. For a file that is `mv -f`, one
    /// `rename(2)` when the two are in one file system (callers stage the
    /// file in the target's own folder): there is no moment without a file
    /// at `to`. A folder replaces only an empty folder, as `rename(2)` does,
    /// and a file only a file or a symlink ([`refuse_replace_across_types`]).
    /// A symlink at `to` is replaced itself, never followed.
    ///
    /// Nothing at `to` is removed beforehand: a replace that fails (a source
    /// gone, a folder moved into itself, a full folder) leaves `to` as it
    /// was. Where `to` is a folder, or a link to one, plain `mv` would move
    /// `from` inside it, so `mv -T` treats `to` as the name itself. GNU then
    /// replaces a link to a folder; BusyBox refuses it with "is a directory"
    /// and leaves the link (both measured).
    ///
    /// [`refuse_replace_across_types`]: crate::providers::refuse_replace_across_types
    pub async fn replace(&self, from: &str, to: &str) -> Result<(), ProviderError> {
        let (f, t) = (shell_quote(from)?, shell_quote(to)?);
        let cmd = format!(
            "if [ -d {f} ] && [ ! -L {f} ]; then fd=1; else fd=0; fi; \
             if [ -e {t} ] || [ -L {t} ]; then \
               if [ -d {t} ] && [ ! -L {t} ]; then td=1; else td=0; fi; \
               if [ \"$fd\" != \"$td\" ]; then printf '{ACROSS} %s %s\\n' \"$fd\" \"$td\" >&2; exit 3; fi; \
             fi; \
             if [ -d {t} ]; then mv -fT -- {f} {t}; else mv -f -- {f} {t}; fi"
        );
        let out = self.run(cmd.as_bytes()).await?;
        if out.status == 0 {
            return Ok(());
        }
        let said = out.stderr_text();
        if let Some(kinds) = said.strip_prefix(ACROSS) {
            let mut kinds = kinds.split_whitespace();
            let source_is_dir = kinds.next() == Some("1");
            let occupant_is_dir = kinds.next() == Some("1");
            crate::providers::refuse_replace_across_types(to, source_is_dir, occupant_is_dir)?;
        }
        Err(command_error("replace", from, &out))
    }

    /// The space of the file system holding `path`: total, used and
    /// available bytes, from `df -Pk` (POSIX format: one line per file
    /// system, 1024-byte blocks).
    pub async fn disk_space(&self, path: &str) -> Result<(u64, u64, u64), ProviderError> {
        let cmd = format!("df -Pk -- {}", shell_quote(path)?);
        let out = self.run_ok("df", path, cmd.as_bytes()).await?;
        parse_df(&out.stdout)
            .ok_or_else(|| ProviderError::ParseError(format!("df {path}: unreadable output")))
    }

    /// The bytes of the file `path` from `offset` into `sink`: `len` of
    /// them, or all to its end when `len` is `None`. Returns how many came.
    /// `dd` with `count=0` only moves the descriptor the group shares, by
    /// `lseek` on a regular file, and `head` or `cat` reads on from there, so
    /// no byte before `offset` is read (measured on BusyBox 1.36.1 and GNU).
    /// `progress` gets the bytes so far.
    pub async fn read_at<W: AsyncWrite + Unpin>(
        &self,
        path: &str,
        offset: u64,
        len: Option<u64>,
        sink: &mut W,
        progress: &mut (dyn FnMut(u64) + Send),
    ) -> Result<u64, ProviderError> {
        let rest = match len {
            Some(n) => format!("head -c {n}"),
            None => "cat".to_string(),
        };
        let cmd = format!(
            "LC_ALL=C; export LC_ALL; {{ dd bs=1 skip={offset} count=0 2>/dev/null; {rest}; }} < {}",
            shell_quote(path)?
        );
        let mut exec = self.exec(cmd.as_bytes()).await?;
        let io = |e: std::io::Error| ProviderError::TransferFailed(format!("{path}: {e}"));
        let limit_bps = self.download_limit_bps.load(Ordering::Relaxed);
        let start = Instant::now();
        let mut done = 0u64;
        let mut stderr = Vec::new();
        let mut stderr_open = true;
        loop {
            let next = tokio::time::timeout(self.io_timeout, async {
                tokio::select! {
                    chunk = exec.stdout_rx.recv() => (true, chunk),
                    chunk = exec.stderr_rx.recv(), if stderr_open => (false, chunk),
                }
            })
            .await
            .map_err(|_| ProviderError::Timeout)?;
            match next {
                (true, Some(chunk)) => {
                    sink.write_all(&chunk).await.map_err(io)?;
                    done += chunk.len() as u64;
                    progress(done);
                    crate::throttle_transfer(done, start.elapsed(), limit_bps).await;
                }
                (true, None) => break,
                (false, Some(chunk)) => {
                    if stderr.len() < codec::MAX_LINE {
                        stderr.extend_from_slice(&chunk);
                    }
                }
                (false, None) => stderr_open = false,
            }
        }
        sink.flush().await.map_err(io)?;
        while let Ok(chunk) = exec.stderr_rx.try_recv() {
            if stderr.len() < codec::MAX_LINE {
                stderr.extend_from_slice(&chunk);
            }
        }
        let status = tokio::time::timeout(self.io_timeout, &mut exec.exit_rx)
            .await
            .map_err(|_| ProviderError::Timeout)?
            .unwrap_or(crate::ssh_exec::EXIT_ABNORMAL);
        if status != 0 {
            let out = Output {
                stdout: Vec::new(),
                stderr,
                status,
            };
            return Err(command_error("read", path, &out));
        }
        Ok(done)
    }

    /// Append `len` bytes from `src` to the existing file `path` with
    /// `cat >>`; `progress` gets the bytes sent so far and `len`.
    pub async fn append<R: AsyncRead + Unpin>(
        &self,
        path: &str,
        src: &mut R,
        len: u64,
        progress: &mut (dyn FnMut(u64, u64) + Send),
    ) -> Result<(), ProviderError> {
        let p = shell_quote(path)?;
        let cmd = format!(
            "LC_ALL=C; export LC_ALL; \
             [ -f {p} ] || {{ echo 'append: No such file or directory' >&2; exit 1; }}; \
             cat >> {p}"
        );
        let mut exec = self.exec(cmd.as_bytes()).await?;
        let io = |e: std::io::Error| ProviderError::TransferFailed(format!("{path}: {e}"));
        let limit_bps = self.upload_limit_bps.load(Ordering::Relaxed);
        let start = Instant::now();
        let mut buf = vec![0u8; UPLOAD_CHUNK];
        let mut sent = 0u64;
        while sent < len {
            let want = (len - sent).min(UPLOAD_CHUNK as u64) as usize;
            let n = src.read(&mut buf[..want]).await.map_err(io)?;
            if n == 0 {
                return Err(ProviderError::TransferFailed(format!(
                    "{path}: the source ended after {sent} of {len} bytes"
                )));
            }
            // A `cat` that has already exited (the file was missing) closes
            // its stdin: its own message, below, says why.
            if exec.stdin.write_all(&buf[..n]).await.is_err() {
                break;
            }
            sent += n as u64;
            progress(sent, len);
            crate::throttle_transfer(sent, start.elapsed(), limit_bps).await;
        }
        let _ = exec.stdin.shutdown().await;
        let out = self.collect(exec).await?;
        if out.status != 0 {
            return Err(command_error("append", path, &out));
        }
        if sent < len {
            return Err(ProviderError::TransferFailed(format!(
                "{path}: the server stopped reading after {sent} of {len} bytes"
            )));
        }
        Ok(())
    }

    /// The paths below the folder `root` whose last component `keep`
    /// accepts, in `find` order, at most `limit` of them. `find` does not
    /// follow symlinks, so a link back up the tree is never walked; a folder
    /// it cannot read is skipped, as SFTP's search skips one. Reading stops
    /// at `limit`, and dropping the channel ends `find`.
    ///
    /// `root` itself fails as [`Self::list`] of it fails, as over SFTP: a
    /// folder that is not there or cannot be read is an error, not a search
    /// with no matches.
    pub async fn find_paths(
        &self,
        root: &str,
        keep: &mut (dyn FnMut(&[u8]) -> bool + Send),
        limit: usize,
    ) -> Result<Vec<Vec<u8>>, ProviderError> {
        let folder = shell_quote(&format!("{}/.", root.trim_end_matches('/')))?;
        self.run_ok(
            "list",
            root,
            format!("ls -lan -- {folder} >/dev/null").as_bytes(),
        )
        .await?;
        let cmd = format!(
            "LC_ALL=C; export LC_ALL; find {} -mindepth 1 -print0 2>/dev/null; :",
            shell_quote(root)?
        );
        let mut exec = self.exec(cmd.as_bytes()).await?;
        let mut found = Vec::new();
        let mut pending: Vec<u8> = Vec::new();
        while let Some(chunk) = tokio::time::timeout(self.io_timeout, exec.stdout_rx.recv())
            .await
            .map_err(|_| ProviderError::Timeout)?
        {
            pending.extend_from_slice(&chunk);
            let mut consumed = 0;
            while let Some(at) = pending[consumed..].iter().position(|&b| b == 0) {
                let path = &pending[consumed..consumed + at];
                let name = path.rsplit(|&b| b == b'/').next().unwrap_or(path);
                if keep(name) {
                    found.push(path.to_vec());
                    if found.len() >= limit {
                        return Ok(found);
                    }
                }
                consumed += at + 1;
            }
            pending.drain(..consumed);
            if pending.len() > MAX_PATH_BYTES {
                return Err(ProviderError::ParseError(format!(
                    "find {root}: a path longer than {MAX_PATH_BYTES} bytes"
                )));
            }
        }
        Ok(found)
    }
}

/// The marker the replace script prints when `from` and `to` are of two
/// kinds, followed by `1` or `0` for "is a folder" of each.
const ACROSS: &str = "aeroftp-replace-across";

/// Longer than any path a Linux kernel accepts (PATH_MAX is 4096).
const MAX_PATH_BYTES: usize = 64 * 1024;

/// Total, used and available bytes from `df -Pk` output. The numbers are
/// the three fields before the first one ending in `%`, which keeps a file
/// system or a mount point with a space in its name readable.
fn parse_df(stdout: &[u8]) -> Option<(u64, u64, u64)> {
    let text = String::from_utf8_lossy(stdout);
    let line = text.lines().rev().find(|l| !l.trim().is_empty())?;
    let fields: Vec<&str> = line.split_whitespace().collect();
    let at = fields
        .iter()
        .enumerate()
        .skip(3)
        .find(|(_, f)| f.ends_with('%'))
        .map(|(i, _)| i)?;
    let num = |i: usize| {
        fields[i]
            .parse::<u64>()
            .ok()
            .map(|k| k.saturating_mul(1024))
    };
    Some((num(at - 3)?, num(at - 2)?, num(at - 1)?))
}
