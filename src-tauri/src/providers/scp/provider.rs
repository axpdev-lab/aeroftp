//! SCP as a storage provider: the explicit SCP protocol, and the mode an SFTP
//! connection continues in when its server has no SFTP.
//!
//! Both run on a connection made by [`crate::providers::sftp::ssh_login`], so
//! host-key trust and the order of the authentication methods are exactly
//! those of SFTP. The operations are [`ScpSession`]'s.
//!
//! A download that stopped goes on from its partial, read from that offset
//! by `dd` and `cat`; an upload goes on by appending with `cat >>`. A replace
//! is `mv -f`, the server's `rename(2)`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::Mutex as TokioMutex;

use super::listing::{Entry, Kind, Meta};
use super::session::ScpSession;
use crate::providers::sftp::{
    format_permissions, resolve_remote_path, ssh_login, SharedSshHandle, SshHandler,
};
use crate::providers::shell_quote::{shell_quote, shell_quote_bytes};
use crate::providers::{
    checksum_matrix, ChecksumCapability, ProviderError, ProviderType, RemoteEntry, SftpConfig,
    StorageProvider,
};

/// Names per `[ -d ]` batch when a listing resolves its symlinks.
const SYMLINK_BATCH: usize = 256;

/// The most entries a search returns, as over SFTP.
const FIND_LIMIT: usize = 500;

/// The digests the server computes: algorithm, tool, hex length.
const DIGESTS: &[(&str, &str, usize)] = &[("sha256", "sha256sum", 64), ("md5", "md5sum", 32)];

/// SCP over one authenticated SSH connection.
pub struct ScpProvider {
    /// The SFTP profile's settings: SCP profiles store exactly what SFTP
    /// profiles store.
    config: SftpConfig,
    handle: Option<SharedSshHandle>,
    session: Option<ScpSession<SshHandler>>,
    /// False when the connection belongs to an `SftpProvider` that went on in
    /// SCP mode: that provider dials, reconnects and disconnects it.
    owns_connection: bool,
    home_dir: String,
    current_dir: String,
    /// The accepted host key's SHA-256, filled during the handshake.
    host_key_sha256_hex: Arc<std::sync::OnceLock<String>>,
    /// Transfer rates in bytes per second, 0 for no limit; kept here so a
    /// reconnect starts its session with them.
    upload_limit_bps: u64,
    download_limit_bps: u64,
}

impl ScpProvider {
    pub fn new(config: SftpConfig) -> Self {
        Self {
            config,
            handle: None,
            session: None,
            owns_connection: true,
            home_dir: "/".to_string(),
            current_dir: "/".to_string(),
            host_key_sha256_hex: Arc::new(std::sync::OnceLock::new()),
            upload_limit_bps: 0,
            download_limit_bps: 0,
        }
    }

    /// SCP mode on a connection another provider made and keeps. `config`
    /// names the server for messages and gives the initial path; it is never
    /// used to dial, so the caller passes it without secrets.
    pub(crate) async fn on_connection(
        config: SftpConfig,
        handle: SharedSshHandle,
    ) -> Result<Self, ProviderError> {
        let mut provider = Self::new(config);
        provider.owns_connection = false;
        provider.start(handle).await?;
        Ok(provider)
    }

    pub(crate) fn home_dir(&self) -> &str {
        &self.home_dir
    }

    pub(crate) fn current_dir(&self) -> &str {
        &self.current_dir
    }

    fn io_timeout(&self) -> Duration {
        Duration::from_secs(self.config.timeout_secs.max(1))
    }

    /// Probe the server on `handle` and take the login directory as home.
    async fn start(&mut self, handle: SharedSshHandle) -> Result<(), ProviderError> {
        let session = ScpSession::open(handle.clone(), self.io_timeout()).await?;
        session.set_speed_limits(self.upload_limit_bps, self.download_limit_bps);
        let home = session.probe().home.clone();
        self.home_dir = if home.starts_with('/') {
            home
        } else {
            "/".to_string()
        };
        self.current_dir = match &self.config.initial_path {
            Some(initial) if !initial.is_empty() => {
                resolve_remote_path(&self.home_dir, &self.home_dir, initial)
            }
            _ => self.home_dir.clone(),
        };
        tracing::info!(
            "SCP: {} ({}) ready, home {}",
            self.config.host,
            session.probe().system,
            self.home_dir
        );
        self.handle = Some(handle);
        self.session = Some(session);
        Ok(())
    }

