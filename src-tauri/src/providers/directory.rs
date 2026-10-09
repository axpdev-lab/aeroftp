//! `StorageProvider` over a directory tree on this machine, behind a path jail.
//!
//! Two front ends share it, and differ only in identity and wording:
//!
//! - **MTP over a desktop mount** ([`DirectoryProvider::mtp`]). A phone grants
//!   one MTP session per physical plug; gvfs takes it at automount, so
//!   exclusive libmtp always loses (`PTP_ERROR_IO`) and can wedge the device.
//!   Browsing the FUSE path as a filesystem while keeping the remote identity
//!   (`ProviderType::Mtp`) is the mechanism proven to work when the desktop
//!   already mounted the phone (APPENDIX-DEVICE-PROFILES live findings
//!   2026-07-16). Exclusive [`super::mtp::MtpProvider`] remains the no-gvfs
//!   fallback.
//! - **A local directory as a remote** ([`DirectoryProvider::local`],
//!   `ProviderType::Local`): what `aeroftp-cli serve webdav /srv/share` or
//!   `aeroftp-cli ls file:///srv/share` open.
//!
//! Virtual paths are `/`-rooted and map 1:1 under the root. Every resolved
//! real path must stay under the canonical root: `..` clamps at `/` by
//! construction, and a symlink that resolves outside is refused.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use async_trait::async_trait;

use crate::providers::mtp::path::{
    join_virtual, leaf_name, normalize_virtual_path, parent_path, split_segments,
};
use crate::providers::mtp::provider::MtpProvider;
use crate::providers::types::{ProviderError, ProviderType, RemoteEntry, StorageInfo};
use crate::providers::StorageProvider;
use crate::transfer_dag::{Capability, TransferCapabilities};

/// Cap for `download_to_bytes` materialization on a phone (10 MiB): every
/// byte crosses USB. Larger objects stream to disk via `download`. A local
/// folder takes the general cap every other backend has.
const MTP_BYTES_CAP: u64 = 10 * 1024 * 1024;

/// Which front end a [`DirectoryProvider`] is: it sets the provider identity
/// and the words its errors use, never the path jail.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DirectoryIdentity {
    /// A portable device browsed through its desktop (gvfs/FUSE) mount.
    Mtp { device_id: String },
    /// A directory of this machine used as a remote.
    Local,
}

/// The folder a `ProviderType::Local` config names: its `host`, which must be
/// an absolute path. A relative one would be resolved against whatever
/// directory the process happens to run in, so it is refused here and made
/// absolute by the caller that knows the user's directory (the CLI).
pub fn local_root_from_config(
    config: &crate::providers::ProviderConfig,
) -> Result<PathBuf, ProviderError> {
    let raw = config.host.trim();
    if raw.is_empty() {
        return Err(ProviderError::InvalidConfig(
            "a local directory remote needs the folder path".to_string(),
        ));
    }
    let root = PathBuf::from(raw);
    if !root.is_absolute() {
        return Err(ProviderError::InvalidConfig(format!(
            "a local directory remote needs an absolute path, got '{raw}'"
        )));
    }
    Ok(root)
}

/// `StorageProvider` over a directory tree rooted at `root_path`.
///
/// Virtual paths are `/`-rooted and map 1:1 under the root. Path jail: every
/// resolved real path must stay under the canonical root (symlink escape
/// refused).
pub struct DirectoryProvider {
    root_path: PathBuf,
    /// Canonicalized root after `connect()`.
    root: Option<PathBuf>,
    identity: DirectoryIdentity,
    display_name: String,
    cwd: String,
}

impl DirectoryProvider {
    /// A portable device through its desktop mount, with the `Mtp` identity so
    /// dual-panel transfers and AeroSync treat it as a remote.
    pub fn mtp(mount_root: PathBuf, device_id: String, display_name: String) -> Self {
        Self {
            root_path: mount_root,
            root: None,
            identity: DirectoryIdentity::Mtp { device_id },
            display_name,
            cwd: "/".to_string(),
        }
    }

    /// A directory of this machine as a remote. The display name is the path
    /// as given, so a served share says which folder it exposes.
    pub fn local(root: PathBuf) -> Self {
        let display_name = root.display().to_string();
        Self {
            root_path: root,
            root: None,
            identity: DirectoryIdentity::Local,
            display_name,
            cwd: "/".to_string(),
        }
    }

