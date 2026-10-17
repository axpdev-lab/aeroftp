//! [`StorageProvider`] over the native Drive client.
//!
//! The root `/` is the account's My Files folder, as in rclone's
//! `protondrive` backend. Paths are resolved by listing each folder on the
//! way; the answers are kept for [`PATH_CACHE_TTL`] and dropped at every
//! write of this provider, so a listing seconds old answers the next lookup
//! instead of a new walk.
//!
//! Semantics, in the words of the trait:
//! - `delete` moves the item to the Proton trash (as the CLI wrapper does);
//!   `delete_permanent` purges it, live or already in the trash;
//! - `rmdir` refuses a folder that lists anything; `rmdir_recursive` trashes
//!   the folder with its content;
//! - `rename` in the same folder is Proton's rename, into another folder its
//!   move (one request, also when the name changes); a taken name is refused
//!   by Proton, never overwritten;
//! - `upload` onto an existing file writes a new revision of it, so its
//!   history and share links stay. A file written in Proton's newer (AEAD)
//!   format cannot get a revision from `proton-crypto` yet: the new content
//!   is then uploaded under a temporary name, the old file is set aside,
//!   the new one takes its name and the old one goes to the trash.
//!
//! [`DriveOps`] erases the OpenPGP provider type of [`DriveClient`], which
//! `load_account` returns as an opaque type, and is the seam the unit tests
//! replace with an in-memory drive.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use proton_crypto_account::proton_crypto::crypto::PGPProviderSync;
use tokio::io::{AsyncRead, AsyncWrite};

use super::api::ProtonApi;
use super::client::{DriveClient, Quota};
use super::download::DownloadOutcome;
use super::drive_crypto::Verified;
use super::error::ProtonError;
use super::nodes::Node;
use super::{account, app_version, vault};
use crate::providers::proton::ProtonConfig;
use crate::providers::{
    ProviderError, ProviderType, RemoteEntry, StorageInfo, StorageProvider, MAX_DOWNLOAD_TO_BYTES,
};

/// How long a resolved path is trusted without listing its folder again.
/// Writes of this provider drop the whole cache at once; changes made
/// elsewhere show within this window.
pub const PATH_CACHE_TTL: Duration = Duration::from_secs(30);

const NOT_SIGNED_IN: &str =
    "This profile is not signed in to Proton yet: sign in with Proton, then connect.";

/// Whether `expected`, a profile's account field, names one of `addresses`:
/// a full address compares whole, a bare name compares to each address's
/// local part, both ignoring case.
fn names_account(expected: &str, addresses: &[&str]) -> bool {
    let expected = expected.trim().to_lowercase();
    addresses.iter().any(|address| {
        let address = address.to_lowercase();
        if expected.contains('@') {
            address == expected
        } else {
            address.split('@').next() == Some(expected.as_str())
        }
    })
}

/// What the provider needs of a node, without the OpenPGP provider type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeInfo {
    pub link_id: String,
    pub parent_link_id: Option<String>,
    /// `None` when the name could not be decrypted.
    pub name: Option<String>,
    pub is_folder: bool,
    pub size: u64,
    pub modified: Option<DateTime<Utc>>,
    pub mime_type: Option<String>,
    /// Plaintext SHA-1 claimed by the uploader, lowercase hex.
    pub sha1: Option<String>,
    /// False when a signature over the node's key or name did not verify.
    pub verified: bool,
}

impl NodeInfo {
    fn of<P: PGPProviderSync>(node: &Node<P>) -> Self {
        let invalid = |v: &Verified| matches!(v, Verified::Invalid(_));
        Self {
            link_id: node.link_id.clone(),
            parent_link_id: node.parent_link_id.clone(),
            name: node.display_name().map(str::to_string),
            is_folder: node.is_folder(),
            size: if node.is_folder() { 0 } else { node.size() },
            modified: node.modification_time(),
            mime_type: node.mime_type.clone(),
            sha1: node.xattr.sha1.clone(),
            verified: !invalid(&node.name_verified) && !invalid(&node.key_verified),
        }
    }
}

/// The Drive operations the provider uses, keyed by link ID.
#[async_trait]
pub trait DriveOps: Send + Sync {
    fn account_email(&self) -> Option<String>;
    async fn root(&self) -> Result<NodeInfo, ProtonError>;
    /// A node, from the client's cache when it has it.
    async fn node(&self, link_id: &str) -> Result<NodeInfo, ProtonError>;
    async fn children(&self, folder_link_id: &str) -> Result<Vec<NodeInfo>, ProtonError>;
    async fn trashed_children(&self, folder_link_id: &str) -> Result<Vec<NodeInfo>, ProtonError>;
    async fn download(
        &self,
        link_id: &str,
        out: &mut (dyn AsyncWrite + Unpin + Send),
        progress: &mut (dyn FnMut(u64) + Send),
    ) -> Result<DownloadOutcome, ProtonError>;
    #[allow(clippy::too_many_arguments)]
    async fn upload_new(
        &self,
        parent_link_id: &str,
        name: &str,
        reader: &mut (dyn AsyncRead + Unpin + Send),
        size: u64,
        modified: Option<DateTime<Utc>>,
        progress: &mut (dyn FnMut(u64) + Send),
    ) -> Result<String, ProtonError>;
    async fn upload_revision(
        &self,
        link_id: &str,
        reader: &mut (dyn AsyncRead + Unpin + Send),
        size: u64,
        modified: Option<DateTime<Utc>>,
        progress: &mut (dyn FnMut(u64) + Send),
    ) -> Result<(), ProtonError>;
    async fn create_folder(&self, parent_link_id: &str, name: &str) -> Result<String, ProtonError>;
    async fn rename(&self, link_id: &str, new_name: &str) -> Result<(), ProtonError>;
    async fn move_node(
        &self,
        link_id: &str,
        new_parent_link_id: &str,
        new_name: Option<&str>,
    ) -> Result<(), ProtonError>;
    async fn trash(&self, link_ids: &[&str]) -> Result<(), ProtonError>;
    async fn delete_permanently(&self, link_ids: &[&str]) -> Result<(), ProtonError>;
    async fn quota(&self) -> Result<Quota, ProtonError>;
    /// Drops every cached node.
    async fn forget_all(&self);
}

impl<P> DriveClient<P>
where
    P: PGPProviderSync,
{
    /// The node read again from the server: a write needs the current name
    /// hash and a download the current revision, not a cached copy.
    async fn fresh(&self, link_id: &str) -> Result<Arc<Node<P>>, ProtonError> {
        self.forget(&[link_id]).await;
        self.node(link_id).await
    }
}

