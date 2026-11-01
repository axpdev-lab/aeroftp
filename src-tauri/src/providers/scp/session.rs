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

use std::sync::Arc;
use std::time::Duration;

use russh::client::{Handle, Handler};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot, Mutex as TokioMutex};

use super::codec::{self, Control, Reply};
use super::listing::{self, Entry};
use crate::providers::shell_quote::{shell_quote, shell_quote_bytes};
use crate::providers::ProviderError;
use crate::ssh_exec::{ssh_exec, ExecSession};

/// Cap on what one command may print (a listing of about 500 000 entries).
const MAX_OUTPUT: usize = 64 * 1024 * 1024;

/// Chunk size for the data of an upload.
const UPLOAD_CHUNK: usize = 32 * 1024;

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
    probe: Probe,
    /// The longest wait for the next bytes from the server.
    io_timeout: Duration,
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

    /// Exactly `n` bytes into `sink`, calling `progress` with the running total.
    async fn copy_to<W: AsyncWrite + Unpin>(
        &mut self,
        n: u64,
        sink: &mut W,
        progress: &mut dyn FnMut(u64),
    ) -> Result<(), ProviderError> {
        let mut done = 0u64;
        while done < n {
            self.fill().await?;
            let avail = (self.buf.len() - self.pos) as u64;
            let take = avail.min(n - done) as usize;
            sink.write_all(&self.buf[self.pos..self.pos + take])
                .await
                .map_err(|e| ProviderError::TransferFailed(format!("writing the download: {e}")))?;
            self.pos += take;
            done += take as u64;
            progress(done);
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
            probe: Probe::default(),
            io_timeout,
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

    async fn exec(&self, command: &[u8]) -> Result<ExecSession, ProviderError> {
        ssh_exec(self.handle.clone(), command)
            .await
            .map_err(ProviderError::ConnectionFailed)
    }

    /// Run `command` under `LC_ALL=C` and collect its output. A command that
    /// prints more than [`MAX_OUTPUT`] is an error, never a silently cut
    /// result.
    pub async fn run(&self, command: &[u8]) -> Result<Output, ProviderError> {
        let mut full = b"LC_ALL=C; export LC_ALL; ".to_vec();
        full.extend_from_slice(command);
        let mut exec = self.exec(&full).await?;
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
            let script = listing::resolve_script(target.as_bytes(), &merged.unresolved)?;
            let out = self.run(&script).await?;
            merged
                .entries
                .extend(listing::parse_resolved(&merged.unresolved, &out.stdout));
        }
        Ok(merged.entries)
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
    /// so far.
    pub async fn upload<R: AsyncRead + Unpin>(
        &self,
        path: &str,
        src: &mut R,
        size: u64,
        mode: u32,
        mtime: Option<u64>,
        progress: &mut dyn FnMut(u64),
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
            progress(sent);
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
    /// the mtime the server sent. The server's `C` line must name the file
    /// that was asked for (CVE-2019-6111).
    pub async fn download<W: AsyncWrite + Unpin>(
        &self,
        path: &str,
        sink: &mut W,
        progress: &mut dyn FnMut(u64),
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
        incoming.copy_to(size, sink, progress).await?;
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
}