    /// What the root is called in an error: the user reads "the MTP mount
    /// root" for a phone and "the served directory" for a local folder.
    fn root_noun(&self) -> &'static str {
        match self.identity {
            DirectoryIdentity::Mtp { .. } => "the MTP mount root",
            DirectoryIdentity::Local => "the served directory",
        }
    }

    /// What a local folder declares: the whole-file, single-slot surface,
    /// plus what the methods of this provider implement for it (resume,
    /// server copy, computed digests). It needs no connection, so agent-info
    /// reads the same value for `local` without opening a folder.
    pub fn local_transfer_capabilities() -> TransferCapabilities {
        let mut caps = MtpProvider::honest_transfer_capabilities();
        caps.preferred_download_segments = Some(
            crate::transfer_settings::download_segments_preference_for(ProviderType::Local),
        );
        caps.resume_download = Capability::Supported;
        caps.resume_upload = Capability::Supported;
        caps.server_side_copy = Capability::Supported;
        caps.server_checksum = Capability::Supported;
        caps
    }

    fn is_local(&self) -> bool {
        self.identity == DirectoryIdentity::Local
    }

    /// The capabilities below read and write files of this machine directly;
    /// a phone behind a desktop mount keeps the whole-file surface, because
    /// every byte of a hash or a copy would cross USB.
    fn local_only(&self, operation: &str) -> Result<(), ProviderError> {
        if self.is_local() {
            Ok(())
        } else {
            Err(ProviderError::NotSupported(operation.to_string()))
        }
    }

    /// Resolve an existing regular file, with the virtual path for errors.
    fn resolve_file(&self, path: &str) -> Result<(String, PathBuf), ProviderError> {
        let vpath = self.virtual_path(path)?;
        let real = self.resolve_existing(&vpath)?;
        if real.is_dir() {
            return Err(ProviderError::InvalidPath(format!(
                "{vpath} is a directory"
            )));
        }
        Ok((vpath, real))
    }

    /// How a connect error names the root it could not open.
    fn root_label(&self) -> &'static str {
        match self.identity {
            DirectoryIdentity::Mtp { .. } => "MTP mount path",
            DirectoryIdentity::Local => "Directory",
        }
    }

    fn root(&self) -> Result<&Path, ProviderError> {
        self.root.as_deref().ok_or(ProviderError::NotConnected)
    }

    /// Resolve `path` against cwd into a normalized virtual path. `..` clamps
    /// at `/` (never escapes the virtual tree by construction).
    fn virtual_path(&self, path: &str) -> Result<String, ProviderError> {
        if path.contains('\0') {
            return Err(ProviderError::InvalidPath(
                "path contains a NUL byte".to_string(),
            ));
        }
        // provider_list_files / provider_change_dir pass "." for "list cwd".
        // Nothing else is trimmed: `report ` and `report` are two files.
        if path.is_empty() || path == "." {
            return Ok(self.cwd.clone());
        }
        let base = if path.starts_with('/') {
            "/"
        } else {
            &self.cwd
        };
        let mut parts: Vec<&str> = base.split('/').filter(|c| !c.is_empty()).collect();
        for comp in path.split('/') {
            match comp {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                other => {
                    if other.contains('\0') {
                        return Err(ProviderError::InvalidPath(
                            "path segment contains a NUL byte".to_string(),
                        ));
                    }
                    parts.push(other);
                }
            }
        }
        if parts.is_empty() {
            Ok("/".to_string())
        } else {
            Ok(format!("/{}", parts.join("/")))
        }
    }

    fn fs_path(&self, virtual_path: &str) -> Result<PathBuf, ProviderError> {
        let root = self.root()?;
        let mut fs = root.to_path_buf();
        for seg in split_segments(virtual_path)? {
            fs.push(seg);
        }
        Ok(fs)
    }

    /// Canonicalize and require the result stays under the mount root.
    fn contained(&self, fs: &Path) -> Result<PathBuf, ProviderError> {
        let root = self.root()?;
        let canon = fs.canonicalize().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ProviderError::NotFound(format!("{}: {e}", fs.display()))
            } else {
                ProviderError::IoError(e)
            }
        })?;
        if !canon.starts_with(root) {
            return Err(ProviderError::InvalidPath(format!(
                "path resolves outside {}",
                self.root_noun()
            )));
        }
        Ok(canon)
    }

    /// Resolve a path that must already exist.
    fn resolve_existing(&self, vpath: &str) -> Result<PathBuf, ProviderError> {
        let fs = self.fs_path(vpath)?;
        self.contained(&fs)
    }

    /// The entry a mutation acts on: its folder must exist inside the root,
    /// and the leaf is the entry itself, not what it points to. Deleting or
    /// renaming `/alias` acts on the link, and leaves the file it names
    /// alone; through `resolve_existing` it reached the target instead.
    fn resolve_entry(&self, vpath: &str) -> Result<PathBuf, ProviderError> {
        let norm = normalize_virtual_path(vpath)?;
        let name = leaf_name(&norm)?;
        let parent = parent_path(&norm)?;
        let entry = self.resolve_existing(&parent)?.join(&name);
        std::fs::symlink_metadata(&entry).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ProviderError::NotFound(format!("{vpath}: {e}"))
            } else {
                ProviderError::IoError(e)
            }
        })?;
        Ok(entry)
    }

    /// Resolve a create target: parent must exist and be contained; leaf is
    /// joined without following a pre-existing symlink on the leaf itself.
    fn resolve_for_create(&self, vpath: &str) -> Result<PathBuf, ProviderError> {
        let norm = normalize_virtual_path(vpath)?;
        if norm == "/" {
            return Err(ProviderError::InvalidPath(format!(
                "cannot create {} itself",
                self.root_noun()
            )));
        }
        let name = leaf_name(&norm)?;
        let parent = parent_path(&norm)?;
        let parent_fs = self.resolve_existing(&parent)?;
        let target = parent_fs.join(&name);
        // Refuse if a symlink at the target already points outside.
        if target.exists() || target.symlink_metadata().is_ok() {
            let _ = self.contained(&target)?;
        }
        Ok(target)
    }

    /// What a listing shows for the symbolic link at `link`: the metadata of
    /// the file it points to when that file is inside the root, so the size
    /// and date listed are those a download reads. A link to a folder is not
    /// followed, because a walk through it can loop back to an ancestor, and a
    /// link that leaves the root or points nowhere cannot be read; those
    /// answer `None` and the listing names them in a warning.
    fn linked_file(&self, link: &Path) -> Option<std::fs::Metadata> {
        let target = self.contained(link).ok()?;
        let meta = std::fs::metadata(target).ok()?;
        meta.is_file().then_some(meta)
    }

    fn entry_for(
        &self,
        name: String,
        virtual_path: String,
        meta: &std::fs::Metadata,
        is_symlink: bool,
    ) -> RemoteEntry {
        let modified = meta
            .modified()
            .ok()
            .map(chrono::DateTime::<chrono::Utc>::from)
            .map(|t| t.to_rfc3339());
        #[cfg(unix)]
        let permissions = {
            use std::os::unix::fs::PermissionsExt;
            Some(format!("{:o}", meta.permissions().mode() & 0o777))
        };
        #[cfg(not(unix))]
        let permissions = None;
        RemoteEntry {
            name,
            path: virtual_path,
            is_dir: meta.is_dir(),
            size: if meta.is_dir() { 0 } else { meta.len() },
            modified,
            permissions,
            owner: None,
            group: None,
            is_symlink,
            link_target: None,
            mime_type: None,
            metadata: Default::default(),
        }
    }

    /// Top-level directories under the mount, exposed as session "storages" for
    /// the open toast (matches exclusive MTP storage list shape).
    pub async fn list_storage_roots(&self) -> Result<Vec<(String, String)>, ProviderError> {
        let root = self.root()?;
        let mut rd = tokio::fs::read_dir(root)
            .await
            .map_err(ProviderError::IoError)?;
        let mut out = Vec::new();
        while let Some(item) = rd.next_entry().await.map_err(ProviderError::IoError)? {
            let meta = match tokio::fs::symlink_metadata(item.path()).await {
                Ok(m) => m,
                Err(_) => continue,
            };
            if !meta.is_dir() {
                continue;
            }
            let name = item.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            let vpath = join_virtual("/", &name)?;
            out.push((name, vpath));
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }
}

