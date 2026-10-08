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

use std::path::{Path, PathBuf};

use async_trait::async_trait;

use crate::providers::mtp::path::{
    join_virtual, leaf_name, normalize_virtual_path, parent_path, split_segments,
};
use crate::providers::mtp::provider::MtpProvider;
use crate::providers::types::{ProviderError, ProviderType, RemoteEntry};
use crate::providers::StorageProvider;
use crate::transfer_dag::TransferCapabilities;

/// Cap for `download_to_bytes` materialization (10 MiB). Larger objects stream
/// to disk via `download`.
const BYTES_CAP: u64 = 10 * 1024 * 1024;

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
        let trimmed = path.trim();
        // provider_list_files / provider_change_dir pass "." for "list cwd".
        if trimmed.is_empty() || trimmed == "." {
            return Ok(self.cwd.clone());
        }
        let base = if trimmed.starts_with('/') {
            "/"
        } else {
            &self.cwd
        };
        let mut parts: Vec<&str> = base.split('/').filter(|c| !c.is_empty()).collect();
        for comp in trimmed.split('/') {
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
        self.download_to_bytes_capped(remote_path, BYTES_CAP).await
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
        let fs = self.resolve_existing(&vpath)?;
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
        let fs = self.resolve_existing(&vpath)?;
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
        let fs = self.resolve_existing(&vpath)?;
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
        let src = self.resolve_existing(&from_v)?;
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
        let src = self.resolve_existing(&from_v)?;
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

    fn transfer_capabilities(&self) -> TransferCapabilities {
        // Same honest surface as exclusive libmtp: whole-file, single slot.
        // Nothing here ranges, resumes or runs in parallel yet, and a
        // capability is declared only where the code above provides it.
        let mut caps = MtpProvider::honest_transfer_capabilities();
        caps.preferred_download_segments = Some(
            crate::transfer_settings::download_segments_preference_for(self.provider_type()),
        );
        caps
    }

    fn supports_delta_sync(&self) -> bool {
        false
    }

    fn supports_resume(&self) -> bool {
        false
    }
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
}
