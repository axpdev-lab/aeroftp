// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

//! Versioned backup for sync: before a sync overwrites or deletes a
//! destination file, move the destination copy to
//! `<destination root>/<backup dir>/<run stamp>/<relative path>`.
//!
//! One implementation for every caller: the AeroSync Plan tab (through the
//! `sync_backup_*` Tauri commands) and `aeroftp-cli sync --backup-dir`. The
//! rules are the same on both sides:
//!
//! - the backup folder is a relative path inside the destination root, with
//!   no `..` and no absolute or drive prefix ([`BackupDir::parse`]);
//! - one stamp per run, UTC, `YYYYMMDDTHHMMSSZ` ([`run_stamp`]);
//! - an archived copy never overwrites an earlier one: a taken name gets a
//!   numeric suffix before the extension, `name.1.ext`, `name.2.ext`;
//! - the backup folder is invisible to the sync on both sides, including the
//!   cleanup of emptied folders ([`is_backup_path`] is the only test);
//! - a missing destination is not an error: there is nothing to keep, so the
//!   archive returns `Ok(None)` and the caller writes the file as usual;
//! - on a remote that cannot move a file into another folder, the archive
//!   refuses before touching anything ([`remote_move_support`]); the caller
//!   is expected to ask first and say so before the run starts.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::providers::{ProviderError, ProviderType, StorageProvider};

/// The backup folder, validated: relative, `/`-separated, no empty, `.` or
/// `..` segment, no leading or trailing `/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupDir(String);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BackupDirError {
    #[error("the backup folder is empty")]
    Empty,
    #[error("the backup folder must be relative to the destination, not an absolute path")]
    Absolute,
    #[error("the backup folder must not contain '..'")]
    ParentSegment,
    #[error("the backup folder contains an invalid name: {0:?}")]
    InvalidSegment(String),
}

impl BackupDirError {
    /// Stable identifier for callers that translate the message.
    pub fn code(&self) -> &'static str {
        match self {
            BackupDirError::Empty => "empty",
            BackupDirError::Absolute => "absolute",
            BackupDirError::ParentSegment => "parent",
            BackupDirError::InvalidSegment(_) => "invalid",
        }
    }
}

impl BackupDir {
    /// Validate a user-supplied backup folder.
    ///
    /// Backslashes count as separators, so a Windows-style `a\..\b` is
    /// refused like `a/../b`. A drive prefix (`C:`), a UNC or rooted path, a
    /// segment with a NUL or a `:` (an NTFS stream or a drive letter) are
    /// refused; `.` segments and doubled separators are refused rather than
    /// normalised, so what the user typed is what gets created.
    pub fn parse(input: &str) -> Result<Self, BackupDirError> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(BackupDirError::Empty);
        }
        let unified = trimmed.replace('\\', "/");
        if unified.starts_with('/') || has_drive_prefix(&unified) {
            return Err(BackupDirError::Absolute);
        }
        let unified = unified.trim_end_matches('/');
        let mut segments = Vec::new();
        for segment in unified.split('/') {
            match segment {
                ".." => return Err(BackupDirError::ParentSegment),
                "" | "." => return Err(BackupDirError::InvalidSegment(segment.to_string())),
                s if s.contains('\0') || s.contains(':') => {
                    return Err(BackupDirError::InvalidSegment(s.to_string()))
                }
                s => segments.push(s),
            }
        }
        Ok(BackupDir(segments.join("/")))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The folders the backup folder sits in, outermost first, without the
    /// destination root and without the backup folder itself: `a/b/c` gives
    /// `["a", "a/b"]`. A sync that writes into one of them would be writing
    /// around its own backups, which the Plan refuses.
    pub fn ancestors(&self) -> Vec<&str> {
        self.0
            .match_indices('/')
            .map(|(i, _)| &self.0[..i])
            .collect()
    }
}