#[async_trait]
impl StorageProvider for DirectoryProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn provider_type(&self) -> ProviderType {
        match self.identity {
            DirectoryIdentity::Mtp { .. } => ProviderType::Mtp,
            DirectoryIdentity::Local => ProviderType::Local,
        }
    }

    fn display_name(&self) -> String {
        self.display_name.clone()
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        let label = self.root_label();
        let meta = tokio::fs::metadata(&self.root_path).await.map_err(|e| {
            ProviderError::ConnectionFailed(format!(
                "{label} {} is not available: {e}",
                self.root_path.display()
            ))
        })?;
        if !meta.is_dir() {
            return Err(ProviderError::ConnectionFailed(format!(
                "{label} {} is not a directory",
                self.root_path.display()
            )));
        }
        let canon = self.root_path.canonicalize().map_err(|e| {
            ProviderError::ConnectionFailed(format!(
                "cannot resolve {label} {}: {e}",
                self.root_path.display()
            ))
        })?;
        self.root = Some(canon);
        self.cwd = "/".to_string();
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        // Do not unmount gvfs: Nautilus keeps the desktop session. We only drop
        // our ProviderState slot (card "connected" ends; attach-dot stays green).
        self.root = None;
        self.cwd = "/".to_string();
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.root.is_some()
    }

    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let vpath = self.virtual_path(path)?;
        let dir = self.resolve_existing(&vpath)?;
        let meta = tokio::fs::metadata(&dir)
            .await
            .map_err(|e| ProviderError::NotFound(format!("{vpath}: {e}")))?;
        if !meta.is_dir() {
            return Err(ProviderError::InvalidPath(format!(
                "{vpath} is not a directory"
            )));
        }
        let mut rd = tokio::fs::read_dir(&dir)
            .await
            .map_err(ProviderError::IoError)?;
        let mut entries = Vec::new();
        let mut left_out = Vec::new();
        while let Some(item) = rd.next_entry().await.map_err(ProviderError::IoError)? {
            let name = item.file_name().to_string_lossy().to_string();
            let Ok(meta) = tokio::fs::symlink_metadata(item.path()).await else {
                left_out.push(name);
                continue;
            };
            let (meta, is_symlink) = if meta.file_type().is_symlink() {
                match self.linked_file(&item.path()) {
                    Some(target) => (target, true),
                    None => {
                        left_out.push(name);
                        continue;
                    }
                }
            } else {
                (meta, false)
            };
            let ventry = join_virtual(&vpath, &name).unwrap_or_else(|_| {
                if vpath == "/" {
                    format!("/{name}")
                } else {
                    format!("{vpath}/{name}")
                }
            });
            entries.push(self.entry_for(name, ventry, &meta, is_symlink));
        }
        if !left_out.is_empty() {
            left_out.sort();
            let shown = left_out
                .iter()
                .take(5)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            let more = left_out.len().saturating_sub(5);
            let more = if more > 0 {
                format!(" and {more} more")
            } else {
                String::new()
            };
            crate::providers::report_warning(format!(
                "{vpath}: {} item(s) not listed: {shown}{more}. A symbolic link is listed \
                 only when it points to a file inside {}; a link to a folder, one that \
                 leaves it, one that points nowhere and an item that cannot be read \
                 are left out.",
                left_out.len(),
                self.root_noun()
            ));
        }
        Ok(entries)
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> {
        self.root()?;
        Ok(self.cwd.clone())
    }

    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        let vpath = self.virtual_path(path)?;
        let fs = self.resolve_existing(&vpath)?;
        let meta = tokio::fs::metadata(&fs)
            .await
            .map_err(|e| ProviderError::NotFound(format!("{vpath}: {e}")))?;
        if !meta.is_dir() {
            return Err(ProviderError::InvalidPath(format!(
                "{vpath} is not a directory"
            )));
        }
        self.cwd = vpath;
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
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let vpath = self.virtual_path(remote_path)?;
        let src = self.resolve_existing(&vpath)?;
        let meta = tokio::fs::metadata(&src)
            .await
            .map_err(|e| ProviderError::NotFound(format!("{vpath}: {e}")))?;
        if meta.is_dir() {
            return Err(ProviderError::InvalidPath(format!(
                "{vpath} is a directory"
            )));
        }
        let total = meta.len();
        let mut reader = tokio::fs::File::open(&src)
            .await
            .map_err(ProviderError::IoError)?;
        if let Some(parent) = Path::new(local_path).parent() {
            if !parent.as_os_str().is_empty() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(ProviderError::IoError)?;
            }
        }
        let mut writer = tokio::fs::File::create(local_path)
            .await
            .map_err(ProviderError::IoError)?;
        let mut buf = vec![0u8; 1024 * 1024];
        let mut done: u64 = 0;
        loop {
            let n = reader
                .read(&mut buf)
                .await
                .map_err(ProviderError::IoError)?;
            if n == 0 {
                break;
            }
            writer
                .write_all(&buf[..n])
                .await
                .map_err(ProviderError::IoError)?;
            done += n as u64;
            if let Some(ref cb) = on_progress {
                cb(done, total);
            }
        }
        writer.flush().await.map_err(ProviderError::IoError)?;
        Ok(())
    }

    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        let cap = match self.identity {
            DirectoryIdentity::Mtp { .. } => MTP_BYTES_CAP,
            DirectoryIdentity::Local => super::MAX_DOWNLOAD_TO_BYTES,
        };
        self.download_to_bytes_capped(remote_path, cap).await
    }

    /// `len` bytes from `offset`, fewer at the end of the file and none past
    /// it (a served SFTP READ takes an empty answer as EOF). What `serve sftp`
    /// and a resumed `serve ftp` RETR read, so a file is served a window at a
    /// time instead of whole into memory.
    async fn read_range(
        &mut self,
        path: &str,
        offset: u64,
        len: u64,
    ) -> Result<Vec<u8>, ProviderError> {
        use tokio::io::{AsyncReadExt, AsyncSeekExt};
        let vpath = self.virtual_path(path)?;
        let src = self.resolve_existing(&vpath)?;
        let mut file = tokio::fs::File::open(&src)
            .await
            .map_err(ProviderError::IoError)?;
        let meta = file.metadata().await.map_err(ProviderError::IoError)?;
        if meta.is_dir() {
            return Err(ProviderError::InvalidPath(format!(
                "{vpath} is a directory"
            )));
        }
        let size = meta.len();
        if offset >= size {
            return Ok(Vec::new());
        }
        let want = len.min(size - offset).min(super::MAX_DOWNLOAD_TO_BYTES);
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(ProviderError::IoError)?;
        let mut buf = Vec::with_capacity(want as usize);
        (&mut file)
            .take(want)
            .read_to_end(&mut buf)
            .await
            .map_err(ProviderError::IoError)?;
        Ok(buf)
    }

    async fn download_to_bytes_capped(
        &mut self,
        remote_path: &str,
        max_bytes: u64,
    ) -> Result<Vec<u8>, ProviderError> {
        let vpath = self.virtual_path(remote_path)?;
        let src = self.resolve_existing(&vpath)?;
        let meta = tokio::fs::metadata(&src)
            .await
            .map_err(|e| ProviderError::NotFound(format!("{vpath}: {e}")))?;
        if meta.is_dir() {
            return Err(ProviderError::InvalidPath(format!(
                "{vpath} is a directory"
            )));
        }
        if meta.len() > max_bytes {
            return Err(ProviderError::TransferFailed(format!(
                "{vpath} exceeded the {:.0} MB in-memory cap (stream to disk instead).",
                max_bytes as f64 / 1_048_576.0,
            )));
        }
        let data = tokio::fs::read(&src)
            .await
            .map_err(ProviderError::IoError)?;
        if data.len() as u64 > max_bytes {
            return Err(ProviderError::TransferFailed(format!(
                "{vpath} exceeded the {:.0} MB in-memory cap (stream to disk instead).",
                max_bytes as f64 / 1_048_576.0,
            )));
        }
        Ok(data)
    }

    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let vpath = self.virtual_path(remote_path)?;
        if vpath == "/" {
            return Err(ProviderError::InvalidPath(format!(
                "cannot upload onto {} itself; name a file inside it",
                self.root_noun()
            )));
        }
        let dest = self.resolve_for_create(&vpath)?;
        let meta = tokio::fs::metadata(local_path)
            .await
            .map_err(|e| ProviderError::NotFound(format!("{local_path}: {e}")))?;
        if meta.is_dir() {
            return Err(ProviderError::InvalidPath(
                "upload source must be a file".to_string(),
            ));
        }
        // Creating the destination truncates it: when it is the source itself
        // (the same path, a link to it, or a second hard link), the read that
        // follows finds nothing and the file is lost while the upload reports
        // success.
        if let Ok(occupant) = std::fs::metadata(&dest) {
            let source = std::fs::metadata(local_path).map_err(ProviderError::IoError)?;
            if same_file(Path::new(local_path), &dest, &source, &occupant) {
                return Err(ProviderError::InvalidPath(format!(
                    "{vpath} is the upload source itself"
                )));
            }
        }
        let total = meta.len();
        let mut reader = tokio::fs::File::open(local_path)
            .await
            .map_err(ProviderError::IoError)?;
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(ProviderError::IoError)?;
        }
        let mut writer = tokio::fs::File::create(&dest)
            .await
            .map_err(ProviderError::IoError)?;
        let mut buf = vec![0u8; 1024 * 1024];
        let mut done: u64 = 0;
        loop {
            let n = reader
                .read(&mut buf)
                .await
                .map_err(ProviderError::IoError)?;
            if n == 0 {
                break;
            }
            writer
                .write_all(&buf[..n])
                .await
                .map_err(ProviderError::IoError)?;
            done += n as u64;
            if let Some(ref cb) = on_progress {
                cb(done, total);
            }
        }
        writer.flush().await.map_err(ProviderError::IoError)?;
        if self.identity == DirectoryIdentity::Local {
            // A copy into a local folder keeps the source's modification time,
            // as `cp -p` does: a sync compares by
            // time, and a stored copy dated "now" reads as newer than the file
            // it was made from on the next run.
            if let Ok(modified) = meta.modified() {
                let file = writer.into_std().await;
                if let Err(e) = file.set_modified(modified) {
                    crate::providers::report_warning(format!(
                        "{vpath}: uploaded, but its modification time could not be kept ({e})"
                    ));
                }
            }
        }
        Ok(())
    }

    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let vpath = self.virtual_path(path)?;
        let dest = self.resolve_for_create(&vpath)?;
        tokio::fs::create_dir(&dest)
            .await
            .map_err(ProviderError::IoError)?;
        Ok(())
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        let vpath = self.virtual_path(path)?;
        if vpath == "/" {
            return Err(ProviderError::NotSupported(format!(
                "cannot delete {}",
                self.root_noun()
            )));
        }
        let fs = self.resolve_entry(&vpath)?;
        let meta = tokio::fs::symlink_metadata(&fs)
            .await
            .map_err(|e| ProviderError::NotFound(format!("{vpath}: {e}")))?;
        if meta.is_dir() && !meta.file_type().is_symlink() {
            return Err(ProviderError::InvalidPath(format!(
                "{vpath} is a directory; use rmdir"
            )));
        }
        tokio::fs::remove_file(&fs)
            .await
            .map_err(ProviderError::IoError)?;
        Ok(())
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let vpath = self.virtual_path(path)?;
        if vpath == "/" {
            return Err(ProviderError::NotSupported(format!(
                "cannot delete {}",
                self.root_noun()
            )));
        }
        let fs = self.resolve_entry(&vpath)?;
        // `remove_dir` never recurses: the operating system refuses a
        // directory that is not empty, which is said as such.
        tokio::fs::remove_dir(&fs).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::DirectoryNotEmpty {
                ProviderError::DirectoryNotEmpty(format!(
                    "{path} holds entries; delete it recursively to remove it with its content ({e})"
                ))
            } else {
                ProviderError::IoError(e)
            }
        })?;
        Ok(())
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        let vpath = self.virtual_path(path)?;
        if vpath == "/" {
            return Err(ProviderError::NotSupported(format!(
                "cannot delete {}",
                self.root_noun()
            )));
        }
        // On a link `remove_dir_all` removes the link and not the folder it
        // names.
        let fs = self.resolve_entry(&vpath)?;
        tokio::fs::remove_dir_all(&fs)
            .await
            .map_err(ProviderError::IoError)?;
        Ok(())
    }

    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let from_v = self.virtual_path(from)?;
        let to_v = self.virtual_path(to)?;
        if from_v == "/" || to_v == "/" {
            return Err(ProviderError::InvalidPath(format!(
                "cannot rename {}",
                self.root_noun()
            )));
        }
        let src = self.resolve_entry(&from_v)?;
        let dest = self.resolve_for_create(&to_v)?;
        if src == dest {
            return Ok(());
        }
        // rename(2) replaces a file at the destination (and an empty folder
        // with a folder): the trait promises no overwrite. A case-insensitive
        // mount finds the source itself under the new spelling, which is no
        // other item. The look and the rename are two calls; a file created
        // in between is still replaced.
        // A look that fails for another reason than an absence says nothing
        // about the destination, and read as free it let rename(2) go.
        match std::fs::symlink_metadata(&dest) {
            Ok(occupant) => {
                let source = std::fs::symlink_metadata(&src).map_err(ProviderError::IoError)?;
                if !same_file(&src, &dest, &source, &occupant) {
                    return Err(ProviderError::AlreadyExists(to.to_string()));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(ProviderError::IoError(e)),
        }
        tokio::fs::rename(&src, &dest)
            .await
            .map_err(ProviderError::IoError)?;
        Ok(())
    }

    /// rename(2), which puts the new file in place of the old in one step.
    async fn replace(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let from_v = self.virtual_path(from)?;
        let to_v = self.virtual_path(to)?;
        if from_v == "/" || to_v == "/" {
            return Err(ProviderError::InvalidPath(format!(
                "cannot rename {}",
                self.root_noun()
            )));
        }
        let src = self.resolve_entry(&from_v)?;
        let dest = self.resolve_for_create(&to_v)?;
        match std::fs::symlink_metadata(&dest) {
            Ok(occupant) => {
                let source = std::fs::symlink_metadata(&src).map_err(ProviderError::IoError)?;
                super::refuse_replace_across_types(to, source.is_dir(), occupant.is_dir())?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(ProviderError::IoError(e)),
        }
        tokio::fs::rename(&src, &dest)
            .await
            .map_err(ProviderError::IoError)
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        let vpath = self.virtual_path(path)?;
        let fs = self.resolve_existing(&vpath)?;
        let meta = tokio::fs::symlink_metadata(&fs)
            .await
            .map_err(|e| ProviderError::NotFound(format!("{vpath}: {e}")))?;
        // `fs` is the resolved target; whether the name itself is a link is
        // asked of the unresolved path.
        let is_symlink = self
            .fs_path(&vpath)
            .ok()
            .and_then(|p| std::fs::symlink_metadata(p).ok())
            .is_some_and(|m| m.file_type().is_symlink());
        let name = if vpath == "/" {
            self.display_name.clone()
        } else {
            leaf_name(&vpath).unwrap_or_else(|_| vpath.clone())
        };
        Ok(self.entry_for(name, vpath, &meta, is_symlink))
    }

    async fn size(&mut self, path: &str) -> Result<u64, ProviderError> {
        let vpath = self.virtual_path(path)?;
        let fs = self.resolve_existing(&vpath)?;
        let meta = tokio::fs::metadata(&fs)
            .await
            .map_err(|e| ProviderError::NotFound(format!("{vpath}: {e}")))?;
        Ok(meta.len())
    }

    async fn exists(&mut self, path: &str) -> Result<bool, ProviderError> {
        let vpath = self.virtual_path(path)?;
        match self.resolve_existing(&vpath) {
            Ok(_) => Ok(true),
            Err(ProviderError::NotFound(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    async fn keep_alive(&mut self) -> Result<(), ProviderError> {
        // Re-check the mount is still there (unplug vanishes the FUSE path).
        let root = self.root()?;
        if !root.is_dir() {
            self.root = None;
            return Err(ProviderError::NotConnected);
        }
        Ok(())
    }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        Ok(match &self.identity {
            DirectoryIdentity::Mtp { device_id } => format!(
                "MTP portable device (gvfs/filesystem): {} ({device_id}) at {}",
                self.display_name,
                self.root_path.display()
            ),
            DirectoryIdentity::Local => {
                format!("Local directory: {}", self.root_path.display())
            }
        })
    }

    fn supports_checksum(&self) -> bool {
        self.is_local()
    }

    async fn checksum(&mut self, path: &str) -> Result<HashMap<String, String>, ProviderError> {
        self.checksum_for(path, "sha256").await
    }

    /// The digest the caller names, computed by reading the file: a folder of
    /// this machine stores none, but reading it costs what the caller would
    /// pay to hash a download, without the copy. An algorithm outside the
    /// list is absent from the map, and the caller falls back as it does for
    /// any backend that lacks it.
    async fn checksum_for(
        &mut self,
        path: &str,
        algorithm: &str,
    ) -> Result<HashMap<String, String>, ProviderError> {
        self.local_only("checksum")?;
        let (_, real) = self.resolve_file(path)?;
        let algorithm = algorithm.to_ascii_lowercase();
        let digest = {
            let algorithm = algorithm.clone();
            tokio::task::spawn_blocking(move || file_digest(&real, &algorithm))
                .await
                .map_err(|e| ProviderError::Other(format!("hash worker failed: {e}")))?
                .map_err(ProviderError::IoError)?
        };
        Ok(digest.map(|hex| (algorithm, hex)).into_iter().collect())
    }

    /// The filesystem that holds the folder, as `df` shows it.
    async fn storage_info(&mut self) -> Result<StorageInfo, ProviderError> {
        self.local_only("storage_info")?;
        let root = self.root()?.to_path_buf();
        let space = tokio::task::spawn_blocking(move || crate::filesystem::filesystem_space(&root))
            .await
            .map_err(|e| ProviderError::Other(format!("statvfs worker failed: {e}")))?
            .map_err(ProviderError::IoError)?;
        Ok(StorageInfo {
            used: space.total.saturating_sub(space.free),
            total: space.total,
            free: space.available,
            versioning_bytes: None,
        })
    }

    fn supports_server_copy(&self) -> bool {
        self.is_local()
    }

    /// Copy a file inside the folder. The copy is written beside the
    /// destination and renamed onto it, so a reader never sees half a file
    /// and an interrupted copy leaves the destination as it was. A file at
    /// the destination is replaced, as `cp` does; a folder there is refused.
    /// A link at the destination is written through to the file it names
    /// (inside the root), as `cp` and `upload` do, instead of being replaced.
    async fn server_copy(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        self.local_only("server_copy")?;
        let (_, src) = self.resolve_file(from)?;
        let to_v = self.virtual_path(to)?;
        let mut dest = self.resolve_for_create(&to_v)?;
        if dest
            .symlink_metadata()
            .is_ok_and(|meta| meta.file_type().is_symlink())
        {
            dest = self.contained(&dest)?;
        }
        if dest.is_dir() {
            return Err(ProviderError::AlreadyExists(format!(
                "{to_v} is a directory"
            )));
        }
        let parent = dest
            .parent()
            .ok_or_else(|| ProviderError::InvalidPath(format!("{to_v} has no folder")))?
            .to_path_buf();
        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            let staged = tempfile::Builder::new()
                .prefix(".aeroftp-copy-")
                .tempfile_in(&parent)?;
            std::fs::copy(&src, staged.path())?;
            staged.persist(&dest).map_err(|e| e.error)?;
            Ok(())
        })
        .await
        .map_err(|e| ProviderError::Other(format!("copy worker failed: {e}")))?
        .map_err(ProviderError::IoError)
    }

    fn supports_resume(&self) -> bool {
        self.is_local()
    }

    fn supports_resume_upload_append(&self) -> bool {
        self.is_local()
    }

    /// Append the file from `offset` to the local copy, which must hold
    /// exactly `offset` bytes: a shorter or longer one is not the beginning
    /// of this file, and appending to it would build a wrong file silently.
    async fn resume_download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        offset: u64,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        self.local_only("resume_download")?;
        let (_, src) = self.resolve_file(remote_path)?;
        append_from(&src, Path::new(local_path), offset, on_progress).await
    }

    /// Append the local file from `offset` to the stored one, which must hold
    /// exactly `offset` bytes, for the same reason.
    async fn resume_upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        offset: u64,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        self.local_only("resume_upload")?;
        let (_, dest) = self.resolve_file(remote_path)?;
        append_from(Path::new(local_path), &dest, offset, on_progress).await
    }

    fn transfer_capabilities(&self) -> TransferCapabilities {
        match self.identity {
            // Same honest surface as exclusive libmtp: whole-file, single slot.
            DirectoryIdentity::Mtp { .. } => {
                let mut caps = MtpProvider::honest_transfer_capabilities();
                caps.preferred_download_segments = Some(
                    crate::transfer_settings::download_segments_preference_for(ProviderType::Mtp),
                );
                caps
            }
            DirectoryIdentity::Local => Self::local_transfer_capabilities(),
        }
    }

    fn supports_delta_sync(&self) -> bool {
        false
    }
}