    fn session(&self) -> Result<&ScpSession<SshHandler>, ProviderError> {
        self.session.as_ref().ok_or(ProviderError::NotConnected)
    }

    fn normalize_path(&self, path: &str) -> String {
        resolve_remote_path(&self.current_dir, &self.home_dir, path)
    }

    /// Whether the server has `tool` (always false before connecting).
    fn has(&self, tool: &str) -> bool {
        self.session.as_ref().is_some_and(|s| s.probe().has(tool))
    }

    /// The names among `names` (entries of `dir`) that are directories once
    /// their symlink is followed, in batches of [`SYMLINK_BATCH`].
    async fn directories_among(
        &self,
        dir: &str,
        names: &[&[u8]],
    ) -> Result<std::collections::HashSet<Vec<u8>>, ProviderError> {
        let session = self.session()?;
        let mut found = std::collections::HashSet::new();
        for batch in names.chunks(SYMLINK_BATCH) {
            let mut script = format!("cd -- {} || exit; for n in", shell_quote(dir)?).into_bytes();
            for name in batch {
                script.push(b' ');
                script.extend_from_slice(&shell_quote_bytes(name)?);
            }
            script.extend_from_slice(b"; do [ -d \"$n\" ] && printf '%s\\0' \"$n\"; done; :");
            let out = session.run(&script).await?;
            found.extend(
                out.stdout
                    .split(|&b| b == 0)
                    .filter(|n| !n.is_empty())
                    .map(<[u8]>::to_vec),
            );
        }
        Ok(found)
    }

    /// Whether `path` is a directory once a symlink is followed.
    async fn is_directory(&self, path: &str) -> Result<bool, ProviderError> {
        let cmd = format!("[ -d {} ]", shell_quote(path)?);
        Ok(self.session()?.run(cmd.as_bytes()).await?.status == 0)
    }

    /// Whether the server has what going on from an offset takes.
    fn can_resume(&self) -> bool {
        ["dd", "cat", "head"].iter().all(|t| self.has(t))
    }

    /// A download into the partial SFTP keeps too (`.aerotmp`, or the file
    /// itself in place), going on from it when it was started from the
    /// version the server has now, by its record.
    async fn download_resumable(
        &mut self,
        remote_path: &str,
        local_path: &str,
        explicit_resume: bool,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        use crate::providers::atomic_write::{ResumableFile, ResumeSource};
        use crate::providers::multi_thread::RangeSourceFingerprint;

        let full = self.normalize_path(remote_path);
        tracing::info!("SCP: Downloading {} to {}", full, local_path);
        let meta = self.session()?.stat_followed(&full).await?;
        if meta.kind == Kind::Dir {
            return Err(ProviderError::InvalidPath(format!("{full} is a directory")));
        }
        let total = meta.size;
        let version =
            RangeSourceFingerprint::of(&remote_entry(String::new(), full.clone(), &meta, false));
        let source = ResumeSource::new(
            "scp",
            &format!("{}:{}", self.config.host, self.config.port),
            &self.config.username,
            &full,
        );
        let local_error = |e: std::io::Error| {
            ProviderError::TransferFailed(format!("Failed to create local file: {e}"))
        };
        let mut part =
            ResumableFile::open_for_source(local_path, &version, &source, explicit_resume)
                .await
                .map_err(local_error)?;
        let mut offset = part.offset();
        // A partial longer than the file is not a part of it.
        if offset > total || (offset > 0 && !self.can_resume()) {
            part.discard().await.ok();
            part = ResumableFile::open_fresh(local_path)
                .await
                .map_err(local_error)?;
            offset = 0;
        }
        let progress = move |done: u64, total: u64| {
            if let Some(p) = &on_progress {
                p(done, total);
            }
        };
        let read = if offset == total && total > 0 {
            progress(total, total);
            Ok(())
        } else if offset == 0 {
            let mut progress = progress;
            self.session()?
                .download(&full, part.file_mut(), &mut progress)
                .await
                .map(drop)
        } else {
            tracing::info!("SCP: going on from byte {} of {}", offset, total);
            let mut on_bytes = move |done: u64| progress(offset + done, total);
            let session = self.session()?;
            match session
                .read_at(&full, offset, None, part.file_mut(), &mut on_bytes)
                .await
            {
                Ok(n) if offset + n == total => Ok(()),
                Ok(n) => Err(ProviderError::TransferFailed(format!(
                    "{full}: {} bytes after the partial's {offset}, expected {}; the file \
                     changed during the download",
                    n,
                    total - offset
                ))),
                Err(e) => Err(e),
            }
        };
        match read {
            Ok(()) => part.commit().await.map_err(|e| {
                ProviderError::TransferFailed(format!("Failed to finish download: {e}"))
            }),
            Err(e) => {
                // Kept for the next attempt to go on from, as SFTP keeps it.
                if let Err(local) = part.keep_for_resume().await {
                    return Err(ProviderError::TransferFailed(format!(
                        "Failed to write the download: {local}"
                    )));
                }
                Err(e)
            }
        }
    }
}

fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

fn last_component(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return "/";
    }
    trimmed.rsplit('/').next().unwrap_or(trimmed)
}

/// The entry callers see, in the shape SFTP gives it.
fn remote_entry(name: String, path: String, meta: &Meta, is_dir: bool) -> RemoteEntry {
    let modified = chrono::DateTime::from_timestamp(meta.mtime, 0)
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%SZ").to_string());
    RemoteEntry {
        name,
        path,
        is_dir,
        size: meta.size,
        modified,
        permissions: Some(format_permissions(meta.mode, is_dir)),
        owner: Some(meta.uid.clone()),
        group: Some(meta.gid.clone()),
        is_symlink: meta.kind == Kind::Symlink,
        link_target: None,
        mime_type: None,
        metadata: Default::default(),
    }
}

fn entry_name(entry: &Entry) -> String {
    String::from_utf8_lossy(&entry.name).into_owned()
}

#[async_trait]
impl StorageProvider for ScpProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn provider_type(&self) -> ProviderType {
        ProviderType::Scp
    }

    fn display_name(&self) -> String {
        format!("{}@{}", self.config.username, self.config.host)
    }

    fn endpoint_identity(&self) -> crate::transfer_dag::EndpointIdentity {
        crate::transfer_dag::EndpointIdentity::new("scp", &self.config.host, &self.config.username)
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        if !self.owns_connection {
            // The SFTP provider that holds the connection reconnects it.
            return if self.session.is_some() {
                Ok(())
            } else {
                Err(ProviderError::NotConnected)
            };
        }
        let _ = self.disconnect().await;
        tracing::info!(
            "SCP: Connecting to {}:{}",
            self.config.host,
            self.config.port
        );
        let handle = ssh_login(&self.config, false, self.host_key_sha256_hex.clone()).await?;
        self.start(Arc::new(TokioMutex::new(handle))).await
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.session = None;
        if let Some(handle) = self.handle.take() {
            if self.owns_connection {
                let _ = tokio::time::timeout(Duration::from_secs(2), async move {
                    let guard = handle.lock().await;
                    let _ = guard
                        .disconnect(russh::Disconnect::ByApplication, "", "en")
                        .await;
                })
                .await;
            }
        }
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.session.is_some()
            && self
                .handle
                .as_ref()
                .is_some_and(|h| h.try_lock().map_or(true, |h| !h.is_closed()))
    }

    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let dir = self.normalize_path(path);
        let entries = self.session()?.list(&dir).await?;
        let links: Vec<&[u8]> = entries
            .iter()
            .filter(|e| e.meta.kind == Kind::Symlink)
            .map(|e| e.name.as_slice())
            .collect();
        let link_dirs = if links.is_empty() {
            Default::default()
        } else {
            self.directories_among(&dir, &links).await?
        };
        Ok(entries
            .iter()
            .map(|e| {
                let name = entry_name(e);
                let is_dir = e.meta.kind == Kind::Dir
                    || (e.meta.kind == Kind::Symlink && link_dirs.contains(&e.name));
                let mut entry = remote_entry(name.clone(), join(&dir, &name), &e.meta, is_dir);
                entry.link_target = e
                    .link_target
                    .as_ref()
                    .map(|t| String::from_utf8_lossy(t).into_owned());
                entry
            })
            .collect())
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> {
        Ok(self.current_dir.clone())
    }

    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        let full = self.normalize_path(path);
        // `stat` first, so a missing path reads as not found rather than as
        // not a directory.
        self.session()?.stat(&full).await?;
        if !self.is_directory(&full).await? {
            return Err(ProviderError::InvalidPath(format!(
                "{full} is not a directory"
            )));
        }
        self.current_dir = full;
        Ok(())
    }

    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        self.cd("..").await
    }

    async fn download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        self.download_resumable(remote_path, local_path, false, on_progress)
            .await
    }

    /// As over SFTP, the offset is read off the partial itself, which the
    /// caller measured: one source of truth.
    async fn resume_download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        _offset: u64,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        self.download_resumable(remote_path, local_path, true, on_progress)
            .await
    }

    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        let full = self.normalize_path(remote_path);
        let mut data = Vec::new();
        self.session()?
            .download(&full, &mut data, &mut |_, _| {})
            .await?;
        Ok(data)
    }

    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let full = self.normalize_path(remote_path);
        tracing::info!("SCP: Uploading {} to {}", local_path, full);
        let mut file = tokio::fs::File::open(local_path).await.map_err(|e| {
            ProviderError::TransferFailed(format!("Failed to open local file: {e}"))
        })?;
        let meta = file.metadata().await.map_err(ProviderError::IoError)?;
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs());
        let session = self.session()?;
        // SCP stamps the mode it is given; a file being replaced keeps its own,
        // as it does over SFTP.
        let mode = match session.stat(&full).await {
            Ok(existing) if existing.kind == Kind::File => existing.mode & 0o7777,
            _ => 0o644,
        };
        let mut progress = move |done: u64, total: u64| {
            if let Some(p) = &on_progress {
                p(done, total);
            }
        };
        session
            .upload(&full, &mut file, meta.len(), mode, mtime, &mut progress)
            .await
    }

    fn supports_resume(&self) -> bool {
        self.can_resume()
    }

    fn supports_resume_upload_append(&self) -> bool {
        self.can_resume()
    }

    /// Go on with an interrupted upload by appending the local file's tail
    /// with `cat >>`. The tail starts where the remote file ends, never at
    /// the caller's offset: appending to a remote shorter than that offset
    /// would leave a hole of the wrong bytes. A remote that is not a part of
    /// the local file by its size (missing, empty, longer) is sent whole.
    async fn resume_upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        offset: u64,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        use tokio::io::AsyncSeekExt;

        let full = self.normalize_path(remote_path);
        let local = tokio::fs::metadata(local_path).await.map_err(|e| {
            ProviderError::TransferFailed(format!("Failed to stat local file: {e}"))
        })?;
        let total = local.len();
        let remote = match self.session()?.stat_followed(&full).await {
            Ok(meta) if meta.kind == Kind::File => Some(meta.size),
            Ok(_) | Err(ProviderError::NotFound(_)) => None,
            Err(e) => return Err(e),
        };
        let from = match remote {
            Some(size) if offset > 0 && size > 0 && size <= total && self.can_resume() => size,
            _ => return self.upload(local_path, remote_path, on_progress).await,
        };
        tracing::info!("SCP: Appending {} from byte {} of {}", full, from, total);
        let progress = move |done: u64, total: u64| {
            if let Some(p) = &on_progress {
                p(done, total);
            }
        };
        if from < total {
            let mut file = tokio::fs::File::open(local_path).await.map_err(|e| {
                ProviderError::TransferFailed(format!("Failed to open local file: {e}"))
            })?;
            file.seek(std::io::SeekFrom::Start(from))
                .await
                .map_err(ProviderError::IoError)?;
            let mut on_bytes = move |done: u64, _: u64| progress(from + done, total);
            self.session()?
                .append(&full, &mut file, total - from, &mut on_bytes)
                .await?;
        } else {
            progress(total, total);
        }
        let now = self.session()?.stat_followed(&full).await?.size;
        if now != total {
            return Err(ProviderError::TransferFailed(format!(
                "{full}: {now} bytes after the append, expected {total}"
            )));
        }
        // The source's time, as a whole upload stamps it.
        if let Some(secs) = local
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
        {
            if self.has("touch") {
                let cmd = format!("touch -d @{secs} -- {}", shell_quote(&full)?);
                let _ = self.session()?.run(cmd.as_bytes()).await;
            }
        }
        Ok(())
    }

    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let full = self.normalize_path(path);
        self.session()?.mkdir(&full).await
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        let full = self.normalize_path(path);
        self.session()?.remove_file(&full).await
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let full = self.normalize_path(path);
        self.session()?.remove_dir(&full).await
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        let full = self.normalize_path(path);
        self.session()?.remove_dir_recursive(&full).await
    }

    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let (from, to) = (self.normalize_path(from), self.normalize_path(to));
        self.session()?.rename(&from, &to).await
    }

    async fn replace(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let (from, to) = (self.normalize_path(from), self.normalize_path(to));
        tracing::info!("SCP: Replacing {} with {}", to, from);
        self.session()?.replace(&from, &to).await
    }

    /// `mv -f` over a file is the server's `rename(2)`, which leaves no
    /// moment without a file at the target when both are in one file
    /// system; callers stage the file in the target's own folder.
    async fn supports_atomic_replace(&mut self) -> Result<bool, ProviderError> {
        Ok(self.has("mv"))
    }

    fn atomic_replace_capability(&self) -> crate::transfer_dag::Capability {
        use crate::transfer_dag::Capability;
        match &self.session {
            None => Capability::SupportedAfterProbe,
            Some(_) if self.has("mv") => Capability::Supported,
            Some(_) => Capability::Unsupported,
        }
    }

    async fn storage_info(&mut self) -> Result<crate::providers::StorageInfo, ProviderError> {
        if !self.has("df") {
            return Err(ProviderError::NotSupported(
                "this server has no df, which reports the free space over SCP".to_string(),
            ));
        }
        let dir = self.current_dir.clone();
        let (total, used, free) = self.session()?.disk_space(&dir).await?;
        Ok(crate::providers::StorageInfo {
            used,
            total,
            free,
            versioning_bytes: None,
        })
    }

    /// `find` is one of the tools SCP mode needs, so a connected session
    /// can always search.
    fn supports_find(&self) -> bool {
        self.session.is_some()
    }

    /// The entries below `path` whose name matches `pattern`, at most 500,
    /// as SFTP's search returns them. `find` does not follow symlinks: a
    /// link to a folder is reported, not walked.
    async fn find(&mut self, path: &str, pattern: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let root = self.normalize_path(path);
        let session = self.session()?;
        let mut keep = |name: &[u8]| {
            crate::providers::matches_find_pattern(&String::from_utf8_lossy(name), pattern)
        };
        let paths = session.find_paths(&root, &mut keep, FIND_LIMIT).await?;
        let entries = session.stat_many(&paths).await?;
        Ok(entries
            .iter()
            .map(|e| {
                let path = String::from_utf8_lossy(&e.name).into_owned();
                remote_entry(
                    last_component(&path).to_string(),
                    path.clone(),
                    &e.meta,
                    e.meta.kind == Kind::Dir,
                )
            })
            .collect())
    }

    async fn set_speed_limit(
        &mut self,
        upload_kb: u64,
        download_kb: u64,
    ) -> Result<(), ProviderError> {
        self.upload_limit_bps = upload_kb * 1024;
        self.download_limit_bps = download_kb * 1024;
        if let Some(session) = &self.session {
            session.set_speed_limits(self.upload_limit_bps, self.download_limit_bps);
        }
        tracing::info!(
            "SCP: Speed limits set: download={}KB/s upload={}KB/s",
            download_kb,
            upload_kb
        );
        Ok(())
    }

    async fn get_speed_limit(&mut self) -> Result<(u64, u64), ProviderError> {
        Ok((self.upload_limit_bps / 1024, self.download_limit_bps / 1024))
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        let full = self.normalize_path(path);
        let meta = self.session()?.stat(&full).await?;
        let is_dir = match meta.kind {
            Kind::Dir => true,
            Kind::Symlink => self.is_directory(&full).await?,
            _ => false,
        };
        Ok(remote_entry(
            last_component(&full).to_string(),
            full.clone(),
            &meta,
            is_dir,
        ))
    }

    async fn size(&mut self, path: &str) -> Result<u64, ProviderError> {
        let full = self.normalize_path(path);
        Ok(self.session()?.stat(&full).await?.size)
    }

    async fn exists(&mut self, path: &str) -> Result<bool, ProviderError> {
        let full = self.normalize_path(path);
        match self.session()?.stat(&full).await {
            Ok(_) => Ok(true),
            Err(ProviderError::NotFound(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    async fn keep_alive(&mut self) -> Result<(), ProviderError> {
        let out = self.session()?.run(b":").await.map_err(|e| match e {
            ProviderError::NotConnected => e,
            other => ProviderError::ConnectionLost(format!("SCP keepalive failed: {other}")),
        })?;
        if out.status != 0 {
            return Err(ProviderError::ConnectionLost(format!(
                "SCP keepalive failed: exit status {}",
                out.status
            )));
        }
        Ok(())
    }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        let system = self
            .session
            .as_ref()
            .map(|s| s.probe().system.clone())
            .unwrap_or_default();
        Ok(format!(
            "SCP over SSH: {}:{} (user: {}, system: {}, home: {})",
            self.config.host, self.config.port, self.config.username, system, self.home_dir
        ))
    }

    fn supports_chmod(&self) -> bool {
        true
    }

    async fn chmod(&mut self, path: &str, mode: u32) -> Result<(), ProviderError> {
        let full = self.normalize_path(path);
        self.session()?.chmod(&full, mode).await
    }

    fn supports_checksum(&self) -> bool {
        DIGESTS.iter().any(|(_, tool, _)| self.has(tool))
    }

    /// The matrix's algorithms the server has a tool for.
    fn checksum_capability(&self, _path: &str) -> ChecksumCapability {
        let mut capability = checksum_matrix::capability(ProviderType::Scp);
        capability.algorithms.retain(|algorithm| {
            DIGESTS
                .iter()
                .any(|(name, tool, _)| name == algorithm && self.has(tool))
        });
        if capability.algorithms.is_empty() {
            return ChecksumCapability::default();
        }
        capability
    }

    /// SHA-256 and MD5 computed on the server by `sha256sum` and `md5sum`,
    /// each when the server has it; a digest that does not come is left out
    /// of the map, so callers omit it, as over SFTP.
    async fn checksum(&mut self, path: &str) -> Result<HashMap<String, String>, ProviderError> {
        let mut out = HashMap::new();
        let full = self.normalize_path(path);
        let quoted = shell_quote(&full)?;
        for (name, tool, hex_len) in DIGESTS {
            if !self.has(tool) {
                continue;
            }
            let cmd = format!("{tool} -- {quoted}");
            let Ok(result) = self.session()?.run(cmd.as_bytes()).await else {
                continue;
            };
            if result.status != 0 {
                continue;
            }
            if let Some(token) = String::from_utf8_lossy(&result.stdout)
                .split_whitespace()
                .next()
            {
                let digest = token.to_ascii_lowercase();
                if digest.len() == *hex_len && digest.bytes().all(|b| b.is_ascii_hexdigit()) {
                    out.insert(name.to_string(), digest);
                }
            }
        }
        Ok(out)
    }

    fn transfer_optimization_hints(&self) -> crate::providers::TransferOptimizationHints {
        crate::providers::TransferOptimizationHints {
            supports_resume_download: false,
            supports_resume_upload: false,
            supports_range_download: false,
            supports_compression: false,
            supports_delta_sync: false,
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joined_paths_and_last_components() {
        assert_eq!(join("/", "a b"), "/a b");
        assert_eq!(join("/srv", "x"), "/srv/x");
        assert_eq!(last_component("/srv/x/"), "x");
        assert_eq!(last_component("/"), "/");
    }

    #[test]
    fn an_entry_reads_like_an_sftp_one() {
        let meta = Meta {
            kind: Kind::Symlink,
            mode: 0o777,
            size: 7,
            mtime: 1_700_000_000,
            uid: "0".to_string(),
            gid: "0".to_string(),
        };
        let entry = remote_entry("l".into(), "/l".into(), &meta, true);
        assert!(entry.is_dir && entry.is_symlink);
        assert_eq!(entry.modified.as_deref(), Some("2023-11-14 22:13:20Z"));
        assert_eq!(entry.permissions.as_deref(), Some("drwxrwxrwx"));
    }
}