fn has_drive_prefix(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// The stamp every archive of one run shares: UTC, `YYYYMMDDTHHMMSSZ`, which
/// sorts by time as text and holds no character a filesystem refuses.
pub fn run_stamp(now: DateTime<Utc>) -> String {
    now.format("%Y%m%dT%H%M%SZ").to_string()
}

/// True for the backup folder itself and everything under it. `rel` is a
/// path relative to the sync root with `/` separators; a leading `/` or `./`
/// is ignored. This is the one test for "the sync must not see this", on
/// both sides and for the cleanup of emptied folders.
pub fn is_backup_path(rel: &str, dir: &BackupDir) -> bool {
    let rel = rel.trim_start_matches("./").trim_start_matches('/');
    let dir = dir.as_str();
    rel == dir
        || (rel.len() > dir.len() && rel.starts_with(dir) && rel.as_bytes()[dir.len()] == b'/')
}

/// Where the copy of `rel` from the run `stamp` goes, relative to the root,
/// before any collision suffix.
pub fn archive_rel_path(dir: &BackupDir, stamp: &str, rel: &str) -> String {
    format!(
        "{}/{}/{}",
        dir.as_str(),
        stamp,
        rel.trim_start_matches("./").trim_start_matches('/')
    )
}

/// `name.ext` with the `n`th collision suffix: `name.1.ext`. A name with no
/// extension, or a dotfile like `.env`, gets the suffix at the end.
fn with_suffix(rel: &str, n: u32) -> String {
    let (parent, name) = match rel.rfind('/') {
        Some(i) => (&rel[..=i], &rel[i + 1..]),
        None => ("", rel),
    };
    match name.rfind('.') {
        Some(dot) if dot > 0 => format!("{}{}.{}{}", parent, &name[..dot], n, &name[dot..]),
        _ => format!("{}{}.{}", parent, name, n),
    }
}

/// Upper bound on collision suffixes, so a folder that answers "exists" to
/// every name cannot loop forever.
const MAX_SUFFIX: u32 = 10_000;

/// Move `<root>/<rel>` to `<root>/<dir>/<stamp>/<rel>` on the local disk.
///
/// `Ok(None)` when `<root>/<rel>` does not exist. The move is a rename inside
/// one root; a rename that fails (for instance across a mount point inside
/// the root) is returned as the error, not replaced by a copy, and the
/// source is then untouched.
pub fn archive_local(
    root: &Path,
    dir: &BackupDir,
    stamp: &str,
    rel: &str,
) -> std::io::Result<Option<PathBuf>> {
    let rel = rel.trim_start_matches("./").trim_start_matches('/');
    let source = root.join(rel);
    match std::fs::symlink_metadata(&source) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    }
    let base = archive_rel_path(dir, stamp, rel);
    let mut target = root.join(&base);
    let mut n = 0;
    while std::fs::symlink_metadata(&target).is_ok() {
        n += 1;
        if n > MAX_SUFFIX {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("no free backup name for {}", base),
            ));
        }
        target = root.join(with_suffix(&base, n));
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(&source, &target)?;
    Ok(Some(target))
}

/// How a remote moves a file into another folder, read from each provider's
/// `rename` (and `server_copy` where the rename is built on it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteMove {
    /// One server operation, or a server move followed by a rename; no data
    /// is copied.
    Native,
    /// A server-side copy followed by a delete: the whole object is copied
    /// again on the server, and the two steps are not atomic.
    ServerCopyDelete,
    /// Download, upload under the new name, delete: the file travels through
    /// this machine twice, and the steps are not atomic.
    ClientCopyDelete,
    /// No way to put a file into another folder; the reason is for logs.
    Unsupported(&'static str),
}

