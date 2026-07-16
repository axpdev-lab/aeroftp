//! Filesystem-backed MTP provider: ride a desktop gvfs (or other FUSE) mount.
//!
//! Why this exists (APPENDIX-DEVICE-PROFILES live findings 2026-07-16): a phone
//! grants **one MTP session per physical plug**. gvfs takes it at automount, so
//! exclusive libmtp always loses (`PTP_ERROR_IO`) and can wedge the device. The
//! only mechanism proven to work when the desktop already mounted the phone is
//! browsing its FUSE path as a filesystem while keeping the **remote** panel
//! identity (`ProviderType::Mtp`) so dual-panel transfers and AeroSync still
//! apply.
//!
//! Exclusive [`super::provider::MtpProvider`] remains the no-gvfs fallback.

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

/// Marker in `ProviderConfig.extra` when this backend is installed.
pub const MTP_EXTRA_BACKEND: &str = "mtp_backend";
pub const MTP_BACKEND_GVFS: &str = "gvfs";
pub const MTP_EXTRA_MOUNT_PATH: &str = "mtp_mount_path";

/// `StorageProvider` over a directory tree rooted at a desktop MTP mount.
///
/// Virtual paths are `/`-rooted and map 1:1 under `mount_root`. Path jail:
/// every resolved real path must stay under the canonical mount root (symlink
/// escape refused).
pub struct MtpFsProvider {
    mount_root: PathBuf,
    /// Canonicalized root after `connect()`.
    root: Option<PathBuf>,
    device_id: String,
    display_name: String,
    cwd: String,
}

impl MtpFsProvider {
    pub fn new(mount_root: PathBuf, device_id: String, display_name: String) -> Self {
        Self {
            mount_root,
            root: None,
            device_id,
            display_name,
            cwd: "/".to_string(),
        }
    }

    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    pub fn mount_root_display(&self) -> String {
        self.mount_root.display().to_string()
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
            return Err(ProviderError::InvalidPath(
                "path resolves outside the MTP mount root".to_string(),
            ));
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
            return Err(ProviderError::InvalidPath(
                "cannot create at device root".to_string(),
            ));
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

    fn entry_for(
        &self,
        name: String,
        virtual_path: String,
        meta: &std::fs::Metadata,
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
            is_symlink: meta.file_type().is_symlink(),
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
impl StorageProvider for MtpFsProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn provider_type(&self) -> ProviderType {
        ProviderType::Mtp
    }

    fn display_name(&self) -> String {
        self.display_name.clone()
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        let meta = tokio::fs::metadata(&self.mount_root).await.map_err(|e| {
            ProviderError::ConnectionFailed(format!(
                "MTP mount path {} is not available: {e}",
                self.mount_root.display()
            ))
        })?;
        if !meta.is_dir() {
            return Err(ProviderError::ConnectionFailed(format!(
                "MTP mount path {} is not a directory",
                self.mount_root.display()
            )));
        }
        let canon = self.mount_root.canonicalize().map_err(|e| {
            ProviderError::ConnectionFailed(format!(
                "cannot resolve MTP mount {}: {e}",
                self.mount_root.display()
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
        while let Some(item) = rd.next_entry().await.map_err(ProviderError::IoError)? {
            let name = item.file_name().to_string_lossy().to_string();
            let meta = match tokio::fs::symlink_metadata(item.path()).await {
                Ok(m) => m,
                Err(_) => continue,
            };
            let ventry = join_virtual(&vpath, &name).unwrap_or_else(|_| {
                if vpath == "/" {
                    format!("/{name}")
                } else {
                    format!("{vpath}/{name}")
                }
            });
            entries.push(self.entry_for(name, ventry, &meta));
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
                "MTP object exceeded the {:.0} MB in-memory cap (stream to disk instead).",
                max_bytes as f64 / 1_048_576.0,
            )));
        }
        let data = tokio::fs::read(&src)
            .await
            .map_err(ProviderError::IoError)?;
        if data.len() as u64 > max_bytes {
            return Err(ProviderError::TransferFailed(format!(
                "MTP object exceeded the {:.0} MB in-memory cap (stream to disk instead).",
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
            return Err(ProviderError::InvalidPath(
                "cannot upload to device root; pick a folder first".to_string(),
            ));
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
            return Err(ProviderError::NotSupported(
                "cannot delete the MTP mount root".to_string(),
            ));
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
            return Err(ProviderError::NotSupported(
                "cannot delete the MTP mount root".to_string(),
            ));
        }
        let fs = self.resolve_existing(&vpath)?;
        tokio::fs::remove_dir(&fs)
            .await
            .map_err(ProviderError::IoError)?;
        Ok(())
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        let vpath = self.virtual_path(path)?;
        if vpath == "/" {
            return Err(ProviderError::NotSupported(
                "cannot delete the MTP mount root".to_string(),
            ));
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
            return Err(ProviderError::InvalidPath(
                "cannot rename the MTP mount root".to_string(),
            ));
        }
        let src = self.resolve_existing(&from_v)?;
        let dest = self.resolve_for_create(&to_v)?;
        tokio::fs::rename(&src, &dest)
            .await
            .map_err(ProviderError::IoError)?;
        Ok(())
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        let vpath = self.virtual_path(path)?;
        let fs = self.resolve_existing(&vpath)?;
        let meta = tokio::fs::symlink_metadata(&fs)
            .await
            .map_err(|e| ProviderError::NotFound(format!("{vpath}: {e}")))?;
        let name = if vpath == "/" {
            self.display_name.clone()
        } else {
            leaf_name(&vpath).unwrap_or_else(|_| vpath.clone())
        };
        Ok(self.entry_for(name, vpath, &meta))
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
        Ok(format!(
            "MTP portable device (gvfs/filesystem): {} ({}) at {}",
            self.display_name,
            self.device_id,
            self.mount_root.display()
        ))
    }

    fn transfer_capabilities(&self) -> TransferCapabilities {
        // Same honest surface as exclusive libmtp: whole-file, single slot.
        MtpProvider::honest_transfer_capabilities()
    }

    fn supports_delta_sync(&self) -> bool {
        false
    }

    fn supports_resume(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    async fn connected(root: &Path) -> MtpFsProvider {
        let mut p = MtpFsProvider::new(
            root.to_path_buf(),
            "dev-test".to_string(),
            "Test Phone".to_string(),
        );
        p.connect().await.expect("connect");
        p
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
            let entries = p.list("/").await.unwrap();
            let leak = entries.iter().find(|e| e.name == "leak.txt").unwrap();
            assert!(leak.is_symlink);
        }
    }

    #[tokio::test]
    async fn connect_requires_existing_directory() {
        let mut p = MtpFsProvider::new(
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
}