/// The hex digest of a file for one of the algorithms a local folder offers,
/// `None` for any other.
fn file_digest(path: &Path, algorithm: &str) -> std::io::Result<Option<String>> {
    use std::io::Read;
    fn stream<D: sha2::Digest>(path: &Path) -> std::io::Result<String> {
        let mut file = std::fs::File::open(path)?;
        let mut hasher = D::new();
        let mut buf = vec![0u8; 1024 * 1024];
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        Ok(hex::encode(hasher.finalize()))
    }
    Ok(Some(match algorithm {
        "md5" => stream::<md5::Md5>(path)?,
        "sha1" => stream::<sha1::Sha1>(path)?,
        "sha256" => stream::<sha2::Sha256>(path)?,
        "sha512" => stream::<sha2::Sha512>(path)?,
        "blake3" => {
            let mut hasher = blake3::Hasher::new();
            hasher.update_reader(std::fs::File::open(path)?)?;
            hasher.finalize().to_hex().to_string()
        }
        _ => return Ok(None),
    }))
}

/// Append `from[offset..]` to `to`, which must hold exactly `offset` bytes.
async fn append_from(
    from: &Path,
    to: &Path,
    offset: u64,
    on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
) -> Result<(), ProviderError> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
    let held = tokio::fs::metadata(to)
        .await
        .map_err(ProviderError::IoError)?
        .len();
    if held != offset {
        return Err(ProviderError::TransferFailed(format!(
            "cannot resume at byte {offset}: {} holds {held} bytes",
            to.display()
        )));
    }
    let mut reader = tokio::fs::File::open(from)
        .await
        .map_err(ProviderError::IoError)?;
    let total = reader
        .metadata()
        .await
        .map_err(ProviderError::IoError)?
        .len();
    if offset > total {
        return Err(ProviderError::TransferFailed(format!(
            "cannot resume at byte {offset}: the source holds {total} bytes"
        )));
    }
    reader
        .seek(std::io::SeekFrom::Start(offset))
        .await
        .map_err(ProviderError::IoError)?;
    let mut writer = tokio::fs::OpenOptions::new()
        .append(true)
        .open(to)
        .await
        .map_err(ProviderError::IoError)?;
    let mut buf = vec![0u8; 1024 * 1024];
    let mut done = offset;
    loop {
        let n = reader
            .read(&mut buf)
            .await
            .map_err(ProviderError::IoError)?;
        if n == 0 {
            break;
        }
        writer
            .write_all(&buf[..n])
            .await
            .map_err(ProviderError::IoError)?;
        done += n as u64;
        if let Some(ref cb) = on_progress {
            cb(done, total);
        }
    }
    writer.flush().await.map_err(ProviderError::IoError)?;
    Ok(())
}