/// The move each provider type offers, from its code. The match has no
/// wildcard on purpose: a new provider type does not compile until someone
/// answers for it.
pub fn remote_move_support(provider: ProviderType) -> RemoteMove {
    use ProviderType as P;
    use RemoteMove::*;
    match provider {
        P::Ftp | P::Ftps => Native,       // RNFR/RNTO with full paths
        P::Sftp => Native,                // SSH_FXP_RENAME
        P::WebDav => Native,              // MOVE, Overwrite: F
        P::GoogleDrive => Native,         // PATCH addParents/removeParents
        P::Dropbox => Native,             // files/move_v2
        P::OneDrive => Native,            // PATCH parentReference
        P::Box => Native,                 // PUT parent.id
        P::PCloud => Native,              // renamefile topath
        P::Mega => Native,                // native `m` / mega-mv
        P::Proton => Native,              // CLI move, then rename
        P::FourShared => Native,          // move, then rename
        P::ZohoWorkdrive => Native,       // move, then rename
        P::Internxt => Native,            // PATCH destinationFolder
        P::KDrive => Native,              // POST move/{parent}
        P::Twake => Native,               // one PATCH with dir_id and name
        P::Jottacloud => Native,          // ?mv=
        P::DrimeCloud => Native,          // move, then rename
        P::FileLu => Native,              // set_folder, then rename
        P::Koofr => Native,               // PUT files/move
        P::OpenDrive => Native,           // move_copy move=true
        P::YandexDisk => Native,          // resources/move
        P::GitLab => Native,              // commit `move` action
        P::ImageKit => Native,            // file move (folders are refused, files are what we move)
        P::Cloudinary => Native,          // public_id rename
        P::S3 => ServerCopyDelete,        // CopyObject (multipart copy above 5 GiB), then delete
        P::Azure => ServerCopyDelete,     // Copy Blob, then delete
        P::Swift => ServerCopyDelete,     // X-Copy-From, then delete
        P::Backblaze => ServerCopyDelete, // b2_copy_file, then delete
        P::GitHub => ClientCopyDelete,    // download, commit, delete
        // rename() keeps only the leaf name of the destination and renames in
        // place with Ok (filen/mod.rs, fn rename): a "move" would stay put.
        P::Filen => Unsupported("rename keeps the file in its folder"),
        P::GooglePhotos => Unsupported("rename is not supported"),
        P::Immich => Unsupported("rename is not supported"),
        P::Uploadcare => Unsupported("rename is not supported"),
        P::AeroVaultMount => Unsupported("read-only mount"),
        P::Peer => Unsupported("read-only peer"),
        // libmtp refuses rename; the gvfs mount's cross-folder rename is untested.
        P::Mtp => Unsupported("rename is not supported"),
        // Not a connection type: AeroCloud connects through its backend's provider.
        P::AeroCloud => Unsupported("not a connection type"),
    }
}

fn join_remote(root: &str, rel: &str) -> String {
    let root = root.trim_end_matches('/');
    format!("{}/{}", root, rel.trim_start_matches('/'))
}

/// The three operations an archive needs from a remote, so the provider path
/// and the GUI's own FTP session run the same archive code.
#[async_trait]
pub trait ArchiveRemote: Send {
    /// How this remote moves a file into another folder.
    fn move_support(&self) -> RemoteMove;
    /// Name used in error messages.
    fn kind_name(&self) -> String;
    /// Whether a file or folder is at `path`. An error means "could not
    /// tell", never "no".
    async fn path_exists(&mut self, path: &str) -> Result<bool, ProviderError>;
    async fn make_dir(&mut self, path: &str) -> Result<(), ProviderError>;
    async fn move_path(&mut self, from: &str, to: &str) -> Result<(), ProviderError>;
}

/// A [`StorageProvider`] seen as an archive target.
pub struct ProviderRemote<'a>(pub &'a mut dyn StorageProvider);

#[async_trait]
impl ArchiveRemote for ProviderRemote<'_> {
    fn move_support(&self) -> RemoteMove {
        remote_move_support(self.0.provider_type())
    }
    fn kind_name(&self) -> String {
        self.0.provider_type().to_string()
    }
    async fn path_exists(&mut self, path: &str) -> Result<bool, ProviderError> {
        self.0.exists(path).await
    }
    async fn make_dir(&mut self, path: &str) -> Result<(), ProviderError> {
        self.0.mkdir(path).await
    }
    async fn move_path(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        self.0.rename(from, to).await
    }
}