#[async_trait]
impl<P> DriveOps for DriveClient<P>
where
    P: PGPProviderSync + Send + Sync + 'static,
{
    fn account_email(&self) -> Option<String> {
        self.account
            .address(&self.my_files.address_id)
            .ok()
            .map(|address| address.email.clone())
    }

    async fn root(&self) -> Result<NodeInfo, ProtonError> {
        Ok(NodeInfo::of(&*DriveClient::root(self).await?))
    }

    async fn node(&self, link_id: &str) -> Result<NodeInfo, ProtonError> {
        Ok(NodeInfo::of(&*DriveClient::node(self, link_id).await?))
    }

    async fn children(&self, folder_link_id: &str) -> Result<Vec<NodeInfo>, ProtonError> {
        let folder = DriveClient::node(self, folder_link_id).await?;
        let children = DriveClient::children(self, &folder).await?;
        Ok(children.iter().map(|child| NodeInfo::of(child)).collect())
    }

    async fn trashed_children(&self, folder_link_id: &str) -> Result<Vec<NodeInfo>, ProtonError> {
        let folder = DriveClient::node(self, folder_link_id).await?;
        let trashed = DriveClient::trashed_children(self, &folder).await?;
        Ok(trashed.iter().map(|node| NodeInfo::of(node)).collect())
    }

    async fn download(
        &self,
        link_id: &str,
        out: &mut (dyn AsyncWrite + Unpin + Send),
        progress: &mut (dyn FnMut(u64) + Send),
    ) -> Result<DownloadOutcome, ProtonError> {
        let node = self.fresh(link_id).await?;
        let mut out = out;
        DriveClient::download(self, &node, &mut out, progress).await
    }

    async fn upload_new(
        &self,
        parent_link_id: &str,
        name: &str,
        reader: &mut (dyn AsyncRead + Unpin + Send),
        size: u64,
        modified: Option<DateTime<Utc>>,
        progress: &mut (dyn FnMut(u64) + Send),
    ) -> Result<String, ProtonError> {
        let parent = DriveClient::node(self, parent_link_id).await?;
        let mut reader = reader;
        self.upload_new_file(&parent, name, &mut reader, size, modified, |sent| {
            progress(sent)
        })
        .await
    }

    async fn upload_revision(
        &self,
        link_id: &str,
        reader: &mut (dyn AsyncRead + Unpin + Send),
        size: u64,
        modified: Option<DateTime<Utc>>,
        progress: &mut (dyn FnMut(u64) + Send),
    ) -> Result<(), ProtonError> {
        let node = self.fresh(link_id).await?;
        let mut reader = reader;
        DriveClient::upload_revision(self, &node, &mut reader, size, modified, |sent| {
            progress(sent)
        })
        .await?;
        self.forget(&[link_id]).await;
        Ok(())
    }

    async fn create_folder(&self, parent_link_id: &str, name: &str) -> Result<String, ProtonError> {
        let parent = DriveClient::node(self, parent_link_id).await?;
        DriveClient::create_folder(self, &parent, name).await
    }

    async fn rename(&self, link_id: &str, new_name: &str) -> Result<(), ProtonError> {
        let node = self.fresh(link_id).await?;
        DriveClient::rename(self, &node, new_name).await
    }

    async fn move_node(
        &self,
        link_id: &str,
        new_parent_link_id: &str,
        new_name: Option<&str>,
    ) -> Result<(), ProtonError> {
        let node = self.fresh(link_id).await?;
        let new_parent = DriveClient::node(self, new_parent_link_id).await?;
        DriveClient::move_node(self, &node, &new_parent, new_name).await
    }

    async fn trash(&self, link_ids: &[&str]) -> Result<(), ProtonError> {
        DriveClient::trash(self, link_ids).await
    }

    async fn delete_permanently(&self, link_ids: &[&str]) -> Result<(), ProtonError> {
        DriveClient::delete_permanently(self, link_ids).await
    }

    async fn quota(&self) -> Result<Quota, ProtonError> {
        DriveClient::quota(self).await
    }

    async fn forget_all(&self) {
        DriveClient::forget_all(self).await
    }
}

/// `ProtonError` as the trait's error, with the path the user asked about.
pub fn map_error(path: &str, error: ProtonError) -> ProviderError {
    match error {
        ProtonError::NotFound(_) => ProviderError::NotFound(path.to_string()),
        ProtonError::AlreadyExists { is_draft: true, .. } => ProviderError::AlreadyExists(format!(
            "{path}: another upload of this name is in progress"
        )),
        ProtonError::AlreadyExists { .. } => ProviderError::AlreadyExists(path.to_string()),
        ProtonError::InvalidName(reason) => ProviderError::InvalidPath(format!("{path}: {reason}")),
        ProtonError::SessionExpired | ProtonError::NotSignedIn => ProviderError::AuthenticationFailed(
            "The Proton session has ended: sign in with Proton again.".into(),
        ),
        ProtonError::HumanVerificationRequired { .. } => ProviderError::AuthenticationFailed(
            "Proton asks for a human verification: complete it in Proton Drive on the web, then connect again.".into(),
        ),
        ProtonError::RateLimited { retry_after_secs } => ProviderError::ServerError(format!(
            "Proton is limiting requests: try again in {retry_after_secs} s"
        )),
        ProtonError::Network(message) => ProviderError::NetworkError(message),
        ProtonError::Timeout => ProviderError::Timeout,
        ProtonError::LocalIo(error) => ProviderError::IoError(error),
        ProtonError::StorageTokenExpired => {
            ProviderError::TransferFailed("Proton storage token expired".into())
        }
        error @ (ProtonError::Integrity(_)
        | ProtonError::Crypto(_)
        | ProtonError::RevisionUnsupported) => ProviderError::TransferFailed(error.to_string()),
        error @ (ProtonError::Api { .. } | ProtonError::Decode(_)) => {
            ProviderError::ServerError(error.to_string())
        }
        ProtonError::SignInTimedOut | ProtonError::SignInCancelled => {
            ProviderError::AuthenticationFailed(error.to_string())
        }
    }
}

/// `path` as an absolute path without `.`, `..`, empty or trailing parts.
/// `..` above the root stays at the root.
pub fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            name => parts.push(name),
        }
    }
    format!("/{}", parts.join("/"))
}

/// The folder and the name of a normalized path; `None` for the root.
fn split(path: &str) -> Option<(String, String)> {
    let (parent, name) = path.rsplit_once('/')?;
    if name.is_empty() {
        return None;
    }
    let parent = if parent.is_empty() { "/" } else { parent };
    Some((parent.to_string(), name.to_string()))
}

fn join(folder: &str, name: &str) -> String {
    if folder == "/" {
        format!("/{name}")
    } else {
        format!("{folder}/{name}")
    }
}

fn to_entry(info: &NodeInfo, path: &str) -> RemoteEntry {
    let name = info.name.clone().unwrap_or_default();
    let mut entry = if info.is_folder {
        RemoteEntry::directory(name, path.to_string())
    } else {
        RemoteEntry::file(name, path.to_string(), info.size)
    };
    entry.modified = info
        .modified
        .map(|time| time.to_rfc3339_opts(SecondsFormat::Millis, true));
    entry.mime_type = info.mime_type.clone().filter(|_| !info.is_folder);
    entry
        .metadata
        .insert("link_id".to_string(), info.link_id.clone());
    if !info.verified {
        entry
            .metadata
            .insert("signature".to_string(), "unverified".to_string());
    }
    entry
}

/// A path resolved at `at`.
#[derive(Clone)]
struct Cached {
    info: NodeInfo,
    at: Instant,
}

/// Where a download is written before it replaces `destination`: a hidden
/// name of fixed length in the same folder, so it fits the file system's name
/// limit whatever the length of the file's own name (a 244-byte name plus a
/// suffix went past ext4's 255 bytes, found live by L2 Debby) and the final
/// rename stays on one file system.
fn partial_download_path(destination: &std::path::Path) -> std::path::PathBuf {
    let unique = uuid::Uuid::new_v4().simple().to_string();
    let name = format!(".aeroftp-proton-{}.part", &unique[..16]);
    match destination.parent().filter(|p| !p.as_os_str().is_empty()) {
        Some(folder) => folder.join(name),
        None => std::path::PathBuf::from(name),
    }
}

/// A local file's size and modification time.
fn local_file(path: &str) -> Result<(u64, Option<DateTime<Utc>>), ProviderError> {
    let meta = std::fs::metadata(path)?;
    if meta.is_dir() {
        return Err(ProviderError::InvalidPath(format!("{path} is a folder")));
    }
    let modified = meta.modified().ok().map(DateTime::<Utc>::from);
    Ok((meta.len(), modified))
}

/// An in-memory download that refuses to grow past `cap`.
struct CappedBuffer {
    data: Vec<u8>,
    cap: u64,
}

impl AsyncWrite for CappedBuffer {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        if self.data.len() as u64 + buf.len() as u64 > self.cap {
            return std::task::Poll::Ready(Err(std::io::Error::other(format!(
                "download exceeded the {:.0} MB cap",
                self.cap as f64 / 1_048_576.0
            ))));
        }
        self.data.extend_from_slice(buf);
        std::task::Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

/// Files transferred, and folders scanned, at once through clones of one
/// connection: below the SDK's 5 concurrent file uploads and downloads
/// (`upload/queue.ts`, `download/queue.ts`). Every clone shares the session,
/// its rate limiter and the block budget of the Drive client.
const MAX_SESSIONS: u16 = 4;

/// What a connection and its clones share: the resolved paths, the items
/// this session trashed and whether a listing left an unreadable name out.
/// A write through any clone drops the paths of all of them.
#[derive(Default)]
struct SharedState {
    paths: std::sync::Mutex<HashMap<String, Cached>>,
    /// Link IDs this session trashed, by path, so a purge right after a
    /// delete finds them without a trash scan.
    trashed: std::sync::Mutex<HashMap<String, String>>,
    /// A listing left out an entry whose name could not be decrypted, so
    /// listings no longer prove that a file is absent.
    skipped_unreadable: AtomicBool,
}

/// Proton Drive through the native client.
pub struct ProtonNativeProvider {
    config: ProtonConfig,
    drive: Option<Arc<dyn DriveOps>>,
    current_path: String,
    shared: Arc<SharedState>,
}

impl ProtonNativeProvider {
    pub fn new(config: ProtonConfig) -> Self {
        Self {
            config,
            drive: None,
            current_path: "/".to_string(),
            shared: Arc::default(),
        }
    }

