//! AeroShare peer drive provider (Phase 1: read-only).
//!
//! "P2P is just a channel": a friend's shared drive is ANOTHER endpoint in the
//! dual-panel transfer fabric, not a separate UI. The iroh engine replicates
//! the drive into a LOCAL replica folder (kept converging by the
//! `crate::peer::runtime::PeerRuntime` sync task); this provider is the thin
//! `StorageProvider` adapter that lets the existing browse/transfer machinery
//! consume that replica like any server.
//!
//! Direction semantics (design doc §3): "their drive" is a folder THEY publish
//! and I replicate, so every read delegates to the replica root and every
//! mutation returns [`ProviderError::ReadOnly`] — Phase 1 ships browse/pull
//! only. The write direction ("my drive to them") is Phase 2 and will be a
//! drive I publish, not a mutation of their replica.
//!
//! Nice property the replica buys us for free: the last synced state stays
//! browsable even while the friend is OFFLINE, and live-converges when they
//! come back (engine stages S8/S9/S10).

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use super::types::{ProviderConfig, ProviderError, ProviderType, RemoteEntry};
use super::StorageProvider;
use async_trait::async_trait;
use std::path::{Path, PathBuf};

/// `ProviderConfig.extra` keys the `"peer"` protocol arm fills in
/// (`provider_commands::to_provider_config`) and the peer runtime reads back.
pub const PEER_EXTRA_NAMESPACE: &str = "peer_namespace";
pub const PEER_EXTRA_TICKET: &str = "peer_ticket";
pub const PEER_EXTRA_LOCAL_FOLDER: &str = "peer_local_folder";
pub const PEER_EXTRA_ROLE: &str = "peer_role";

/// Stable marker used inside [`ProviderError::ReadOnly`] messages so the GUI
/// can detect the condition and localize it.
const READ_ONLY_DETAIL: &str = "AeroShare Phase 1 shares are browse/pull only";

/// Parsed connection settings for one peer drive.
#[derive(Debug, Clone)]
pub struct PeerProviderConfig {
    /// The friend's AeroFTP-ID (already validated by the connect arm).
    pub friend_afid: String,
    /// Optional human alias for the friend (used in display names).
    pub friend_alias: String,
    /// The drive's iroh-docs namespace id (hex).
    pub namespace_id: String,
    /// The LOCAL folder the sync task replicates the drive into.
    pub replica_root: PathBuf,
    /// My relationship to the drive (`replicator` for "their drive" in P1).
    pub role: String,
}

impl PeerProviderConfig {
    pub fn from_provider_config(config: &ProviderConfig) -> Result<Self, ProviderError> {
        let namespace_id = config
            .extra
            .get(PEER_EXTRA_NAMESPACE)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                ProviderError::InvalidConfig(
                    "AeroShare connection requires the drive namespace".to_string(),
                )
            })?;
        let replica_root = config
            .extra
            .get(PEER_EXTRA_LOCAL_FOLDER)
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| {
                ProviderError::InvalidConfig(
                    "AeroShare connection requires the local replica folder".to_string(),
                )
            })?;
        if !replica_root.is_absolute() {
            return Err(ProviderError::InvalidConfig(
                "AeroShare replica folder must be an absolute path".to_string(),
            ));
        }
        Ok(Self {
            friend_afid: config.host.trim().to_string(),
            friend_alias: config.username.clone().unwrap_or_default(),
            namespace_id,
            replica_root,
            role: config
                .extra
                .get(PEER_EXTRA_ROLE)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "replicator".to_string()),
        })
    }
}

/// Read-only `StorageProvider` over a peer drive's local replica folder.
pub struct PeerProvider {
    config: PeerProviderConfig,
    /// Canonicalized replica root, fixed at `connect()`. All path resolution
    /// is anchored here so nothing the frontend sends can read outside it.
    root: Option<PathBuf>,
    /// Virtual cwd, always normalized and `/`-rooted (FTP-style).
    cwd: String,
}

impl PeerProvider {
    pub fn new(config: PeerProviderConfig) -> Self {
        Self {
            config,
            root: None,
            cwd: "/".to_string(),
        }
    }

    /// Uniform Phase-1 answer for every mutation verb.
    fn read_only<T>(op: &str) -> Result<T, ProviderError> {
        Err(ProviderError::ReadOnly(format!(
            "{READ_ONLY_DETAIL} ({op})"
        )))
    }