/// The GUI's FTP session. RNFR/RNTO takes full paths, so it moves across
/// folders; on Unix servers it also replaces an occupied destination, which
/// is why the archive picks a free name before it moves.
#[async_trait]
impl ArchiveRemote for crate::ftp::FtpManager {
    fn move_support(&self) -> RemoteMove {
        RemoteMove::Native
    }
    fn kind_name(&self) -> String {
        "FTP".to_string()
    }
    async fn path_exists(&mut self, path: &str) -> Result<bool, ProviderError> {
        self.exists(path)
            .await
            .map_err(|e| ProviderError::ServerError(e.to_string()))
    }
    async fn make_dir(&mut self, path: &str) -> Result<(), ProviderError> {
        self.mkdir(path)
            .await
            .map_err(|e| ProviderError::ServerError(e.to_string()))
    }
    async fn move_path(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        self.rename(from, to)
            .await
            .map_err(|e| ProviderError::ServerError(e.to_string()))
    }
}

/// Create `<root>/<rel_dir>` one level at a time. A level that already exists
/// is skipped; a failed mkdir is accepted only if the level exists afterwards
/// (providers report an existing folder with many different errors), and
/// any other failure is returned.
async fn mkdir_levels<R: ArchiveRemote + ?Sized>(
    remote: &mut R,
    root: &str,
    rel_dir: &str,
) -> Result<(), ProviderError> {
    let mut current = String::new();
    for segment in rel_dir.split('/').filter(|s| !s.is_empty()) {
        if !current.is_empty() {
            current.push('/');
        }
        current.push_str(segment);
        let path = join_remote(root, &current);
        if remote.path_exists(&path).await? {
            continue;
        }
        if let Err(e) = remote.make_dir(&path).await {
            if !remote.path_exists(&path).await.unwrap_or(false) {
                return Err(e);
            }
        }
    }
    Ok(())
}

/// Move `<root>/<rel>` to `<root>/<dir>/<stamp>/<rel>` on a remote.
///
/// Refuses with [`ProviderError::NotSupported`] before touching anything when
/// the provider cannot move a file into another folder. `Ok(None)` when
/// `<root>/<rel>` does not exist. After the move the target must exist, or
/// the archive fails: a provider that answered Ok without moving must not
/// let the caller overwrite the only copy.
pub async fn archive_remote(
    provider: &mut dyn StorageProvider,
    root: &str,
    dir: &BackupDir,
    stamp: &str,
    rel: &str,
) -> Result<Option<String>, ProviderError> {
    archive_remote_on(&mut ProviderRemote(provider), root, dir, stamp, rel).await
}