    /// A clone on the same connection, for a parallel transfer or scan.
    fn clone_connected(&self) -> Result<Box<dyn StorageProvider>, ProviderError> {
        let drive = self.drive()?;
        Ok(Box::new(Self {
            config: self.config.clone(),
            drive: Some(drive),
            current_path: self.current_path.clone(),
            shared: Arc::clone(&self.shared),
        }))
    }

    fn trashed(&self) -> std::sync::MutexGuard<'_, HashMap<String, String>> {
        self.shared
            .trashed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn paths(&self) -> std::sync::MutexGuard<'_, HashMap<String, Cached>> {
        self.shared
            .paths
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// A provider over `drive`, already connected (tests).
    #[cfg(test)]
    fn with_drive(config: ProtonConfig, drive: Arc<dyn DriveOps>) -> Self {
        let mut provider = Self::new(config);
        provider.drive = Some(drive);
        provider
    }

    fn drive(&self) -> Result<Arc<dyn DriveOps>, ProviderError> {
        self.drive.clone().ok_or(ProviderError::NotConnected)
    }

    fn absolute(&self, path: &str) -> String {
        let path = path.trim();
        if path.starts_with('/') || path.starts_with('\\') {
            normalize(path)
        } else {
            normalize(&format!("{}/{path}", self.current_path))
        }
    }

    fn cached(&self, path: &str) -> Option<NodeInfo> {
        self.paths()
            .get(path)
            .filter(|cached| cached.at.elapsed() < PATH_CACHE_TTL)
            .map(|cached| cached.info.clone())
    }

    fn remember(&mut self, path: String, info: NodeInfo) {
        self.paths().insert(
            path,
            Cached {
                info,
                at: Instant::now(),
            },
        );
    }

    /// Drops every resolved path, for this connection and its clones:
    /// called after each write.
    fn invalidate(&mut self) {
        self.paths().clear();
    }

    /// The active children of `folder`, cached under `folder_path`. Entries
    /// whose name cannot be decrypted are left out (and remembered, see
    /// [`StorageProvider::listing_is_authoritative`]).
    async fn children_of(
        &mut self,
        folder_path: &str,
        folder: &NodeInfo,
    ) -> Result<Vec<(String, NodeInfo)>, ProviderError> {
        let children = self
            .drive()?
            .children(&folder.link_id)
            .await
            .map_err(|error| map_error(folder_path, error))?;
        let mut named = Vec::with_capacity(children.len());
        for child in children {
            let Some(name) = child.name.clone() else {
                self.shared
                    .skipped_unreadable
                    .store(true, Ordering::Relaxed);
                log::warn!("Proton: an item in {folder_path} has a name that cannot be decrypted");
                continue;
            };
            let path = join(folder_path, &name);
            self.remember(path.clone(), child.clone());
            named.push((path, child));
        }
        Ok(named)
    }

    /// Resolves a normalized absolute path, walking from the deepest folder
    /// on the way that is still cached.
    async fn resolve(&mut self, path: &str) -> Result<NodeInfo, ProviderError> {
        if let Some(info) = self.cached(path) {
            return Ok(info);
        }
        let drive = self.drive()?;
        let components: Vec<&str> = path.split('/').filter(|c| !c.is_empty()).collect();
        let mut start = 0;
        let mut current = None;
        for depth in (1..components.len()).rev() {
            let prefix = format!("/{}", components[..depth].join("/"));
            if let Some(info) = self.cached(&prefix) {
                current = Some((prefix, info));
                start = depth;
                break;
            }
        }
        let (mut current_path, mut current) = match current {
            Some(found) => found,
            None => {
                let root = drive.root().await.map_err(|error| map_error("/", error))?;
                self.remember("/".to_string(), root.clone());
                ("/".to_string(), root)
            }
        };
        for name in &components[start..] {
            if !current.is_folder {
                return Err(ProviderError::NotFound(path.to_string()));
            }
            let children = self.children_of(&current_path, &current).await?;
            let next = join(&current_path, name);
            current = children
                .into_iter()
                .find(|(child_path, _)| *child_path == next)
                .map(|(_, info)| info)
                .ok_or_else(|| ProviderError::NotFound(path.to_string()))?;
            current_path = next;
        }
        Ok(current)
    }

    async fn resolve_folder(&mut self, path: &str) -> Result<NodeInfo, ProviderError> {
        let info = self.resolve(path).await?;
        if !info.is_folder {
            return Err(ProviderError::InvalidPath(format!(
                "{path} is not a folder"
            )));
        }
        Ok(info)
    }

    /// The parent folder and the name of a path that is not the root.
    async fn parent_and_name(
        &mut self,
        path: &str,
    ) -> Result<(NodeInfo, String, String), ProviderError> {
        let (parent_path, name) = split(path).ok_or_else(|| {
            ProviderError::InvalidPath("the root of Proton Drive cannot be changed".into())
        })?;
        let parent = self.resolve_folder(&parent_path).await?;
        Ok((parent, parent_path, name))
    }

    /// A node that is not the root, for a write on it.
    async fn resolve_writable(&mut self, path: &str) -> Result<NodeInfo, ProviderError> {
        if path == "/" {
            return Err(ProviderError::InvalidPath(
                "the root of Proton Drive cannot be changed".into(),
            ));
        }
        self.resolve(path).await
    }

    /// Uploads `local` under `name` in `parent`: a new file, or a revision of
    /// the file already there.
    async fn upload_into(
        &mut self,
        local: &str,
        parent: &NodeInfo,
        remote: &str,
        name: &str,
        progress: &crate::providers::upload_progress::UploadProgress,
    ) -> Result<(), ProviderError> {
        let drive = self.drive()?;
        let (size, modified) = local_file(local)?;
        let mut report = |sent: u64| progress.report(sent);

        let mut file = tokio::fs::File::open(local).await?;
        let conflict = match drive
            .upload_new(
                &parent.link_id,
                name,
                &mut file,
                size,
                modified,
                &mut report,
            )
            .await
        {
            Ok(_) => return Ok(()),
            Err(ProtonError::AlreadyExists {
                link_id: Some(existing),
                is_draft: false,
                ..
            }) => existing,
            Err(error) => return Err(map_error(remote, error)),
        };

        let existing = drive
            .node(&conflict)
            .await
            .map_err(|error| map_error(remote, error))?;
        if existing.is_folder {
            return Err(ProviderError::AlreadyExists(format!(
                "{remote} is a folder"
            )));
        }
        let mut file = tokio::fs::File::open(local).await?;
        match drive
            .upload_revision(&conflict, &mut file, size, modified, &mut report)
            .await
        {
            Ok(()) => Ok(()),
            Err(ProtonError::RevisionUnsupported) => {
                self.replace_by_new_file(local, parent, &conflict, remote, name, progress)
                    .await
            }
            Err(error) => Err(map_error(remote, error)),
        }
    }