    fn root(&self) -> Result<&Path, ProviderError> {
        self.root.as_deref().ok_or(ProviderError::NotConnected)
    }

    /// Short, human-scannable form of an AeroFTP-ID for labels/logs.
    fn short_afid(&self) -> String {
        let id = self.config.friend_afid.as_str();
        if id.len() > 13 {
            format!("{}…{}", &id[..8], &id[id.len() - 4..])
        } else {
            id.to_string()
        }
    }

    /// Normalize `path` against the virtual cwd into a `/`-rooted virtual
    /// path. `..` clamps at the root (FTP semantics), so the produced
    /// component list can never point above the replica root by
    /// construction.
    fn virtual_path(&self, path: &str) -> Result<String, ProviderError> {
        if path.contains('\0') {
            return Err(ProviderError::InvalidPath(
                "path contains a NUL byte".to_string(),
            ));
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
                other => parts.push(other),
            }
        }
        Ok(format!("/{}", parts.join("/")))
    }

    /// Map a virtual path onto the replica filesystem.
    fn fs_path(&self, virtual_path: &str) -> Result<PathBuf, ProviderError> {
        let mut fs = self.root()?.to_path_buf();
        for comp in virtual_path.split('/').filter(|c| !c.is_empty()) {
            fs.push(comp);
        }
        Ok(fs)
    }

    /// Resolve + contain: canonicalize `fs` (follows symlinks) and require the
    /// result to stay inside the canonical replica root. The engine never
    /// writes symlinks, but the user may have pointed the replica at a folder
    /// that contains them; refusing the escape keeps "browse a friend's drive"
    /// from turning into "browse my own disk through their share".
    fn contained(&self, fs: &Path) -> Result<PathBuf, ProviderError> {
        let canon = fs
            .canonicalize()
            .map_err(|e| ProviderError::NotFound(format!("{}: {e}", fs.display())))?;
        if !canon.starts_with(self.root()?) {
            return Err(ProviderError::InvalidPath(
                "path resolves outside the AeroShare replica".to_string(),
            ));
        }
        Ok(canon)
    }

    /// Build a `RemoteEntry` for one direntry, without following symlinks.
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

    fn join_virtual(dir: &str, name: &str) -> String {
        if dir == "/" {
            format!("/{name}")
        } else {
            format!("{dir}/{name}")
        }
    }
}