/// [`archive_remote`] on any [`ArchiveRemote`].
pub async fn archive_remote_on<R: ArchiveRemote + ?Sized>(
    remote: &mut R,
    root: &str,
    dir: &BackupDir,
    stamp: &str,
    rel: &str,
) -> Result<Option<String>, ProviderError> {
    if let RemoteMove::Unsupported(why) = remote.move_support() {
        return Err(ProviderError::NotSupported(format!(
            "versioned backup needs to move files into another folder, and {} cannot: {}",
            remote.kind_name(),
            why
        )));
    }
    let rel = rel.trim_start_matches("./").trim_start_matches('/');
    let source = join_remote(root, rel);
    if !remote.path_exists(&source).await? {
        return Ok(None);
    }
    let base = archive_rel_path(dir, stamp, rel);
    // The folders first: some providers cannot answer "is this there?" for a
    // path whose parent is missing (FTP lists the parent and gets a 550), so
    // the free-name check below needs the parent to exist already.
    if let Some(i) = base.rfind('/') {
        mkdir_levels(remote, root, &base[..i]).await?;
    }
    let mut target_rel = base.clone();
    let mut n = 0;
    while remote.path_exists(&join_remote(root, &target_rel)).await? {
        n += 1;
        if n > MAX_SUFFIX {
            return Err(ProviderError::AlreadyExists(format!(
                "no free backup name for {}",
                base
            )));
        }
        target_rel = with_suffix(&base, n);
    }
    let target = join_remote(root, &target_rel);
    remote.move_path(&source, &target).await?;
    if !remote.path_exists(&target).await? {
        return Err(ProviderError::Other(format!(
            "the server reported {} moved to {}, but it is not there",
            source, target
        )));
    }
    Ok(Some(target))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::RemoteEntry;
    use async_trait::async_trait;
    use chrono::TimeZone;
    use std::collections::BTreeSet;

    fn dir(s: &str) -> BackupDir {
        BackupDir::parse(s).unwrap()
    }

    #[test]
    fn parse_accepts_relative_folders_and_refuses_everything_else() {
        assert_eq!(dir(".aeroftp-versions").as_str(), ".aeroftp-versions");
        assert_eq!(dir(" a/b/ ").as_str(), "a/b");
        assert_eq!(dir("a\\b").as_str(), "a/b");
        assert_eq!(BackupDir::parse(""), Err(BackupDirError::Empty));
        assert_eq!(BackupDir::parse("   "), Err(BackupDirError::Empty));
        assert_eq!(BackupDir::parse("/abs"), Err(BackupDirError::Absolute));
        assert_eq!(
            BackupDir::parse("\\\\server\\share"),
            Err(BackupDirError::Absolute)
        );
        assert_eq!(BackupDir::parse("C:\\x"), Err(BackupDirError::Absolute));
        assert_eq!(BackupDir::parse("c:x"), Err(BackupDirError::Absolute));
        assert_eq!(BackupDir::parse(".."), Err(BackupDirError::ParentSegment));
        assert_eq!(
            BackupDir::parse("a/../b"),
            Err(BackupDirError::ParentSegment)
        );
        assert_eq!(
            BackupDir::parse("a\\..\\b"),
            Err(BackupDirError::ParentSegment)
        );
        assert!(matches!(
            BackupDir::parse("a//b"),
            Err(BackupDirError::InvalidSegment(_))
        ));
        assert!(matches!(
            BackupDir::parse("./a"),
            Err(BackupDirError::InvalidSegment(_))
        ));
        assert!(matches!(
            BackupDir::parse("a/file:stream"),
            Err(BackupDirError::InvalidSegment(_))
        ));
    }

    #[test]
    fn ancestors_are_the_folders_around_the_backup_folder() {
        assert!(dir("v").ancestors().is_empty());
        assert_eq!(dir("a/b/c").ancestors(), vec!["a", "a/b"]);
    }

    #[test]
    fn stamp_is_utc_and_sortable() {
        let t = Utc.with_ymd_and_hms(2026, 9, 25, 7, 5, 9).unwrap();
        assert_eq!(run_stamp(t), "20260925T070509Z");
    }

    #[test]
    fn backup_path_is_the_folder_and_what_is_under_it_only() {
        let d = dir(".aeroftp-versions");
        assert!(is_backup_path(".aeroftp-versions", &d));
        assert!(is_backup_path(
            ".aeroftp-versions/20260925T070509Z/a.txt",
            &d
        ));
        assert!(is_backup_path("/.aeroftp-versions/x", &d));
        assert!(is_backup_path("./.aeroftp-versions", &d));
        assert!(!is_backup_path(".aeroftp-versions-old/x", &d));
        assert!(!is_backup_path("docs/.aeroftp-versions/x", &d));
        assert!(!is_backup_path(".aeroftp", &d));
        let nested = dir("keep/v");
        assert!(is_backup_path("keep/v/s/x", &nested));
        assert!(!is_backup_path("keep/x", &nested));
    }

    #[test]
    fn collision_suffix_goes_before_the_extension() {
        assert_eq!(with_suffix("v/s/a/report.pdf", 1), "v/s/a/report.1.pdf");
        assert_eq!(with_suffix("v/s/archive.tar.gz", 2), "v/s/archive.tar.2.gz");
        assert_eq!(with_suffix("v/s/Makefile", 1), "v/s/Makefile.1");
        assert_eq!(with_suffix("v/s/.env", 3), "v/s/.env.3");
        assert_eq!(with_suffix("v.d/s/noext", 1), "v.d/s/noext.1");
    }

    #[test]
    fn archive_local_moves_and_never_overwrites_an_earlier_copy() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let d = dir(".aeroftp-versions");
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(root.join("docs/a.txt"), b"one").unwrap();

        let first = archive_local(root, &d, "S", "docs/a.txt").unwrap().unwrap();
        assert_eq!(first, root.join(".aeroftp-versions/S/docs/a.txt"));
        assert!(!root.join("docs/a.txt").exists());
        assert_eq!(std::fs::read(&first).unwrap(), b"one");

        // Same run, same file again (deleted and rewritten in one run).
        std::fs::write(root.join("docs/a.txt"), b"two").unwrap();
        let second = archive_local(root, &d, "S", "docs/a.txt").unwrap().unwrap();
        assert_eq!(second, root.join(".aeroftp-versions/S/docs/a.1.txt"));
        assert_eq!(std::fs::read(&first).unwrap(), b"one");
        assert_eq!(std::fs::read(&second).unwrap(), b"two");
    }

    #[test]
    fn archive_local_of_a_missing_file_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        let got = archive_local(tmp.path(), &dir("v"), "S", "nope.txt").unwrap();
        assert_eq!(got, None);
        assert!(!tmp.path().join("v").exists());
    }

    /// Every provider type answers explicitly; this pins each answer so a
    /// change of class is a visible diff here, not a silent one.
    #[test]
    fn every_provider_type_has_an_explicit_move_answer() {
        use ProviderType as P;
        let expected: &[(ProviderType, &str)] = &[
            (P::Ftp, "native"),
            (P::Ftps, "native"),
            (P::Sftp, "native"),
            (P::WebDav, "native"),
            (P::S3, "server-copy"),
            (P::AeroCloud, "unsupported"),
            (P::GoogleDrive, "native"),
            (P::Dropbox, "native"),
            (P::OneDrive, "native"),
            (P::Mega, "native"),
            (P::Proton, "native"),
            (P::Box, "native"),
            (P::PCloud, "native"),
            (P::Azure, "server-copy"),
            (P::Filen, "unsupported"),
            (P::FourShared, "native"),
            (P::ZohoWorkdrive, "native"),
            (P::Internxt, "native"),
            (P::KDrive, "native"),
            (P::Jottacloud, "native"),
            (P::DrimeCloud, "native"),
            (P::FileLu, "native"),
            (P::Koofr, "native"),
            (P::OpenDrive, "native"),
            (P::YandexDisk, "native"),
            (P::GitHub, "client-copy"),
            (P::GitLab, "native"),
            (P::Swift, "server-copy"),
            (P::GooglePhotos, "unsupported"),
            (P::Immich, "unsupported"),
            (P::ImageKit, "native"),
            (P::Uploadcare, "unsupported"),
            (P::Backblaze, "server-copy"),
            (P::Cloudinary, "native"),
            (P::AeroVaultMount, "unsupported"),
            (P::Peer, "unsupported"),
            (P::Mtp, "unsupported"),
        ];
        // 37 variants today; the match in remote_move_support is what makes a
        // 38th fail to compile. This count makes it fail here too.
        assert_eq!(expected.len(), 37);
        let unique: BTreeSet<String> = expected.iter().map(|(p, _)| format!("{:?}", p)).collect();
        assert_eq!(
            unique.len(),
            expected.len(),
            "a provider type is listed twice"
        );
        for (p, class) in expected {
            let got = match remote_move_support(*p) {
                RemoteMove::Native => "native",
                RemoteMove::ServerCopyDelete => "server-copy",
                RemoteMove::ClientCopyDelete => "client-copy",
                RemoteMove::Unsupported(_) => "unsupported",
            };
            assert_eq!(&got, class, "{:?}", p);
        }
    }

    /// A remote tree in memory. `mkdir` needs the parent, like the id-based
    /// providers; `rename` needs the target's parent and refuses an occupied
    /// target. `leaf_only_rename` behaves like Filen's rename: it keeps the
    /// file in its folder and answers Ok.
    struct MemTree {
        kind: ProviderType,
        files: BTreeSet<String>,
        dirs: BTreeSet<String>,
        leaf_only_rename: bool,
        calls: Vec<String>,
    }

    impl MemTree {
        fn new(kind: ProviderType, files: &[&str]) -> Self {
            let mut dirs = BTreeSet::new();
            dirs.insert("/r".to_string());
            for f in files {
                let mut p = f.to_string();
                while let Some(i) = p.rfind('/') {
                    p.truncate(i);
                    if p.is_empty() {
                        break;
                    }
                    dirs.insert(p.clone());
                }
            }
            MemTree {
                kind,
                files: files.iter().map(|s| s.to_string()).collect(),
                dirs,
                leaf_only_rename: false,
                calls: Vec::new(),
            }
        }
        fn parent(p: &str) -> String {
            p[..p.rfind('/').unwrap()].to_string()
        }
    }

    #[async_trait]
    impl StorageProvider for MemTree {
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
        fn provider_type(&self) -> ProviderType {
            self.kind
        }
        fn display_name(&self) -> String {
            "mem".into()
        }
        async fn connect(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn disconnect(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        fn is_connected(&self) -> bool {
            true
        }
        async fn list(&mut self, _p: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
            Ok(vec![])
        }
        async fn pwd(&mut self) -> Result<String, ProviderError> {
            Ok("/".into())
        }
        async fn cd(&mut self, _p: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn cd_up(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn download(
            &mut self,
            _r: &str,
            _l: &str,
            _cb: Option<Box<dyn Fn(u64, u64) + Send>>,
        ) -> Result<(), ProviderError> {
            Err(ProviderError::NotSupported("download".into()))
        }
        async fn download_to_bytes(&mut self, _r: &str) -> Result<Vec<u8>, ProviderError> {
            Err(ProviderError::NotSupported("download".into()))
        }
        async fn upload(
            &mut self,
            _l: &str,
            _r: &str,
            _cb: Option<Box<dyn Fn(u64, u64) + Send>>,
        ) -> Result<(), ProviderError> {
            Err(ProviderError::NotSupported("upload".into()))
        }
        async fn mkdir(&mut self, p: &str) -> Result<(), ProviderError> {
            self.calls.push(format!("mkdir {}", p));
            if self.dirs.contains(p) {
                // Like SFTP: a generic error for a folder that is there.
                return Err(ProviderError::ServerError(
                    "Failed to create directory".into(),
                ));
            }
            if !self.dirs.contains(&Self::parent(p)) {
                return Err(ProviderError::NotFound(Self::parent(p)));
            }
            self.dirs.insert(p.to_string());
            Ok(())
        }
        async fn delete(&mut self, _p: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn rmdir(&mut self, _p: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn rmdir_recursive(&mut self, _p: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
            self.calls.push(format!("rename {} {}", from, to));
            if !self.files.contains(from) {
                return Err(ProviderError::NotFound(from.into()));
            }
            if self.leaf_only_rename {
                let leaf = &to[to.rfind('/').unwrap() + 1..];
                let moved = format!("{}/{}", Self::parent(from), leaf);
                self.files.remove(from);
                self.files.insert(moved);
                return Ok(());
            }
            if !self.dirs.contains(&Self::parent(to)) {
                return Err(ProviderError::InvalidPath(
                    "destination parent does not exist".into(),
                ));
            }
            if self.files.contains(to) {
                return Err(ProviderError::AlreadyExists(to.into()));
            }
            self.files.remove(from);
            self.files.insert(to.to_string());
            Ok(())
        }
        async fn stat(&mut self, _p: &str) -> Result<RemoteEntry, ProviderError> {
            Err(ProviderError::NotSupported("stat".into()))
        }
        async fn size(&mut self, _p: &str) -> Result<u64, ProviderError> {
            Ok(0)
        }
        async fn exists(&mut self, p: &str) -> Result<bool, ProviderError> {
            // Like FTP, which answers by listing the parent: a missing parent
            // is an error, not a "no".
            if !self.dirs.contains(&Self::parent(p)) {
                return Err(ProviderError::ServerError("550 file does not exist".into()));
            }
            Ok(self.files.contains(p) || self.dirs.contains(p))
        }
        async fn keep_alive(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn server_info(&mut self) -> Result<String, ProviderError> {
            Ok("mem".into())
        }
    }

    #[tokio::test]
    async fn archive_remote_creates_the_levels_and_moves() {
        let mut p = MemTree::new(ProviderType::Sftp, &["/r/docs/a.txt"]);
        let d = dir(".aeroftp-versions");
        let got = archive_remote(&mut p, "/r", &d, "S", "docs/a.txt")
            .await
            .unwrap();
        assert_eq!(got.as_deref(), Some("/r/.aeroftp-versions/S/docs/a.txt"));
        assert!(!p.files.contains("/r/docs/a.txt"));
        assert!(p.files.contains("/r/.aeroftp-versions/S/docs/a.txt"));

        // A second archive in the same run reuses the existing levels (whose
        // mkdir would fail with a generic error) and takes a suffix.
        p.files.insert("/r/docs/a.txt".into());
        let again = archive_remote(&mut p, "/r", &d, "S", "docs/a.txt")
            .await
            .unwrap();
        assert_eq!(
            again.as_deref(),
            Some("/r/.aeroftp-versions/S/docs/a.1.txt")
        );
        assert!(p.files.contains("/r/.aeroftp-versions/S/docs/a.txt"));
    }

    #[tokio::test]
    async fn archive_remote_of_a_missing_file_is_none_and_creates_nothing() {
        let mut p = MemTree::new(ProviderType::Koofr, &[]);
        let got = archive_remote(&mut p, "/r", &dir("v"), "S", "x.txt")
            .await
            .unwrap();
        assert_eq!(got, None);
        assert!(p.calls.is_empty(), "{:?}", p.calls);
    }

    #[tokio::test]
    async fn archive_remote_refuses_before_touching_an_unsupported_remote() {
        let mut p = MemTree::new(ProviderType::Filen, &["/r/a.txt"]);
        let err = archive_remote(&mut p, "/r", &dir("v"), "S", "a.txt")
            .await
            .unwrap_err();
        assert!(matches!(err, ProviderError::NotSupported(_)), "{err:?}");
        assert!(p.calls.is_empty(), "{:?}", p.calls);
        assert!(p.files.contains("/r/a.txt"));
    }

    #[tokio::test]
    async fn archive_remote_fails_when_the_move_did_not_happen() {
        // A provider that answers Ok but leaves the file in its folder under
        // the new leaf name: the target is missing, so the archive fails and
        // the caller must not overwrite.
        let mut p = MemTree::new(ProviderType::Sftp, &["/r/a.txt"]);
        p.leaf_only_rename = true;
        let err = archive_remote(&mut p, "/r", &dir("v"), "S", "a.txt")
            .await
            .unwrap_err();
        assert!(matches!(err, ProviderError::Other(_)), "{err:?}");
    }

    #[tokio::test]
    async fn archive_remote_at_the_server_root() {
        let mut p = MemTree::new(ProviderType::Ftp, &["/a.txt"]);
        p.dirs.insert("/".into());
        // mkdir of "/v" needs parent "/", which MemTree::parent gives as "".
        p.dirs.insert(String::new());
        let got = archive_remote(&mut p, "/", &dir("v"), "S", "a.txt")
            .await
            .unwrap();
        assert_eq!(got.as_deref(), Some("/v/S/a.txt"));
    }
}