/// Whether `a` and `b` (with their metadata) are one file: the same inode
/// on the same device on Unix; elsewhere the same canonical path, which a
/// case-insensitive volume gives both spellings of one name. Two files that
/// merely share a size and times are two files.
fn same_file(
    a: &std::path::Path,
    b: &std::path::Path,
    a_meta: &std::fs::Metadata,
    b_meta: &std::fs::Metadata,
) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let _ = (a, b);
        a_meta.dev() == b_meta.dev() && a_meta.ino() == b_meta.ino()
    }
    #[cfg(not(unix))]
    {
        let _ = (a_meta, b_meta);
        match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    async fn connected(root: &Path) -> DirectoryProvider {
        let mut p = DirectoryProvider::mtp(
            root.to_path_buf(),
            "dev-test".to_string(),
            "Test Phone".to_string(),
        );
        p.connect().await.expect("connect");
        p
    }

    /// `tokio::fs::rename` is POSIX rename(2), which replaces the file at the
    /// destination: a rename onto an existing file destroyed it and answered
    /// Ok. It is now refused, and nothing is touched; a rename to a free name
    /// and one that only changes the letter case still go through.
    #[tokio::test]
    async fn rename_refuses_an_existing_destination_and_allows_a_case_change() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.txt"), b"A").unwrap();
        std::fs::write(dir.path().join("b.txt"), b"B").unwrap();
        std::fs::create_dir(dir.path().join("d")).unwrap();
        let mut p = connected(dir.path()).await;

        for to in ["/b.txt", "/d"] {
            let outcome = p.rename("/a.txt", to).await;
            assert!(
                matches!(outcome, Err(ProviderError::AlreadyExists(_))),
                "{to}: {outcome:?}"
            );
        }
        assert_eq!(std::fs::read(dir.path().join("b.txt")).unwrap(), b"B");
        assert_eq!(std::fs::read(dir.path().join("a.txt")).unwrap(), b"A");

        p.rename("/a.txt", "/A.txt")
            .await
            .expect("case-only rename");
        assert_eq!(std::fs::read(dir.path().join("A.txt")).unwrap(), b"A");
        p.rename("/A.txt", "/c.txt").await.expect("free name");
        assert_eq!(std::fs::read(dir.path().join("c.txt")).unwrap(), b"A");
    }

    /// The case-only rename above meets its own source only on a
    /// case-insensitive volume, so on a case-sensitive one it could not
    /// fail. A hard link is a second name for the same file on any Unix
    /// volume: the look must take it for the source, not for another item.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_second_name_for_the_same_file_is_not_another_item() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.txt"), b"A").unwrap();
        std::fs::hard_link(dir.path().join("a.txt"), dir.path().join("link.txt")).unwrap();
        let mut p = connected(dir.path()).await;
        p.rename("/a.txt", "/link.txt")
            .await
            .expect("the same file under two names is no other item");
        // rename(2) between two names of one file does nothing: both stay.
        assert_eq!(std::fs::read(dir.path().join("link.txt")).unwrap(), b"A");
        assert_eq!(std::fs::read(dir.path().join("a.txt")).unwrap(), b"A");
    }

    /// `replace` keeps rename(2), which puts the new file in place in one step.
    #[tokio::test]
    async fn replace_puts_the_new_file_in_place() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.txt"), b"A").unwrap();
        std::fs::write(dir.path().join("b.txt"), b"B").unwrap();
        let mut p = connected(dir.path()).await;
        p.replace("/a.txt", "/b.txt").await.expect("replace");
        assert_eq!(std::fs::read(dir.path().join("b.txt")).unwrap(), b"A");
        assert!(!dir.path().join("a.txt").exists());
    }

    #[tokio::test]
    async fn browses_lists_and_transfers() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join("Internal shared storage")).unwrap();
        std::fs::create_dir(dir.path().join("Internal shared storage/DCIM")).unwrap();
        std::fs::write(
            dir.path().join("Internal shared storage/DCIM/IMG_001.JPG"),
            b"JPEG",
        )
        .unwrap();

        let mut p = connected(dir.path()).await;
        assert!(p.is_connected());
        assert_eq!(p.provider_type(), ProviderType::Mtp);
        assert_eq!(p.pwd().await.unwrap(), "/");

        let root = p.list("/").await.unwrap();
        assert_eq!(root.len(), 1);
        assert_eq!(root[0].name, "Internal shared storage");
        assert!(root[0].is_dir);

        p.cd("/Internal shared storage/DCIM").await.unwrap();
        assert_eq!(p.pwd().await.unwrap(), "/Internal shared storage/DCIM");
        let files_dot = p.list(".").await.unwrap();
        assert!(
            files_dot.iter().any(|e| e.name == "IMG_001.JPG"),
            "list(\".\") must list cwd: {files_dot:?}"
        );

        assert_eq!(p.download_to_bytes("IMG_001.JPG").await.unwrap(), b"JPEG");

        let out_dir = tempfile::tempdir().unwrap();
        let dest = out_dir.path().join("photo.jpg");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_cb = seen.clone();
        p.download(
            "/Internal shared storage/DCIM/IMG_001.JPG",
            dest.to_str().unwrap(),
            Some(Box::new(move |done, total| {
                seen_cb.lock().unwrap().push((done, total));
            })),
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"JPEG");
        assert_eq!(seen.lock().unwrap().last().copied(), Some((4, 4)));

        let upload_src = out_dir.path().join("note.txt");
        std::fs::write(&upload_src, b"hello gvfs").unwrap();
        p.upload(
            upload_src.to_str().unwrap(),
            "/Internal shared storage/DCIM/note.txt",
            None,
        )
        .await
        .unwrap();
        assert!(dir
            .path()
            .join("Internal shared storage/DCIM/note.txt")
            .exists());

        p.mkdir("/Internal shared storage/DCIM/Album")
            .await
            .unwrap();
        assert!(dir
            .path()
            .join("Internal shared storage/DCIM/Album")
            .is_dir());

        p.rename(
            "/Internal shared storage/DCIM/note.txt",
            "/Internal shared storage/DCIM/Album/note.txt",
        )
        .await
        .unwrap();
        assert!(dir
            .path()
            .join("Internal shared storage/DCIM/Album/note.txt")
            .exists());

        p.delete("/Internal shared storage/DCIM/Album/note.txt")
            .await
            .unwrap();
        p.rmdir("/Internal shared storage/DCIM/Album")
            .await
            .unwrap();

        p.disconnect().await.unwrap();
        assert!(!p.is_connected());
    }

    #[tokio::test]
    async fn path_jail_clamps_dotdot_and_refuses_symlink_escape() {
        let outside = tempfile::tempdir().expect("outside");
        std::fs::write(outside.path().join("secret.txt"), b"top-secret").unwrap();
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("ok.txt"), b"fine").unwrap();

        let mut p = connected(dir.path()).await;

        // `..` clamps at virtual root.
        assert_eq!(
            p.download_to_bytes("../../../ok.txt").await.unwrap(),
            b"fine"
        );
        assert!(!p.exists("../../../etc/hostname").await.unwrap());

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                outside.path().join("secret.txt"),
                dir.path().join("leak.txt"),
            )
            .unwrap();
            let err = p.download_to_bytes("leak.txt").await;
            assert!(
                matches!(err, Err(ProviderError::InvalidPath(_))),
                "symlink escape must be refused, got {err:?}"
            );
            // Listed only when it points to a file inside the root: this one
            // leaves it, so the listing leaves it out and says so.
            let warnings = crate::providers::CallWarnings::default();
            let entries = warnings.scope(p.list("/")).await.unwrap();
            assert!(entries.iter().all(|e| e.name != "leak.txt"), "{entries:?}");
            let said = warnings.take();
            assert!(said.len() == 1 && said[0].contains("leak.txt"), "{said:?}");
        }
    }

    #[tokio::test]
    async fn connect_requires_existing_directory() {
        let mut p = DirectoryProvider::mtp(
            PathBuf::from("/no/such/mtp/mount/path-xyz"),
            "dev".into(),
            "x".into(),
        );
        let err = p.connect().await.unwrap_err();
        assert!(matches!(err, ProviderError::ConnectionFailed(_)));
    }

    #[tokio::test]
    async fn storage_roots_are_top_level_dirs() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("Internal shared storage")).unwrap();
        std::fs::create_dir(dir.path().join("SD card")).unwrap();
        std::fs::write(dir.path().join("readme.txt"), b"x").unwrap();

        let p = connected(dir.path()).await;
        let roots = p.list_storage_roots().await.unwrap();
        let names: Vec<_> = roots.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["Internal shared storage", "SD card"]);
    }

    #[tokio::test]
    async fn keep_alive_fails_when_mount_vanishes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        let mut p = connected(&path).await;
        // Drop the tempdir (unplug simulation).
        drop(dir);
        let err = p.keep_alive().await.unwrap_err();
        assert!(matches!(err, ProviderError::NotConnected));
        assert!(!p.is_connected());
    }

    async fn local(root: &Path) -> DirectoryProvider {
        let mut p = DirectoryProvider::local(root.to_path_buf());
        p.connect().await.expect("connect");
        p
    }

    /// A local folder is its own backend: it says `Local`, names the folder,
    /// and its errors speak of the served directory, not of a phone.
    #[tokio::test]
    async fn a_local_directory_has_its_own_identity() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut p = local(dir.path()).await;
        assert_eq!(p.provider_type(), ProviderType::Local);
        assert_eq!(p.display_name(), dir.path().display().to_string());
        let info = p.server_info().await.unwrap();
        assert!(info.starts_with("Local directory: "), "{info}");

        let refused = p.delete("/").await.unwrap_err();
        assert!(
            matches!(&refused, ProviderError::NotSupported(m) if m == "cannot delete the served directory"),
            "{refused:?}"
        );
        let refused = p.rmdir_recursive("/").await.unwrap_err();
        assert!(
            matches!(refused, ProviderError::NotSupported(_)),
            "{refused:?}"
        );
        let src = dir.path().join("a.txt");
        std::fs::write(&src, b"A").unwrap();
        let refused = p
            .upload(src.to_str().unwrap(), "/", None)
            .await
            .unwrap_err();
        assert!(
            matches!(&refused, ProviderError::InvalidPath(m) if m.contains("the served directory itself")),
            "{refused:?}"
        );

        let mut missing = DirectoryProvider::local(dir.path().join("nope"));
        let refused = missing.connect().await.unwrap_err();
        assert!(
            matches!(&refused, ProviderError::ConnectionFailed(m) if m.starts_with("Directory ")),
            "{refused:?}"
        );
    }

    /// Every verb a served share or a CLI command uses, on a real folder.
    #[tokio::test]
    async fn a_local_directory_lists_transfers_and_edits() {
        let dir = tempfile::tempdir().expect("tempdir");
        let work = tempfile::tempdir().expect("work");
        let mut p = local(dir.path()).await;

        let src = work.path().join("note.txt");
        std::fs::write(&src, b"hello local").unwrap();
        p.mkdir("/docs").await.unwrap();
        p.upload(src.to_str().unwrap(), "/docs/note.txt", None)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read(dir.path().join("docs/note.txt")).unwrap(),
            b"hello local"
        );

        let listed = p.list("/docs").await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].path, "/docs/note.txt");
        assert_eq!(listed[0].size, 11);
        assert!(!listed[0].is_symlink);
        assert_eq!(p.stat("/docs/note.txt").await.unwrap().size, 11);
        assert!(p.exists("/docs/note.txt").await.unwrap());

        let back = work.path().join("back.txt");
        p.download("/docs/note.txt", back.to_str().unwrap(), None)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&back).unwrap(), b"hello local");

        p.rename("/docs/note.txt", "/docs/renamed.txt")
            .await
            .unwrap();
        assert!(dir.path().join("docs/renamed.txt").is_file());
        p.delete("/docs/renamed.txt").await.unwrap();
        p.rmdir("/docs").await.unwrap();
        assert!(!dir.path().join("docs").exists());

        p.mkdir("/tree").await.unwrap();
        p.mkdir("/tree/sub").await.unwrap();
        std::fs::write(dir.path().join("tree/sub/f.txt"), b"f").unwrap();
        p.rmdir_recursive("/tree").await.unwrap();
        assert!(!dir.path().join("tree").exists());
    }

    /// The jail holds for writes too: `..` clamps at the root, and a folder
    /// that is a link to somewhere else takes no upload, folder or rename,
    /// and nothing lands outside.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_local_directory_refuses_writes_that_leave_it() {
        let outside = tempfile::tempdir().expect("outside");
        let dir = tempfile::tempdir().expect("tempdir");
        let work = tempfile::tempdir().expect("work");
        std::os::unix::fs::symlink(outside.path(), dir.path().join("out")).unwrap();
        let src = work.path().join("x.txt");
        std::fs::write(&src, b"x").unwrap();
        std::fs::write(dir.path().join("a.txt"), b"A").unwrap();
        let mut p = local(dir.path()).await;

        p.upload(src.to_str().unwrap(), "../../escape.txt", None)
            .await
            .unwrap();
        assert!(dir.path().join("escape.txt").is_file());

        let up = p.upload(src.to_str().unwrap(), "/out/x.txt", None).await;
        assert!(matches!(up, Err(ProviderError::InvalidPath(_))), "{up:?}");
        let made = p.mkdir("/out/new").await;
        assert!(
            matches!(made, Err(ProviderError::InvalidPath(_))),
            "{made:?}"
        );
        let moved = p.rename("/a.txt", "/out/a.txt").await;
        assert!(
            matches!(moved, Err(ProviderError::InvalidPath(_))),
            "{moved:?}"
        );
        std::fs::write(outside.path().join("secret.txt"), b"s").unwrap();
        let read = p.download_to_bytes("/out/secret.txt").await;
        assert!(
            matches!(read, Err(ProviderError::InvalidPath(_))),
            "{read:?}"
        );

        let landed: Vec<_> = std::fs::read_dir(outside.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(landed, vec!["secret.txt"], "nothing may land outside");
        assert_eq!(std::fs::read(dir.path().join("a.txt")).unwrap(), b"A");
    }

    /// A link to a file inside the root is listed with the size and date a
    /// download reads; a link to a folder (a walk through it can loop), one
    /// that leaves the root and one that points nowhere are left out, and
    /// the listing says which.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_listing_shows_the_links_it_can_read_and_names_the_rest() {
        let outside = tempfile::tempdir().expect("outside");
        std::fs::write(outside.path().join("secret.txt"), b"top-secret").unwrap();
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("real.txt"), b"twelve bytes").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let link = |target: &Path, name: &str| {
            std::os::unix::fs::symlink(target, dir.path().join(name)).unwrap()
        };
        link(&dir.path().join("real.txt"), "inside.txt");
        link(&dir.path().join("sub"), "loop");
        link(&outside.path().join("secret.txt"), "leak.txt");
        link(&dir.path().join("gone.txt"), "dangling.txt");
        let mut p = local(dir.path()).await;

        let warnings = crate::providers::CallWarnings::default();
        let entries = warnings.scope(p.list("/")).await.unwrap();
        let mut names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["inside.txt", "real.txt", "sub"]);
        let inside = entries.iter().find(|e| e.name == "inside.txt").unwrap();
        assert!(inside.is_symlink && !inside.is_dir);
        assert_eq!(inside.size, 12);
        assert_eq!(
            p.download_to_bytes("/inside.txt").await.unwrap(),
            b"twelve bytes"
        );

        let said = warnings.take();
        assert_eq!(said.len(), 1, "{said:?}");
        for name in ["dangling.txt", "leak.txt", "loop"] {
            assert!(said[0].contains(name), "{name} unnamed in {said:?}");
        }
        assert!(said[0].starts_with("/: 3 item(s) not listed"), "{said:?}");

        let stat = p.stat("/inside.txt").await.unwrap();
        assert!(stat.is_symlink && stat.size == 12, "{stat:?}");
    }

    /// A served folder reads a window at a time, and a whole file up to the
    /// general cap: the 10 MiB phone cap refused an 11 MiB file to every
    /// `serve` mode. A phone keeps its cap.
    #[tokio::test]
    async fn a_local_directory_reads_ranges_and_files_past_the_phone_cap() {
        let dir = tempfile::tempdir().expect("tempdir");
        let big: Vec<u8> = (0..11 * 1024 * 1024u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(dir.path().join("big.bin"), &big).unwrap();
        std::fs::write(dir.path().join("small.txt"), b"0123456789").unwrap();

        let mut p = local(dir.path()).await;
        assert_eq!(
            p.download_to_bytes("/big.bin").await.unwrap().len(),
            big.len()
        );
        assert_eq!(p.read_range("/small.txt", 2, 3).await.unwrap(), b"234");
        assert_eq!(p.read_range("/small.txt", 8, 100).await.unwrap(), b"89");
        assert!(p.read_range("/small.txt", 10, 4).await.unwrap().is_empty());
        let window = p.read_range("/big.bin", 5_000_000, 4).await.unwrap();
        assert_eq!(window, big[5_000_000..5_000_004].to_vec());

        let mut phone = connected(dir.path()).await;
        assert!(phone.download_to_bytes("/big.bin").await.is_err());
    }

    /// An upload into a local folder keeps the source's modification time, so
    /// the next sync does not read the copy as newer than its source.
    #[tokio::test]
    async fn an_upload_into_a_local_directory_keeps_the_source_time() {
        let dir = tempfile::tempdir().expect("tempdir");
        let work = tempfile::tempdir().expect("work");
        let src = work.path().join("old.txt");
        std::fs::write(&src, b"old").unwrap();
        let then =
            std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000);
        std::fs::File::options()
            .write(true)
            .open(&src)
            .unwrap()
            .set_modified(then)
            .unwrap();
        let mut p = local(dir.path()).await;
        p.upload(src.to_str().unwrap(), "/old.txt", None)
            .await
            .unwrap();
        let stored = std::fs::metadata(dir.path().join("old.txt"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(stored, then);
    }

    /// Deleting, renaming or removing a link acts on the link: the file or
    /// folder it names stays. Through the target, a delete of `/alias`
    /// removed the file and left the link dangling.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_mutation_on_a_link_acts_on_the_link_not_its_target() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("real.txt"), b"keep").unwrap();
        std::fs::create_dir(dir.path().join("folder")).unwrap();
        std::fs::write(dir.path().join("folder/inside.txt"), b"keep").unwrap();
        let link = |target: &str, name: &str| {
            std::os::unix::fs::symlink(dir.path().join(target), dir.path().join(name)).unwrap()
        };
        link("real.txt", "alias.txt");
        link("real.txt", "moved-from.txt");
        link("folder", "folder-link");
        let mut p = local(dir.path()).await;

        p.delete("/alias.txt").await.unwrap();
        assert!(dir.path().join("alias.txt").symlink_metadata().is_err());
        p.rename("/moved-from.txt", "/moved-to.txt").await.unwrap();
        let moved = dir.path().join("moved-to.txt").symlink_metadata().unwrap();
        assert!(moved.file_type().is_symlink());
        p.rmdir_recursive("/folder-link").await.unwrap();
        assert!(dir.path().join("folder-link").symlink_metadata().is_err());

        assert_eq!(std::fs::read(dir.path().join("real.txt")).unwrap(), b"keep");
        assert_eq!(
            std::fs::read(dir.path().join("folder/inside.txt")).unwrap(),
            b"keep"
        );
    }

    /// ` report` and `report` are two files: a path is not trimmed into
    /// another one. A trailing space is checked only where the filesystem
    /// keeps it: Win32 strips trailing spaces from a name, so on Windows the
    /// fixture `report ` would be created as `report` and there would be no
    /// second file to tell apart.
    #[tokio::test]
    async fn names_that_differ_by_spaces_are_different_files() {
        let mut spaced = vec![" report"];
        if cfg!(not(windows)) {
            spaced.push("report ");
        }
        for name in spaced {
            let dir = tempfile::tempdir().expect("tempdir");
            std::fs::write(dir.path().join("report"), b"plain").unwrap();
            std::fs::write(dir.path().join(name), b"spaced").unwrap();
            let mut p = local(dir.path()).await;
            let vpath = format!("/{name}");
            assert_eq!(
                p.download_to_bytes(&vpath).await.unwrap(),
                b"spaced",
                "{name:?}"
            );
            assert_eq!(
                p.download_to_bytes("/report").await.unwrap(),
                b"plain",
                "{name:?}"
            );
            p.delete(&vpath).await.unwrap();
            assert_eq!(
                std::fs::read(dir.path().join("report")).unwrap(),
                b"plain",
                "{name:?}"
            );
        }
    }

    /// An upload of a file onto itself truncated it and reported success; it
    /// is refused, by path and through a second hard link, and the file
    /// keeps its bytes.
    #[tokio::test]
    async fn an_upload_onto_its_own_source_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("data.bin");
        std::fs::write(&src, b"precious").unwrap();
        let mut p = local(dir.path()).await;
        let same = p.upload(src.to_str().unwrap(), "/data.bin", None).await;
        assert!(
            matches!(same, Err(ProviderError::InvalidPath(_))),
            "{same:?}"
        );
        #[cfg(unix)]
        {
            std::fs::hard_link(&src, dir.path().join("twin.bin")).unwrap();
            let twin = p.upload(src.to_str().unwrap(), "/twin.bin", None).await;
            assert!(
                matches!(twin, Err(ProviderError::InvalidPath(_))),
                "{twin:?}"
            );
        }
        assert_eq!(std::fs::read(&src).unwrap(), b"precious");
    }

    /// A local folder answers the digest the caller names by reading the
    /// file; an algorithm it does not compute is absent, and a phone offers
    /// none (each byte would cross USB).
    #[tokio::test]
    async fn a_local_directory_computes_the_digest_it_is_asked_for() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("abc.txt"), b"abc").unwrap();
        let mut p = local(dir.path()).await;
        assert!(p.supports_checksum());
        for (algorithm, hex) in [
            ("md5", "900150983cd24fb0d6963f7d28e17f72"),
            ("sha1", "a9993e364706816aba3e25717850c26c9cd0d89d"),
            ("sha256", "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"),
            ("sha512", "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"),
            ("blake3", "6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85"),
        ] {
            let map = p.checksum_for("/abc.txt", algorithm).await.unwrap();
            assert_eq!(map.get(algorithm).map(String::as_str), Some(hex), "{algorithm}");
        }
        assert!(p
            .checksum_for("/abc.txt", "quickxor")
            .await
            .unwrap()
            .is_empty());
        assert!(p.checksum("/abc.txt").await.unwrap().contains_key("sha256"));
        assert_eq!(
            p.checksum_capability("/abc.txt").algorithms,
            vec!["md5", "sha1", "sha256", "sha512", "blake3"]
        );

        let mut phone = connected(dir.path()).await;
        assert!(!phone.supports_checksum());
        assert!(matches!(
            phone.checksum_for("/abc.txt", "sha256").await,
            Err(ProviderError::NotSupported(_))
        ));
    }

    /// `df` on a local folder reports the filesystem that holds it.
    #[tokio::test]
    async fn a_local_directory_reports_its_filesystem() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut p = local(dir.path()).await;
        let info = p.storage_info().await.unwrap();
        let space = crate::filesystem::filesystem_space(dir.path()).unwrap();
        assert_eq!(info.total, space.total);
        assert!(info.total > 0 && info.free <= info.total && info.used <= info.total);

        let mut phone = connected(dir.path()).await;
        assert!(matches!(
            phone.storage_info().await,
            Err(ProviderError::NotSupported(_))
        ));
    }

    /// A copy inside the folder: a new name, a file replaced as `cp` does, a
    /// folder at the destination refused, a link out of the root refused,
    /// and no staged copy left behind.
    #[tokio::test]
    async fn a_local_directory_copies_a_file_inside_itself() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.txt"), b"A").unwrap();
        std::fs::write(dir.path().join("b.txt"), b"old").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let mut p = local(dir.path()).await;
        assert!(p.supports_server_copy());

        p.server_copy("/a.txt", "/sub/a.txt").await.unwrap();
        assert_eq!(std::fs::read(dir.path().join("sub/a.txt")).unwrap(), b"A");
        p.server_copy("/a.txt", "/b.txt").await.unwrap();
        assert_eq!(std::fs::read(dir.path().join("b.txt")).unwrap(), b"A");
        let onto_folder = p.server_copy("/a.txt", "/sub").await;
        assert!(
            matches!(onto_folder, Err(ProviderError::AlreadyExists(_))),
            "{onto_folder:?}"
        );
        let folder = p.server_copy("/sub", "/sub2").await;
        assert!(
            matches!(folder, Err(ProviderError::InvalidPath(_))),
            "{folder:?}"
        );

        #[cfg(unix)]
        {
            let outside = tempfile::tempdir().expect("outside");
            std::os::unix::fs::symlink(outside.path(), dir.path().join("out")).unwrap();
            let escaped = p.server_copy("/a.txt", "/out/a.txt").await;
            assert!(
                matches!(escaped, Err(ProviderError::InvalidPath(_))),
                "{escaped:?}"
            );
            assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
        }
        let staged: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .chain(std::fs::read_dir(dir.path().join("sub")).unwrap())
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(".aeroftp-copy-"))
            .collect();
        assert!(staged.is_empty(), "{staged:?}");
    }

    /// A transfer resumes from the byte the partial copy holds, in either
    /// direction; a partial copy of another length is refused and left as it
    /// was, since appending to it would build a wrong file without a word.
    #[tokio::test]
    async fn a_local_directory_resumes_from_the_bytes_already_held() {
        let dir = tempfile::tempdir().expect("tempdir");
        let work = tempfile::tempdir().expect("work");
        std::fs::write(dir.path().join("f.bin"), b"0123456789").unwrap();
        let mut p = local(dir.path()).await;
        assert!(p.supports_resume() && p.supports_resume_upload_append());

        let part = work.path().join("f.part");
        std::fs::write(&part, b"0123").unwrap();
        p.resume_download("/f.bin", part.to_str().unwrap(), 4, None)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&part).unwrap(), b"0123456789");

        std::fs::write(&part, b"01").unwrap();
        let wrong = p
            .resume_download("/f.bin", part.to_str().unwrap(), 4, None)
            .await;
        assert!(
            matches!(wrong, Err(ProviderError::TransferFailed(_))),
            "{wrong:?}"
        );
        assert_eq!(std::fs::read(&part).unwrap(), b"01");

        std::fs::write(dir.path().join("up.bin"), b"abc").unwrap();
        let src = work.path().join("up.bin");
        std::fs::write(&src, b"abcdef").unwrap();
        p.resume_upload(src.to_str().unwrap(), "/up.bin", 3, None)
            .await
            .unwrap();
        assert_eq!(std::fs::read(dir.path().join("up.bin")).unwrap(), b"abcdef");

        let caps = p.transfer_capabilities();
        assert!(caps.resume_download.is_available() && caps.resume_upload.is_available());
        assert!(caps.server_side_copy.is_available() && caps.server_checksum.is_available());
        let phone = connected(dir.path()).await;
        let caps = phone.transfer_capabilities();
        assert!(!caps.resume_download.is_available() && !caps.server_checksum.is_available());
    }

    /// agent-info answers for `local` with what the provider declares, not
    /// with capabilities derived from generic hints.
    #[tokio::test]
    async fn agent_info_and_the_provider_declare_the_same_local_capabilities() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = local(dir.path()).await;
        let live = serde_json::to_value(p.transfer_capabilities()).unwrap();
        let declared = serde_json::to_value(
            crate::agent_session::transfer_capabilities_for_protocol("local").expect("local"),
        )
        .unwrap();
        assert_eq!(declared, live);
    }

    /// A copy onto a link writes the file the link names, as `cp` and
    /// `upload` do; the link stays a link.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_copy_onto_a_link_writes_the_file_it_names() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.txt"), b"new").unwrap();
        std::fs::write(dir.path().join("b.txt"), b"old").unwrap();
        std::os::unix::fs::symlink(dir.path().join("b.txt"), dir.path().join("l.txt")).unwrap();
        let mut p = local(dir.path()).await;
        p.server_copy("/a.txt", "/l.txt").await.unwrap();
        assert_eq!(std::fs::read(dir.path().join("b.txt")).unwrap(), b"new");
        let link = dir.path().join("l.txt").symlink_metadata().unwrap();
        assert!(link.file_type().is_symlink());
    }
}