#[async_trait]
impl StorageProvider for PeerProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn provider_type(&self) -> ProviderType {
        ProviderType::Peer
    }

    fn display_name(&self) -> String {
        let who = if self.config.friend_alias.trim().is_empty() {
            self.short_afid()
        } else {
            self.config.friend_alias.trim().to_string()
        };
        format!("AeroShare: {who}")
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        // The replica may not exist yet (first connect before the first sync
        // pass lands): create it so "browse while empty" works and the sync
        // task converges into it. Canonicalize AFTER creation so the root
        // anchor is symlink-free.
        tokio::fs::create_dir_all(&self.config.replica_root)
            .await
            .map_err(|e| {
                ProviderError::ConnectionFailed(format!(
                    "cannot create replica folder {}: {e}",
                    self.config.replica_root.display()
                ))
            })?;
        let canon = self.config.replica_root.canonicalize().map_err(|e| {
            ProviderError::ConnectionFailed(format!(
                "cannot resolve replica folder {}: {e}",
                self.config.replica_root.display()
            ))
        })?;
        self.root = Some(canon);
        self.cwd = "/".to_string();
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        // The sync task is owned by the PeerRuntime (it outlives the panel
        // connection by design: tray-keeps-serving); nothing to tear down.
        self.root = None;
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.root.is_some()
    }

    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let vpath = self.virtual_path(path)?;
        let dir = self.contained(&self.fs_path(&vpath)?)?;
        let mut rd = tokio::fs::read_dir(&dir)
            .await
            .map_err(ProviderError::IoError)?;
        let mut entries = Vec::new();
        while let Some(item) = rd.next_entry().await.map_err(ProviderError::IoError)? {
            let name = item.file_name().to_string_lossy().to_string();
            // symlink_metadata: report links instead of following them.
            let meta = match tokio::fs::symlink_metadata(item.path()).await {
                Ok(m) => m,
                Err(_) => continue, // raced away mid-listing
            };
            let ventry = Self::join_virtual(&vpath, &name);
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
        let fs = self.contained(&self.fs_path(&vpath)?)?;
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
        let src = self.contained(&self.fs_path(&vpath)?)?;
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
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(ProviderError::IoError)?;
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
        let vpath = self.virtual_path(remote_path)?;
        let src = self.contained(&self.fs_path(&vpath)?)?;
        tokio::fs::read(&src).await.map_err(ProviderError::IoError)
    }

    async fn upload(
        &mut self,
        _local_path: &str,
        _remote_path: &str,
        _on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        Self::read_only("upload")
    }

    async fn mkdir(&mut self, _path: &str) -> Result<(), ProviderError> {
        Self::read_only("mkdir")
    }

    async fn delete(&mut self, _path: &str) -> Result<(), ProviderError> {
        Self::read_only("delete")
    }

    async fn rmdir(&mut self, _path: &str) -> Result<(), ProviderError> {
        Self::read_only("rmdir")
    }

    async fn rmdir_recursive(&mut self, _path: &str) -> Result<(), ProviderError> {
        Self::read_only("rmdir")
    }

    async fn rename(&mut self, _from: &str, _to: &str) -> Result<(), ProviderError> {
        Self::read_only("rename")
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        let vpath = self.virtual_path(path)?;
        let fs = self.contained(&self.fs_path(&vpath)?)?;
        let meta = tokio::fs::symlink_metadata(&fs)
            .await
            .map_err(|e| ProviderError::NotFound(format!("{vpath}: {e}")))?;
        let name = vpath.rsplit('/').next().unwrap_or("").to_string();
        let name = if name.is_empty() {
            "/".to_string()
        } else {
            name
        };
        Ok(self.entry_for(name, vpath, &meta))
    }

    async fn size(&mut self, path: &str) -> Result<u64, ProviderError> {
        let vpath = self.virtual_path(path)?;
        let fs = self.contained(&self.fs_path(&vpath)?)?;
        let meta = tokio::fs::metadata(&fs)
            .await
            .map_err(|e| ProviderError::NotFound(format!("{vpath}: {e}")))?;
        Ok(meta.len())
    }

    async fn exists(&mut self, path: &str) -> Result<bool, ProviderError> {
        let vpath = self.virtual_path(path)?;
        match self.contained(&self.fs_path(&vpath)?) {
            Ok(_) => Ok(true),
            Err(ProviderError::NotFound(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    async fn keep_alive(&mut self) -> Result<(), ProviderError> {
        self.root()?;
        Ok(())
    }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        Ok(format!(
            "AeroShare peer drive (read-only replica) — friend {}, namespace {}, role {}",
            self.short_afid(),
            self.config.namespace_id,
            self.config.role
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config(root: &Path) -> PeerProviderConfig {
        PeerProviderConfig {
            friend_afid: "AFID1testtesttesttesttesttesttest".to_string(),
            friend_alias: "Carol".to_string(),
            namespace_id: "8ce68153cc3b80d778b594b7e3787e3511745ca28b384ebdb4fab5ec41be0832"
                .to_string(),
            replica_root: root.to_path_buf(),
            role: "replicator".to_string(),
        }
    }

    async fn connected_provider(root: &Path) -> PeerProvider {
        let mut p = PeerProvider::new(test_config(root));
        p.connect().await.expect("connect");
        p
    }

    #[tokio::test]
    async fn browses_the_replica_read_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join("photos")).unwrap();
        std::fs::write(dir.path().join("photos/a.txt"), b"hello").unwrap();
        std::fs::write(dir.path().join("readme.md"), b"hi").unwrap();

        let mut p = connected_provider(dir.path()).await;
        assert!(p.is_connected());
        assert_eq!(p.pwd().await.unwrap(), "/");

        let mut names: Vec<String> = p
            .list("/")
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        names.sort();
        assert_eq!(names, vec!["photos".to_string(), "readme.md".to_string()]);

        p.cd("photos").await.unwrap();
        assert_eq!(p.pwd().await.unwrap(), "/photos");
        let listed = p.list(".").await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].path, "/photos/a.txt");
        assert_eq!(listed[0].size, 5);

        assert_eq!(p.download_to_bytes("a.txt").await.unwrap(), b"hello");
        assert_eq!(p.size("/photos/a.txt").await.unwrap(), 5);
        assert!(p.exists("a.txt").await.unwrap());
        assert!(!p.exists("missing.txt").await.unwrap());

        p.cd_up().await.unwrap();
        assert_eq!(p.pwd().await.unwrap(), "/");
        // cd_up at root clamps (stays at root) instead of erroring.
        p.cd_up().await.unwrap();
        assert_eq!(p.pwd().await.unwrap(), "/");
    }

    #[tokio::test]
    async fn download_reports_progress_and_writes_the_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("data.bin"), vec![7u8; 4096]).unwrap();
        let out_dir = tempfile::tempdir().expect("outdir");
        let out = out_dir.path().join("nested/data.bin");

        let mut p = connected_provider(dir.path()).await;
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen_cb = seen.clone();
        p.download(
            "/data.bin",
            out.to_str().unwrap(),
            Some(Box::new(move |done, total| {
                seen_cb.lock().unwrap().push((done, total));
            })),
        )
        .await
        .unwrap();

        assert_eq!(std::fs::read(&out).unwrap(), vec![7u8; 4096]);
        let seen = seen.lock().unwrap();
        assert_eq!(seen.last().copied(), Some((4096, 4096)));
    }

    #[tokio::test]
    async fn every_mutation_is_read_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.txt"), b"x").unwrap();
        let mut p = connected_provider(dir.path()).await;

        let is_read_only =
            |r: Result<(), ProviderError>| matches!(r, Err(ProviderError::ReadOnly(_)));
        assert!(is_read_only(p.upload("/tmp/x", "/a.txt", None).await));
        assert!(is_read_only(p.mkdir("/new").await));
        assert!(is_read_only(p.delete("/a.txt").await));
        assert!(is_read_only(p.rmdir("/a").await));
        assert!(is_read_only(p.rmdir_recursive("/a").await));
        assert!(is_read_only(p.rename("/a.txt", "/b.txt").await));
        // And nothing was actually touched.
        assert!(dir.path().join("a.txt").exists());
        assert!(!dir.path().join("new").exists());
    }

    #[tokio::test]
    async fn traversal_and_symlink_escapes_are_contained() {
        let outside = tempfile::tempdir().expect("outside");
        std::fs::write(outside.path().join("secret.txt"), b"top-secret").unwrap();
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("ok.txt"), b"fine").unwrap();

        let mut p = connected_provider(dir.path()).await;

        // `..` clamps at the virtual root: this resolves to /ok.txt, not an
        // escape to the real parent directory.
        assert_eq!(
            p.download_to_bytes("../../../ok.txt").await.unwrap(),
            b"fine"
        );
        // A path that names something outside the root simply does not exist
        // in the virtual tree.
        assert!(!p.exists("../../../etc/hostname").await.unwrap());

        // A symlink INSIDE the replica pointing OUTSIDE it must not be served.
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
            // It still LISTS (as a symlink entry) without following.
            let entries = p.list("/").await.unwrap();
            let leak = entries.iter().find(|e| e.name == "leak.txt").unwrap();
            assert!(leak.is_symlink);
        }
    }

    #[tokio::test]
    async fn config_requires_namespace_and_absolute_folder() {
        let mut config = ProviderConfig {
            name: "test".to_string(),
            provider_type: ProviderType::Peer,
            host: "AFID1xyz".to_string(),
            port: None,
            username: Some("Carol".to_string()),
            password: None,
            initial_path: None,
            extra: Default::default(),
        };
        assert!(matches!(
            PeerProviderConfig::from_provider_config(&config),
            Err(ProviderError::InvalidConfig(_))
        ));

        config
            .extra
            .insert(PEER_EXTRA_NAMESPACE.to_string(), "abc123".to_string());
        config.extra.insert(
            PEER_EXTRA_LOCAL_FOLDER.to_string(),
            "relative/path".to_string(),
        );
        assert!(matches!(
            PeerProviderConfig::from_provider_config(&config),
            Err(ProviderError::InvalidConfig(_))
        ));

        config.extra.insert(
            PEER_EXTRA_LOCAL_FOLDER.to_string(),
            "/tmp/aeroshare-test".to_string(),
        );
        let parsed = PeerProviderConfig::from_provider_config(&config).expect("valid");
        assert_eq!(parsed.namespace_id, "abc123");
        assert_eq!(parsed.role, "replicator");
        assert_eq!(parsed.friend_alias, "Carol");
    }
}