    /// The AEAD fallback of [`Self::upload_into`]: the new content goes up
    /// under a temporary name, the old file is renamed aside, the new one
    /// takes the name and only then is the old one trashed. Each failure
    /// undoes the steps before it, so the name keeps either file.
    async fn replace_by_new_file(
        &mut self,
        local: &str,
        parent: &NodeInfo,
        old_link_id: &str,
        remote: &str,
        name: &str,
        progress: &crate::providers::upload_progress::UploadProgress,
    ) -> Result<(), ProviderError> {
        log::info!(
            "Proton: {remote} is in Proton's newer format; replacing it with a new file, the old one goes to the trash"
        );
        let drive = self.drive()?;
        let (size, modified) = local_file(local)?;
        let mut report = |sent: u64| progress.report(sent);
        let unique = uuid::Uuid::new_v4().simple().to_string();
        let temporary = format!(".aeroftp-upload-{}", &unique[..8]);
        let mut file = tokio::fs::File::open(local).await?;
        let new_link_id = drive
            .upload_new(
                &parent.link_id,
                &temporary,
                &mut file,
                size,
                modified,
                &mut report,
            )
            .await
            .map_err(|error| map_error(remote, error))?;

        let aside = crate::providers::set_aside_name(name);
        if let Err(error) = drive.rename(old_link_id, &aside).await {
            let _ = drive.trash(&[&new_link_id]).await;
            return Err(map_error(remote, error));
        }
        if let Err(error) = drive.rename(&new_link_id, name).await {
            let undone = drive.rename(old_link_id, name).await;
            let _ = drive.trash(&[&new_link_id]).await;
            let error = map_error(remote, error);
            return Err(match undone {
                Ok(()) => error,
                Err(undo) => ProviderError::Other(format!(
                    "replacing {remote} stopped halfway: {error}; the old file is now {} ({})",
                    join(&split(remote).map(|(p, _)| p).unwrap_or_default(), &aside),
                    map_error(remote, undo)
                )),
            });
        }
        if let Err(error) = drive.trash(&[old_link_id]).await {
            log::warn!("Proton: the replaced copy of {remote} stays as {aside}: {error}");
        }
        Ok(())
    }

    /// The trashed item that was `path`: the one this session trashed, else
    /// the one trashed from its folder under its name.
    async fn find_trashed(&mut self, path: &str) -> Result<Option<String>, ProviderError> {
        if let Some(link_id) = self.trashed().get(path).cloned() {
            return Ok(Some(link_id));
        }
        let Some((parent_path, name)) = split(path) else {
            return Ok(None);
        };
        let parent = match self.resolve_folder(&parent_path).await {
            Ok(parent) => parent,
            Err(ProviderError::NotFound(_)) => return Ok(None),
            Err(error) => return Err(error),
        };
        let matches: Vec<String> = self
            .drive()?
            .trashed_children(&parent.link_id)
            .await
            .map_err(|error| map_error(path, error))?
            .into_iter()
            .filter(|node| node.name.as_deref() == Some(name.as_str()))
            .map(|node| node.link_id)
            .collect();
        crate::providers::the_one_trashed_item(path, matches)
    }
}

#[async_trait]
impl StorageProvider for ProtonNativeProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn provider_type(&self) -> ProviderType {
        ProviderType::Proton
    }

    fn display_name(&self) -> String {
        self.account_email()
            .unwrap_or_else(|| self.config.display_name.clone())
    }

    fn account_email(&self) -> Option<String> {
        self.drive.as_ref().and_then(|drive| drive.account_email())
    }

    /// Opens the profile's saved session. A saved profile that has none takes
    /// over the unbound one (a sign-in made before the profile was saved),
    /// once the account it unlocks is the one the profile names.
    async fn connect(&mut self) -> Result<(), ProviderError> {
        let profile_id = self.config.profile_id.clone();
        let (session, unbound) = match vault::load_session(profile_id.as_deref())? {
            Some(session) => (session, false),
            None if profile_id.is_some() => (
                vault::load_session(None)?
                    .ok_or_else(|| ProviderError::AuthenticationFailed(NOT_SIGNED_IN.into()))?,
                true,
            ),
            None => return Err(ProviderError::AuthenticationFailed(NOT_SIGNED_IN.into())),
        };
        let source = if unbound { None } else { profile_id.as_deref() };

        let api = Arc::new(ProtonApi::new(app_version::app_version()));
        api.set_tokens(Some(session.tokens.clone())).await;
        api.set_session_store(Arc::new(vault::VaultSession::new(source, &session)));
        let account = account::load_account(&api, &session.key_password)
            .await
            .map_err(|error| map_error("/", error))?;
        if let (true, Some(profile_id)) = (unbound, profile_id.as_deref()) {
            let addresses: Vec<&str> = account
                .addresses()
                .iter()
                .map(|address| address.email.as_str())
                .collect();
            if let Some(expected) = self.config.username.as_deref() {
                if !names_account(expected, &addresses) {
                    return Err(ProviderError::AuthenticationFailed(format!(
                        "The pending Proton sign-in belongs to another account than {expected}: \
                         sign in with Proton again for this profile."
                    )));
                }
            }
            let current = super::session::ProtonSession {
                tokens: api.tokens().await.unwrap_or(session.tokens.clone()),
                key_password: session.key_password.clone(),
            };
            vault::bind_unbound_session(profile_id, &current)?;
            api.set_session_store(Arc::new(vault::VaultSession::new(
                Some(profile_id),
                &current,
            )));
        }
        let drive = DriveClient::open(Arc::clone(&api), account)
            .await
            .map_err(|error| map_error("/", error))?;

        self.drive = Some(Arc::new(drive));
        self.current_path = "/".to_string();
        // A new connection starts clean; clones of the old one keep theirs.
        self.shared = Arc::default();
        tracing::info!("[Proton native] connected");
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        // The session stays saved and valid: disconnecting is not signing
        // out, which only the explicit sign-out and the profile delete do.
        self.drive = None;
        self.current_path = "/".to_string();
        self.invalidate();
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.drive.is_some()
    }

