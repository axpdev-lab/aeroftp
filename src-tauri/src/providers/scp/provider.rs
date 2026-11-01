//! SCP as a storage provider: the explicit SCP protocol, and the mode an SFTP
//! connection continues in when its server has no SFTP.
//!
//! Both run on a connection made by [`crate::providers::sftp::ssh_login`], so
//! host-key trust and the order of the authentication methods are exactly
//! those of SFTP. The operations are [`ScpSession`]'s.
//!
//! What SCP cannot do is answered as such, never emulated: no resume, no
//! ranged reads, one transfer at a time on the connection's session.

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
        let full = self.normalize_path(remote_path);
        tracing::info!("SCP: Downloading {} to {}", full, local_path);
        let mut atomic = crate::providers::atomic_write::AtomicFile::new(local_path)
            .await
            .map_err(ProviderError::IoError)?;
        let mut progress = move |done: u64, total: u64| {
            if let Some(p) = &on_progress {
                p(done, total);
            }
        };
        self.session()?
            .download(&full, atomic.file_mut(), &mut progress)
            .await?;
        atomic.commit().await.map_err(ProviderError::IoError)
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
        self.has("sha256sum")
    }

    fn checksum_capability(&self, _path: &str) -> ChecksumCapability {
        if self.has("sha256sum") {
            checksum_matrix::capability(ProviderType::Scp)
        } else {
            ChecksumCapability::default()
        }
    }

    /// SHA-256 computed on the server by `sha256sum`; an empty map when the
    /// server cannot give one, so callers omit the hash, as over SFTP.
    async fn checksum(&mut self, path: &str) -> Result<HashMap<String, String>, ProviderError> {
        let mut out = HashMap::new();
        if !self.has("sha256sum") {
            return Ok(out);
        }
        let full = self.normalize_path(path);
        let cmd = format!("sha256sum -- {}", shell_quote(&full)?);
        let Ok(result) = self.session()?.run(cmd.as_bytes()).await else {
            return Ok(out);
        };
        if result.status != 0 {
            return Ok(out);
        }
        if let Some(token) = String::from_utf8_lossy(&result.stdout)
            .split_whitespace()
            .next()
        {
            let digest = token.to_ascii_lowercase();
            if digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()) {
                out.insert("sha256".to_string(), digest);
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