    fn listing_is_authoritative(&self) -> bool {
        !self.shared.skipped_unreadable.load(Ordering::Relaxed)
    }

    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let target = self.absolute(path);
        let folder = self.resolve_folder(&target).await?;
        let children = self.children_of(&target, &folder).await?;
        Ok(children
            .iter()
            .map(|(child_path, info)| to_entry(info, child_path))
            .collect())
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> {
        Ok(self.current_path.clone())
    }

    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        let target = self.absolute(path);
        self.resolve_folder(&target).await?;
        self.current_path = target;
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
        let remote = self.absolute(remote_path);
        let info = self.resolve(&remote).await?;
        if info.is_folder {
            return Err(ProviderError::InvalidPath(format!("{remote} is a folder")));
        }
        let drive = self.drive()?;
        let destination = std::path::Path::new(local_path);
        if let Some(parent) = destination.parent().filter(|p| !p.as_os_str().is_empty()) {
            tokio::fs::create_dir_all(parent).await?;
        }
        // The content lands beside the destination and takes its place only
        // when every block and the manifest signature verified, so a failed
        // download neither leaves a partial file nor spoils one already there.
        let partial = partial_download_path(destination);
        let total = info.size;
        let mut report = move |written: u64| {
            if let Some(callback) = on_progress.as_ref() {
                callback(written, total.max(written));
            }
        };
        let result = async {
            let mut file = tokio::fs::File::create(&partial).await?;
            drive
                .download(&info.link_id, &mut file, &mut report)
                .await
                .map_err(|error| map_error(&remote, error))?;
            file.sync_all().await?;
            drop(file);
            tokio::fs::rename(&partial, destination).await?;
            Ok::<(), ProviderError>(())
        }
        .await;
        if result.is_err() {
            let _ = tokio::fs::remove_file(&partial).await;
        }
        result
    }

    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        self.download_to_bytes_capped(remote_path, MAX_DOWNLOAD_TO_BYTES)
            .await
    }

    async fn download_to_bytes_capped(
        &mut self,
        remote_path: &str,
        max_bytes: u64,
    ) -> Result<Vec<u8>, ProviderError> {
        let remote = self.absolute(remote_path);
        let info = self.resolve(&remote).await?;
        if info.is_folder {
            return Err(ProviderError::InvalidPath(format!("{remote} is a folder")));
        }
        if info.size > max_bytes {
            return Err(ProviderError::TransferFailed(format!(
                "Download exceeded the {:.0} MB cap.",
                max_bytes as f64 / 1_048_576.0,
            )));
        }
        let mut buffer = CappedBuffer {
            data: Vec::with_capacity(info.size as usize),
            cap: max_bytes,
        };
        self.drive()?
            .download(&info.link_id, &mut buffer, &mut |_| {})
            .await
            .map_err(|error| match error {
                ProtonError::LocalIo(error) => ProviderError::TransferFailed(error.to_string()),
                other => map_error(&remote, other),
            })?;
        Ok(buffer.data)
    }

    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let remote = self.absolute(remote_path);
        let (size, _) = local_file(local_path)?;
        let (parent, _, name) = self.parent_and_name(&remote).await?;
        let progress = crate::providers::upload_progress::UploadProgress::new(on_progress, size);
        progress.start();
        let result = self
            .upload_into(local_path, &parent, &remote, &name, &progress)
            .await;
        self.invalidate();
        result?;
        progress.complete();
        Ok(())
    }

    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let target = self.absolute(path);
        let (parent, _, name) = self.parent_and_name(&target).await?;
        let result = self
            .drive()?
            .create_folder(&parent.link_id, &name)
            .await
            .map_err(|error| map_error(&target, error));
        self.invalidate();
        result.map(|_| ())
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        let target = self.absolute(path);
        let info = self.resolve_writable(&target).await?;
        let result = self
            .drive()?
            .trash(&[&info.link_id])
            .await
            .map_err(|error| map_error(&target, error));
        self.invalidate();
        result?;
        self.trashed().insert(target, info.link_id);
        Ok(())
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let target = self.absolute(path);
        let info = self.resolve_writable(&target).await?;
        if !info.is_folder {
            return Err(ProviderError::InvalidPath(format!(
                "{target} is not a folder"
            )));
        }
        // Proton trashes a folder with its content, so only one that lists
        // empty is sent (the trait's promise, round 2 of the 4.2.1 review).
        self.refuse_non_empty_dir(&target).await?;
        self.delete(&target).await
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        let target = self.absolute(path);
        let info = self.resolve_writable(&target).await?;
        if !info.is_folder {
            return Err(ProviderError::InvalidPath(format!(
                "{target} is not a folder"
            )));
        }
        self.delete(&target).await
    }

    /// Purges `path` for good: the live item (trashed first, as Proton
    /// requires), or the item a `delete` of that path put in the trash.
    /// `Ok(false)` when neither exists.
    async fn delete_permanent(&mut self, path: &str) -> Result<bool, ProviderError> {
        let target = self.absolute(path);
        let link_id = match self.resolve_writable(&target).await {
            Ok(info) => info.link_id,
            Err(ProviderError::NotFound(_)) => match self.find_trashed(&target).await? {
                Some(link_id) => link_id,
                None => return Ok(false),
            },
            Err(error) => return Err(error),
        };
        let result = self
            .drive()?
            .delete_permanently(&[&link_id])
            .await
            .map_err(|error| map_error(&target, error));
        self.invalidate();
        self.trashed().remove(&target);
        result.map(|()| true)
    }

    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let from = self.absolute(from);
        let to = self.absolute(to);
        if from == to {
            return Ok(());
        }
        if to.starts_with(&format!("{from}/")) {
            return Err(ProviderError::InvalidPath(format!(
                "{from} cannot be moved inside itself"
            )));
        }
        let source = self.resolve_writable(&from).await?;
        let (from_parent, from_name) = split(&from).unwrap_or_default();
        let (new_parent, to_parent, to_name) = self.parent_and_name(&to).await?;
        let drive = self.drive()?;
        let result = if from_parent == to_parent {
            drive.rename(&source.link_id, &to_name).await
        } else {
            let new_name = (to_name != from_name).then_some(to_name.as_str());
            drive
                .move_node(&source.link_id, &new_parent.link_id, new_name)
                .await
        };
        self.invalidate();
        result.map_err(|error| match error {
            ProtonError::AlreadyExists { .. } => ProviderError::AlreadyExists(to.clone()),
            other => map_error(&from, other),
        })
    }

    /// No: Proton renames and moves without an overwrite, so there is no
    /// one-step replace.
    async fn supports_atomic_replace(&mut self) -> Result<bool, ProviderError> {
        Ok(false)
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        let target = self.absolute(path);
        let info = self.resolve(&target).await?;
        if target == "/" {
            return Ok(RemoteEntry::directory("/".to_string(), "/".to_string()));
        }
        Ok(to_entry(&info, &target))
    }

    async fn size(&mut self, path: &str) -> Result<u64, ProviderError> {
        Ok(self.stat(path).await?.size)
    }

    async fn exists(&mut self, path: &str) -> Result<bool, ProviderError> {
        match self.stat(path).await {
            Ok(_) => Ok(true),
            Err(ProviderError::NotFound(_)) => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Nothing to send: the SDK rules ask for no polling, and a session left
    /// idle is refreshed by the next request.
    async fn keep_alive(&mut self) -> Result<(), ProviderError> {
        Ok(())
    }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        Ok(format!(
            "Proton Drive, AeroFTP native client ({})",
            app_version::app_version()
        ))
    }

    async fn storage_info(&mut self) -> Result<StorageInfo, ProviderError> {
        let quota = self
            .drive()?
            .quota()
            .await
            .map_err(|error| map_error("/", error))?;
        Ok(StorageInfo {
            used: quota.used,
            total: quota.total,
            free: quota.total.saturating_sub(quota.used),
            versioning_bytes: None,
        })
    }

    fn supports_checksum(&self) -> bool {
        true
    }

    /// The SHA-1 the uploader recorded in the file's signed attributes, when
    /// it recorded one (AeroFTP, the official clients and rclone do).
    async fn checksum(&mut self, path: &str) -> Result<HashMap<String, String>, ProviderError> {
        let target = self.absolute(path);
        let info = self.resolve(&target).await?;
        let mut digests = HashMap::new();
        if let Some(sha1) = info.sha1 {
            digests.insert("sha1".to_string(), sha1);
        }
        Ok(digests)
    }

    fn transfer_executor_kind(&self) -> crate::providers::ProviderTransferExecutorKind {
        crate::providers::ProviderTransferExecutorKind::HttpClonePool
    }

    fn transfer_executor_max_sessions(&self) -> u16 {
        MAX_SESSIONS
    }

    fn clone_for_transfer(&self) -> Result<Box<dyn StorageProvider>, ProviderError> {
        self.clone_connected()
    }

    /// A worker that finished a file is still on the shared connection, so
    /// the executor may hand it the next file.
    fn supports_transfer_worker_reuse(&self) -> bool {
        true
    }

    fn list_executor_kind(&self) -> crate::providers::ProviderListExecutorKind {
        crate::providers::ProviderListExecutorKind::HttpClonePool
    }

    fn list_executor_max_sessions(&self) -> u16 {
        MAX_SESSIONS
    }

    fn clone_for_list(&self) -> Result<Box<dyn StorageProvider>, ProviderError> {
        self.clone_connected()
    }

    fn checksum_capability(&self, _path: &str) -> crate::providers::ChecksumCapability {
        crate::providers::ChecksumCapability {
            algorithms: vec!["sha1".to_string()],
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[derive(Clone)]
    struct FakeNode {
        parent: Option<String>,
        name: Option<String>,
        folder: bool,
        content: Vec<u8>,
        trashed: bool,
        aead: bool,
        modified: Option<DateTime<Utc>>,
    }

    /// An in-memory Drive with Proton's rules: names are unique among the
    /// active children of a folder, a trashed item is purged only from the
    /// trash, an AEAD file takes no revision.
    #[derive(Default)]
    struct FakeDrive {
        nodes: Mutex<HashMap<String, FakeNode>>,
        next: AtomicUsize,
        calls: Mutex<Vec<String>>,
        /// `download` writes this many bytes, then fails its manifest check.
        corrupt_download: Mutex<Option<usize>>,
    }

    impl FakeDrive {
        fn new() -> Arc<Self> {
            let drive = Arc::new(Self::default());
            drive.insert("root", None, Some("root"), true, b"");
            drive
        }

        fn insert(
            &self,
            id: &str,
            parent: Option<&str>,
            name: Option<&str>,
            folder: bool,
            content: &[u8],
        ) {
            self.nodes.lock().unwrap().insert(
                id.to_string(),
                FakeNode {
                    parent: parent.map(str::to_string),
                    name: name.map(str::to_string),
                    folder,
                    content: content.to_vec(),
                    trashed: false,
                    aead: false,
                    modified: None,
                },
            );
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }

        fn count(&self, prefix: &str) -> usize {
            self.calls()
                .iter()
                .filter(|call| call.starts_with(prefix))
                .count()
        }

        fn log(&self, call: String) {
            self.calls.lock().unwrap().push(call);
        }

        fn get(&self, id: &str) -> FakeNode {
            self.nodes.lock().unwrap()[id].clone()
        }

        fn active_named(&self, parent: &str, name: &str) -> Option<String> {
            self.nodes
                .lock()
                .unwrap()
                .iter()
                .find(|(_, n)| {
                    !n.trashed
                        && n.parent.as_deref() == Some(parent)
                        && n.name.as_deref() == Some(name)
                })
                .map(|(id, _)| id.clone())
        }

        fn taken(&self, parent: &str, name: &str) -> Result<(), ProtonError> {
            match self.active_named(parent, name) {
                Some(existing) => Err(ProtonError::AlreadyExists {
                    link_id: Some(existing),
                    draft_client_uid: None,
                    is_draft: false,
                }),
                None => Ok(()),
            }
        }

        fn info(&self, id: &str, node: &FakeNode) -> NodeInfo {
            NodeInfo {
                link_id: id.to_string(),
                parent_link_id: node.parent.clone(),
                name: node.name.clone(),
                is_folder: node.folder,
                size: node.content.len() as u64,
                modified: node.modified,
                mime_type: None,
                sha1: (!node.folder).then(|| format!("sha1-of-{}", node.content.len())),
                verified: true,
            }
        }

        fn new_id(&self) -> String {
            format!("n{}", self.next.fetch_add(1, Ordering::SeqCst))
        }
    }

    #[async_trait]
    impl DriveOps for FakeDrive {
        fn account_email(&self) -> Option<String> {
            Some("someone@example.com".into())
        }

        async fn root(&self) -> Result<NodeInfo, ProtonError> {
            DriveOps::node(self, "root").await
        }

        async fn node(&self, link_id: &str) -> Result<NodeInfo, ProtonError> {
            let nodes = self.nodes.lock().unwrap();
            let node = nodes
                .get(link_id)
                .ok_or_else(|| ProtonError::NotFound(link_id.into()))?;
            Ok(self.info(link_id, node))
        }

        async fn children(&self, folder_link_id: &str) -> Result<Vec<NodeInfo>, ProtonError> {
            self.log(format!("children {folder_link_id}"));
            let nodes = self.nodes.lock().unwrap();
            let mut children: Vec<NodeInfo> = nodes
                .iter()
                .filter(|(_, n)| !n.trashed && n.parent.as_deref() == Some(folder_link_id))
                .map(|(id, n)| self.info(id, n))
                .collect();
            children.sort_by(|a, b| a.link_id.cmp(&b.link_id));
            Ok(children)
        }

        async fn trashed_children(
            &self,
            folder_link_id: &str,
        ) -> Result<Vec<NodeInfo>, ProtonError> {
            self.log(format!("trash-scan {folder_link_id}"));
            let nodes = self.nodes.lock().unwrap();
            Ok(nodes
                .iter()
                .filter(|(_, n)| n.trashed && n.parent.as_deref() == Some(folder_link_id))
                .map(|(id, n)| self.info(id, n))
                .collect())
        }

        async fn download(
            &self,
            link_id: &str,
            out: &mut (dyn AsyncWrite + Unpin + Send),
            progress: &mut (dyn FnMut(u64) + Send),
        ) -> Result<DownloadOutcome, ProtonError> {
            let content = self.get(link_id).content;
            let corrupt = *self.corrupt_download.lock().unwrap();
            if let Some(bytes) = corrupt {
                out.write_all(&content[..bytes]).await.unwrap();
                return Err(ProtonError::Integrity("manifest".into()));
            }
            out.write_all(&content)
                .await
                .map_err(ProtonError::LocalIo)?;
            progress(content.len() as u64);
            Ok(DownloadOutcome {
                bytes: content.len() as u64,
                blocks: 1,
                sha1: String::new(),
            })
        }

        async fn upload_new(
            &self,
            parent_link_id: &str,
            name: &str,
            reader: &mut (dyn AsyncRead + Unpin + Send),
            size: u64,
            modified: Option<DateTime<Utc>>,
            progress: &mut (dyn FnMut(u64) + Send),
        ) -> Result<String, ProtonError> {
            self.log(format!("upload-new {name}"));
            self.taken(parent_link_id, name)?;
            let mut content = Vec::new();
            reader.read_to_end(&mut content).await.unwrap();
            assert_eq!(content.len() as u64, size);
            progress(size);
            let id = self.new_id();
            self.insert(&id, Some(parent_link_id), Some(name), false, &content);
            self.nodes.lock().unwrap().get_mut(&id).unwrap().modified = modified;
            Ok(id)
        }

        async fn upload_revision(
            &self,
            link_id: &str,
            reader: &mut (dyn AsyncRead + Unpin + Send),
            _size: u64,
            modified: Option<DateTime<Utc>>,
            _progress: &mut (dyn FnMut(u64) + Send),
        ) -> Result<(), ProtonError> {
            self.log(format!("revision {link_id}"));
            if self.get(link_id).aead {
                return Err(ProtonError::RevisionUnsupported);
            }
            let mut content = Vec::new();
            reader.read_to_end(&mut content).await.unwrap();
            let mut nodes = self.nodes.lock().unwrap();
            let node = nodes.get_mut(link_id).unwrap();
            node.content = content;
            node.modified = modified;
            Ok(())
        }

        async fn create_folder(
            &self,
            parent_link_id: &str,
            name: &str,
        ) -> Result<String, ProtonError> {
            self.log(format!("mkdir {name}"));
            self.taken(parent_link_id, name)?;
            let id = self.new_id();
            self.insert(&id, Some(parent_link_id), Some(name), true, b"");
            Ok(id)
        }

        async fn rename(&self, link_id: &str, new_name: &str) -> Result<(), ProtonError> {
            self.log(format!("rename {link_id} {new_name}"));
            let parent = self.get(link_id).parent.unwrap();
            self.taken(&parent, new_name)?;
            self.nodes.lock().unwrap().get_mut(link_id).unwrap().name = Some(new_name.into());
            Ok(())
        }

        async fn move_node(
            &self,
            link_id: &str,
            new_parent_link_id: &str,
            new_name: Option<&str>,
        ) -> Result<(), ProtonError> {
            self.log(format!("move {link_id} {new_parent_link_id} {new_name:?}"));
            let name = new_name
                .map(str::to_string)
                .unwrap_or_else(|| self.get(link_id).name.unwrap());
            self.taken(new_parent_link_id, &name)?;
            let mut nodes = self.nodes.lock().unwrap();
            let node = nodes.get_mut(link_id).unwrap();
            node.parent = Some(new_parent_link_id.into());
            node.name = Some(name);
            Ok(())
        }

        async fn trash(&self, link_ids: &[&str]) -> Result<(), ProtonError> {
            self.log(format!("trash {}", link_ids.join(",")));
            let mut nodes = self.nodes.lock().unwrap();
            for id in link_ids {
                nodes.get_mut(*id).unwrap().trashed = true;
            }
            Ok(())
        }

        async fn delete_permanently(&self, link_ids: &[&str]) -> Result<(), ProtonError> {
            self.log(format!("purge {}", link_ids.join(",")));
            let mut nodes = self.nodes.lock().unwrap();
            for id in link_ids {
                nodes
                    .remove(*id)
                    .ok_or_else(|| ProtonError::NotFound((*id).into()))?;
            }
            Ok(())
        }

        async fn quota(&self) -> Result<Quota, ProtonError> {
            Ok(Quota {
                used: 300,
                total: 1000,
            })
        }

        async fn forget_all(&self) {}
    }

    fn config() -> ProtonConfig {
        ProtonConfig {
            display_name: "Proton".into(),
            binary_path: None,
            connection_mode: crate::providers::ProtonConnectionMode::Native,
            profile_id: Some("srv_test".into()),
            username: None,
        }
    }

    /// A provider over a drive holding `/docs/a.txt` ("alpha"), `/docs/sub/`
    /// and `/empty/`.
    fn provider() -> (ProtonNativeProvider, Arc<FakeDrive>) {
        let drive = FakeDrive::new();
        drive.insert("docs", Some("root"), Some("docs"), true, b"");
        drive.insert("a", Some("docs"), Some("a.txt"), false, b"alpha");
        drive.insert("sub", Some("docs"), Some("sub"), true, b"");
        drive.insert("empty", Some("root"), Some("empty"), true, b"");
        let provider =
            ProtonNativeProvider::with_drive(config(), Arc::clone(&drive) as Arc<dyn DriveOps>);
        (provider, drive)
    }

    fn local_file_with(dir: &tempfile::TempDir, name: &str, content: &[u8]) -> String {
        let path = dir.path().join(name);
        std::fs::write(&path, content).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn paths_are_normalized_without_escaping_the_root() {
        assert_eq!(normalize(""), "/");
        assert_eq!(normalize("/"), "/");
        assert_eq!(normalize("/a/./b/../c/"), "/a/c");
        assert_eq!(normalize("../../x"), "/x");
        assert_eq!(normalize("a\\b"), "/a/b");
        assert_eq!(split("/a/b.txt"), Some(("/a".into(), "b.txt".into())));
        assert_eq!(split("/b.txt"), Some(("/".into(), "b.txt".into())));
        assert_eq!(split("/"), None);
    }

    #[test]
    fn proton_errors_map_to_what_the_user_can_act_on() {
        assert!(matches!(
            map_error("/x", ProtonError::NotFound("node 1".into())),
            ProviderError::NotFound(ref p) if p == "/x"
        ));
        assert!(matches!(
            map_error(
                "/x",
                ProtonError::AlreadyExists {
                    link_id: None,
                    draft_client_uid: Some("other".into()),
                    is_draft: true
                }
            ),
            ProviderError::AlreadyExists(ref t) if t.contains("in progress")
        ));
        for ended in [ProtonError::SessionExpired, ProtonError::NotSignedIn] {
            assert!(matches!(
                map_error("/", ended),
                ProviderError::AuthenticationFailed(ref t) if t.contains("sign in")
            ));
        }
        assert!(matches!(
            map_error("/", ProtonError::HumanVerificationRequired { methods: vec![] }),
            ProviderError::AuthenticationFailed(ref t) if t.contains("human verification")
        ));
        assert!(matches!(
            map_error("/x", ProtonError::InvalidName("too long".into())),
            ProviderError::InvalidPath(_)
        ));
        assert!(matches!(
            map_error("/x", ProtonError::Integrity("block 1".into())),
            ProviderError::TransferFailed(_)
        ));
        assert!(matches!(
            map_error("/x", ProtonError::Network("reset".into())),
            ProviderError::NetworkError(_)
        ));
        assert!(matches!(
            map_error("/x", ProtonError::Timeout),
            ProviderError::Timeout
        ));
        assert!(matches!(
            map_error(
                "/x",
                ProtonError::LocalIo(std::io::Error::other("disk full"))
            ),
            ProviderError::IoError(_)
        ));
    }

    #[tokio::test]
    async fn lists_resolve_through_the_cache_until_a_write() {
        let (mut p, drive) = provider();
        let root = p.list("/").await.unwrap();
        let names: Vec<&str> = root.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["docs", "empty"]);
        let docs = p.list("/docs").await.unwrap();
        let a = docs.iter().find(|e| e.name == "a.txt").unwrap();
        assert_eq!(
            (a.path.as_str(), a.size, a.is_dir),
            ("/docs/a.txt", 5, false)
        );
        assert_eq!(drive.count("children"), 2);

        // Resolved from the listings above, no new walk.
        assert_eq!(p.stat("/docs/a.txt").await.unwrap().size, 5);
        assert!(p.exists("/docs/sub").await.unwrap());
        assert_eq!(drive.count("children"), 2);

        p.mkdir("/docs/new").await.unwrap();
        assert!(p.exists("/docs/new").await.unwrap());
        assert!(
            drive.count("children") > 2,
            "a write must drop the cached listings"
        );
    }

    #[tokio::test]
    async fn relative_paths_follow_the_current_folder() {
        let (mut p, _) = provider();
        p.cd("docs").await.unwrap();
        assert_eq!(p.pwd().await.unwrap(), "/docs");
        assert_eq!(p.stat("a.txt").await.unwrap().path, "/docs/a.txt");
        p.cd_up().await.unwrap();
        assert_eq!(p.pwd().await.unwrap(), "/");
        assert!(matches!(
            p.cd("/docs/a.txt").await,
            Err(ProviderError::InvalidPath(_))
        ));
        assert!(matches!(
            p.cd("/nope").await,
            Err(ProviderError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn an_upload_onto_an_existing_file_writes_a_revision() {
        let (mut p, drive) = provider();
        let dir = tempfile::tempdir().unwrap();
        let fresh = local_file_with(&dir, "b.txt", b"bravo");
        p.upload(&fresh, "/docs/b.txt", None).await.unwrap();
        let b = drive.active_named("docs", "b.txt").expect("uploaded");
        assert!(drive.get(&b).modified.is_some(), "the local mtime goes up");

        let newer = local_file_with(&dir, "a.txt", b"alpha two");
        p.upload(&newer, "/docs/a.txt", None).await.unwrap();
        assert_eq!(drive.get("a").content, b"alpha two");
        assert_eq!(drive.count("revision a"), 1);
        assert_eq!(
            drive.active_named("docs", "a.txt").as_deref(),
            Some("a"),
            "a revision keeps the file and its link"
        );
    }

    #[tokio::test]
    async fn an_aead_file_is_replaced_by_a_new_file_and_the_old_one_trashed() {
        let (mut p, drive) = provider();
        drive.nodes.lock().unwrap().get_mut("a").unwrap().aead = true;
        let dir = tempfile::tempdir().unwrap();
        let newer = local_file_with(&dir, "a.txt", b"alpha two");
        p.upload(&newer, "/docs/a.txt", None).await.unwrap();

        let now = drive.active_named("docs", "a.txt").expect("name kept");
        assert_ne!(now, "a");
        assert_eq!(drive.get(&now).content, b"alpha two");
        assert!(drive.get("a").trashed, "the old file is in the trash");
        let names: Vec<String> = p
            .list("/docs")
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names.len(), 2, "no temporary or set-aside left: {names:?}");
    }

    #[tokio::test]
    async fn an_upload_onto_a_folder_name_is_refused() {
        let (mut p, _) = provider();
        let dir = tempfile::tempdir().unwrap();
        let local = local_file_with(&dir, "sub", b"x");
        assert!(matches!(
            p.upload(&local, "/docs/sub", None).await,
            Err(ProviderError::AlreadyExists(_))
        ));
    }

    #[tokio::test]
    async fn renames_stay_in_place_and_moves_are_one_request() {
        let (mut p, drive) = provider();
        p.rename("/docs/a.txt", "/docs/b.txt").await.unwrap();
        assert_eq!(drive.count("rename a b.txt"), 1);
        p.rename("/docs/b.txt", "/empty/b.txt").await.unwrap();
        assert_eq!(drive.count("move a empty None"), 1);
        p.rename("/empty/b.txt", "/docs/sub/c.txt").await.unwrap();
        assert_eq!(drive.count("move a sub Some(\"c.txt\")"), 1);
        assert_eq!(drive.get("a").name.as_deref(), Some("c.txt"));
    }

    #[tokio::test]
    async fn a_rename_onto_a_taken_name_is_refused_and_changes_nothing() {
        let (mut p, drive) = provider();
        drive.insert("b", Some("docs"), Some("b.txt"), false, b"bravo");
        let error = p.rename("/docs/a.txt", "/docs/b.txt").await.unwrap_err();
        assert!(
            matches!(error, ProviderError::AlreadyExists(ref t) if t == "/docs/b.txt"),
            "{error:?}"
        );
        assert_eq!(drive.get("a").name.as_deref(), Some("a.txt"));
        assert!(matches!(
            p.rename("/docs", "/docs/sub/docs").await,
            Err(ProviderError::InvalidPath(_))
        ));
    }

    #[tokio::test]
    async fn rmdir_refuses_a_folder_with_content() {
        let (mut p, drive) = provider();
        assert!(matches!(
            p.rmdir("/docs").await,
            Err(ProviderError::DirectoryNotEmpty(_))
        ));
        assert!(!drive.get("docs").trashed);
        p.rmdir("/empty").await.unwrap();
        assert!(drive.get("empty").trashed);
        p.rmdir_recursive("/docs").await.unwrap();
        assert!(drive.get("docs").trashed);
    }

    #[tokio::test]
    async fn the_root_takes_no_writes() {
        let (mut p, drive) = provider();
        for result in [
            p.delete("/").await,
            p.rmdir("/").await,
            p.rename("/", "/x").await,
            p.mkdir("/").await,
        ] {
            assert!(
                matches!(result, Err(ProviderError::InvalidPath(_))),
                "{result:?}"
            );
        }
        assert_eq!(drive.count("trash"), 0);
    }

    #[tokio::test]
    async fn a_purge_after_a_delete_finds_the_trashed_item() {
        let (mut p, drive) = provider();
        p.delete("/docs/a.txt").await.unwrap();
        assert!(drive.get("a").trashed);
        assert!(p.delete_permanent("/docs/a.txt").await.unwrap());
        assert_eq!(drive.count("trash-scan"), 0, "this session knew the id");
        assert!(!drive.nodes.lock().unwrap().contains_key("a"));
        // Gone for good: neither live nor in the trash.
        assert!(!p.delete_permanent("/docs/a.txt").await.unwrap());
        assert_eq!(drive.count("trash-scan docs"), 1);

        // Another session trashed it: found in the trash of its folder.
        drive.insert("b", Some("docs"), Some("b.txt"), false, b"bravo");
        drive.nodes.lock().unwrap().get_mut("b").unwrap().trashed = true;
        let mut other =
            ProtonNativeProvider::with_drive(config(), Arc::clone(&drive) as Arc<dyn DriveOps>);
        assert!(other.delete_permanent("/docs/b.txt").await.unwrap());
        assert_eq!(drive.count("trash-scan docs"), 2);
        assert_eq!(drive.count("purge b"), 1);
        assert!(!drive.nodes.lock().unwrap().contains_key("b"));
    }

    #[tokio::test]
    async fn two_trashed_items_of_one_name_are_not_purged() {
        let (mut p, drive) = provider();
        for id in ["t1", "t2"] {
            drive.insert(id, Some("docs"), Some("dup.txt"), false, b"x");
            drive.nodes.lock().unwrap().get_mut(id).unwrap().trashed = true;
        }
        assert!(p.delete_permanent("/docs/dup.txt").await.is_err());
        assert_eq!(drive.count("purge"), 0);
    }

    #[tokio::test]
    async fn a_failed_download_leaves_no_partial_and_keeps_the_old_copy() {
        let (mut p, drive) = provider();
        let dir = tempfile::tempdir().unwrap();
        let destination = local_file_with(&dir, "a.txt", b"previous");
        *drive.corrupt_download.lock().unwrap() = Some(3);
        assert!(matches!(
            p.download("/docs/a.txt", &destination, None).await,
            Err(ProviderError::TransferFailed(_))
        ));
        assert_eq!(std::fs::read(&destination).unwrap(), b"previous");
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            1,
            "no partial left"
        );

        *drive.corrupt_download.lock().unwrap() = None;
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        p.download(
            "/docs/a.txt",
            &destination,
            Some(Box::new(move |done, total| {
                record.lock().unwrap().push((done, total))
            })),
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), b"alpha");
        assert_eq!(seen.lock().unwrap().last(), Some(&(5, 5)));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    /// The partial file's name does not grow with the file's: a 244-byte
    /// name (120 x "ä" plus ".txt", valid on ext4) downloads.
    #[tokio::test]
    async fn a_long_file_name_downloads_through_a_short_partial_name() {
        let long = format!("{}.txt", "ä".repeat(120));
        assert_eq!(long.len(), 244);
        let (mut p, drive) = provider();
        drive.insert("long", Some("docs"), Some(&long), false, b"long name");
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join(&long);
        let partial = partial_download_path(&destination);
        assert_eq!(partial.parent(), Some(dir.path()));
        assert!(partial.file_name().unwrap().len() < 64);

        p.download(
            &format!("/docs/{long}"),
            destination.to_str().unwrap(),
            None,
        )
        .await
        .expect("a long name downloads");
        assert_eq!(std::fs::read(&destination).unwrap(), b"long name");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn an_in_memory_download_refuses_a_file_over_the_cap() {
        let (mut p, _) = provider();
        assert_eq!(p.download_to_bytes("/docs/a.txt").await.unwrap(), b"alpha");
        assert!(matches!(
            p.download_to_bytes_capped("/docs/a.txt", 4).await,
            Err(ProviderError::TransferFailed(_))
        ));
    }

    #[tokio::test]
    async fn an_unreadable_name_is_left_out_and_listings_stop_being_proof() {
        let (mut p, drive) = provider();
        assert!(p.listing_is_authoritative());
        drive.insert("x", Some("docs"), None, false, b"secret");
        let names: Vec<String> = p
            .list("/docs")
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, vec!["a.txt", "sub"]);
        assert!(!p.listing_is_authoritative());
    }

    #[tokio::test]
    async fn checksum_and_quota_come_from_the_drive() {
        let (mut p, _) = provider();
        let digests = p.checksum("/docs/a.txt").await.unwrap();
        assert_eq!(digests.get("sha1").map(String::as_str), Some("sha1-of-5"));
        let info = p.storage_info().await.unwrap();
        assert_eq!((info.used, info.total, info.free), (300, 1000, 700));
        assert_eq!(p.checksum_capability("/").algorithms, vec!["sha1"]);
    }

    /// Clones share the connection and its caches: a write through one drops
    /// what the others resolved, and a purge finds what another one trashed.
    #[tokio::test]
    async fn clones_share_the_connection_and_its_caches() {
        let (mut p, drive) = provider();
        assert!(p.exists("/docs/a.txt").await.unwrap());
        let walks = drive.count("children");

        let mut clone = p.clone_for_transfer().expect("connected");
        clone.delete("/docs/a.txt").await.unwrap();
        assert!(
            !p.exists("/docs/a.txt").await.unwrap(),
            "the primary must not answer from a path its clone just trashed"
        );
        assert!(drive.count("children") > walks);

        assert!(p.delete_permanent("/docs/a.txt").await.unwrap());
        assert_eq!(drive.count("trash-scan"), 0, "the clone's trash is known");

        let caps = p.transfer_capabilities();
        assert_eq!(caps.file_parallel, crate::transfer_dag::Capability::Supported);
        assert_eq!(caps.max_file_slots, Some(MAX_SESSIONS));
        assert_eq!(caps.list_parallel, crate::transfer_dag::Capability::Supported);

        assert!(matches!(
            ProtonNativeProvider::new(config()).clone_for_transfer(),
            Err(ProviderError::NotConnected)
        ));
    }

    #[tokio::test]
    async fn a_disconnected_provider_says_so() {
        let mut p = ProtonNativeProvider::new(config());
        assert!(matches!(
            p.list("/").await,
            Err(ProviderError::NotConnected)
        ));
    }

    /// A pending sign-in is taken over only by a profile that names its
    /// account, or names none.
    #[test]
    fn a_profile_names_its_account_by_address_or_bare_name() {
        let addresses = ["Someone@proton.me", "alias@pm.me"];
        assert!(names_account("someone@proton.me", &addresses));
        assert!(names_account(" ALIAS@pm.me ", &addresses));
        assert!(names_account("someone", &addresses));
        assert!(!names_account("someone@pm.me", &addresses));
        assert!(!names_account("other", &addresses));
        assert!(!names_account("x@y.z", &[]));
    }
}
