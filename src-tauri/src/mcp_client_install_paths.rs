//! Bounded, link-free package snapshots. No host directory is mounted directly.
// SPDX-License-Identifier: GPL-3.0-or-later
use crate::mcp_client_config::McpDirectoryGrant;
#[cfg(target_os = "linux")]
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use tokio_util::sync::CancellationToken;

pub(crate) const MAX_FILES: usize = 8192;
pub(crate) const MAX_TREE_BYTES: u64 = 256 * 1024 * 1024;
pub(crate) const MAX_ARCHIVE_BYTES: usize = 64 * 1024 * 1024;

pub(crate) fn relative_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path.components().all(|c| matches!(c, Component::Normal(_)))
        && path.to_str().is_some_and(|s| {
            s.len() <= 1024
                && !s.contains('\\')
                && !s.contains(':')
                && !s.chars().any(char::is_control)
        })
}

pub(crate) fn check_cancel(cancel: &CancellationToken) -> Result<(), &'static str> {
    if cancel.is_cancelled() {
        Err("MCP_INSTALL_CANCELLED")
    } else {
        Ok(())
    }
}

/// Never grant a system root, an ancestor of HOME, or private application/credential trees.
/// Child projects under HOME are allowed with explicit user consent.
pub(crate) fn custom_path(path: &Path) -> Result<PathBuf, &'static str> {
    let canonical = path.canonicalize().map_err(|_| "MCP_DIRECTORY_INVALID")?;
    if canonical != path
        || !canonical.is_dir()
        || [
            "/usr", "/etc", "/proc", "/sys", "/dev", "/run", "/lib", "/lib64", "/bin", "/sbin",
            "/root",
        ]
        .iter()
        .any(|p| canonical.starts_with(p))
        || (canonical.starts_with("/var") && !canonical.starts_with("/var/www"))
        || crate::portable::credential_store_dir().is_some_and(|protected| {
            let protected = protected.canonicalize().unwrap_or(protected);
            protected.starts_with(&canonical) || canonical.starts_with(protected)
        })
        || std::env::var_os("HOME").is_some_and(|home| Path::new(&home).starts_with(&canonical))
        || canonical.parent().is_none()
        || canonical == Path::new("/tmp")
        || canonical == Path::new("/home")
        || canonical.components().any(|c| {
            c.as_os_str().to_str().is_some_and(|s| {
                matches!(
                    s,
                    ".ssh"
                        | ".gnupg"
                        | ".config"
                        | ".local"
                        | ".codex"
                        | ".claude"
                        | ".aws"
                        | ".azure"
                        | ".kube"
                        | ".cache"
                ) || s.eq_ignore_ascii_case("aeroftp")
                    || s.eq_ignore_ascii_case("mcp-servers")
            })
        })
    {
        return Err("MCP_DIRECTORY_INVALID");
    }
    Ok(canonical)
}

#[cfg(target_os = "linux")]
pub(crate) fn directory_grant(path: &Path) -> Result<McpDirectoryGrant, &'static str> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| "MCP_DIRECTORY_INVALID")?;
    let meta = file.metadata().map_err(|_| "MCP_DIRECTORY_INVALID")?;
    Ok(McpDirectoryGrant {
        path: path.to_str().ok_or("MCP_DIRECTORY_INVALID")?.into(),
        device: meta.dev(),
        inode: meta.ino(),
    })
}
#[cfg(not(target_os = "linux"))]
pub(crate) fn directory_grant(_: &Path) -> Result<McpDirectoryGrant, &'static str> {
    Err("MCP_STDIO_SANDBOX_UNAVAILABLE")
}

/// Safe extraction never interprets an archive's modes, ownership, links or scripts.
pub(crate) fn extract_archive(
    bytes: &[u8],
    destination: &Path,
    cancel: &CancellationToken,
) -> Result<(), &'static str> {
    let reader = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(reader);
    let mut total = 0u64;
    let mut seen = std::collections::HashSet::new();
    for entry in archive
        .entries()
        .map_err(|_| "MCP_INSTALL_ARCHIVE")?
        .raw(true)
    {
        check_cancel(cancel)?;
        let mut entry = entry.map_err(|_| "MCP_INSTALL_ARCHIVE")?;
        let path = entry
            .path()
            .map_err(|_| "MCP_INSTALL_ARCHIVE")?
            .into_owned();
        if !relative_path(&path)
            || path.components().count() > 32
            || !seen.insert(path.clone())
            || seen.len() > MAX_FILES
        {
            return Err("MCP_INSTALL_ARCHIVE");
        }
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            std::fs::create_dir_all(destination.join(path)).map_err(|_| "MCP_INSTALL_IO")?;
        } else if kind.is_file() {
            let size = entry.size();
            total = total.checked_add(size).ok_or("MCP_INSTALL_LIMIT")?;
            if total > MAX_TREE_BYTES {
                return Err("MCP_INSTALL_LIMIT");
            }
            let target = destination.join(path);
            std::fs::create_dir_all(target.parent().ok_or("MCP_INSTALL_ARCHIVE")?)
                .map_err(|_| "MCP_INSTALL_IO")?;
            let mut output = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(target)
                .map_err(|_| "MCP_INSTALL_ARCHIVE")?;
            let mut chunk = [0u8; 65536];
            loop {
                check_cancel(cancel)?;
                let count = entry.read(&mut chunk).map_err(|_| "MCP_INSTALL_ARCHIVE")?;
                if count == 0 {
                    break;
                }
                output
                    .write_all(&chunk[..count])
                    .map_err(|_| "MCP_INSTALL_IO")?;
            }
        } else {
            return Err("MCP_INSTALL_ARCHIVE");
        }
    }
    Ok(())
}

/// Read a source via pinned, NOFOLLOW directory/file descriptors, never through a
/// path that can be swapped after checking it. Hash the private copy, then mount it.
#[cfg(target_os = "linux")]
pub(crate) fn snapshot(
    grant: &McpDirectoryGrant,
    cancel: &CancellationToken,
) -> Result<(tempfile::TempDir, String), &'static str> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    fn open(path: &Path, directory: bool) -> Result<std::fs::File, &'static str> {
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(
                libc::O_NOFOLLOW | libc::O_NONBLOCK | if directory { libc::O_DIRECTORY } else { 0 },
            )
            .open(path)
            .map_err(|_| "MCP_DIRECTORY_CHANGED")
    }
    struct Budget {
        count: usize,
        total: u64,
        hash: Sha256,
    }
    fn copy_tree(
        source: &std::fs::File,
        destination: &Path,
        relative: &Path,
        depth: usize,
        budget: &mut Budget,
        cancel: &CancellationToken,
    ) -> Result<(), &'static str> {
        if depth > 32 {
            return Err("MCP_INSTALL_LIMIT");
        }
        let anchor = PathBuf::from(format!("/proc/self/fd/{}", source.as_raw_fd()));
        let mut entries = std::fs::read_dir(&anchor)
            .map_err(|_| "MCP_DIRECTORY_CHANGED")?
            .take(MAX_FILES + 1)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| "MCP_DIRECTORY_CHANGED")?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            check_cancel(cancel)?;
            budget.count += 1;
            if budget.count > MAX_FILES {
                return Err("MCP_INSTALL_LIMIT");
            }
            let name = entry.file_name();
            if name
                .to_str()
                .is_some_and(|name| matches!(name, ".ssh" | ".gnupg" | ".aws" | ".azure" | ".kube"))
            {
                return Err("MCP_DIRECTORY_INVALID");
            }
            let rel = relative.join(&name);
            if !relative_path(&rel) {
                return Err("MCP_DIRECTORY_INVALID");
            }
            let kind = entry.file_type().map_err(|_| "MCP_DIRECTORY_CHANGED")?;
            let target = destination.join(&name);
            let mut file = open(&anchor.join(&name), kind.is_dir())?;
            let meta = file.metadata().map_err(|_| "MCP_DIRECTORY_CHANGED")?;
            let path = rel.to_str().ok_or("MCP_DIRECTORY_INVALID")?.as_bytes();
            budget.hash.update((path.len() as u64).to_le_bytes());
            budget.hash.update(path);
            if meta.is_dir() {
                budget.hash.update(b"d");
                std::fs::create_dir(&target).map_err(|_| "MCP_INSTALL_IO")?;
                copy_tree(&file, &target, &rel, depth + 1, budget, cancel)?;
            } else if meta.is_file() && meta.nlink() == 1 {
                budget.hash.update(b"f");
                budget.hash.update(meta.len().to_le_bytes());
                budget.total = budget
                    .total
                    .checked_add(meta.len())
                    .ok_or("MCP_INSTALL_LIMIT")?;
                if budget.total > MAX_TREE_BYTES {
                    return Err("MCP_INSTALL_LIMIT");
                }
                let mut output = std::fs::File::create(target).map_err(|_| "MCP_INSTALL_IO")?;
                let mut remaining = meta.len();
                let mut chunk = [0u8; 65536];
                while remaining > 0 {
                    check_cancel(cancel)?;
                    let bound = (remaining as usize).min(chunk.len());
                    let n = file
                        .read(&mut chunk[..bound])
                        .map_err(|_| "MCP_DIRECTORY_CHANGED")?;
                    if n == 0 {
                        return Err("MCP_DIRECTORY_CHANGED");
                    }
                    output
                        .write_all(&chunk[..n])
                        .map_err(|_| "MCP_INSTALL_IO")?;
                    budget.hash.update(&chunk[..n]);
                    remaining -= n as u64;
                }
                if file
                    .read(&mut chunk[..1])
                    .map_err(|_| "MCP_DIRECTORY_CHANGED")?
                    != 0
                {
                    return Err("MCP_DIRECTORY_CHANGED");
                }
            } else {
                return Err("MCP_DIRECTORY_INVALID");
            }
        }
        Ok(())
    }
    let path = Path::new(&grant.path);
    if path.canonicalize().map_err(|_| "MCP_DIRECTORY_CHANGED")? != path {
        return Err("MCP_DIRECTORY_CHANGED");
    }
    let file = open(path, true)?;
    let meta = file.metadata().map_err(|_| "MCP_DIRECTORY_CHANGED")?;
    if meta.dev() != grant.device || meta.ino() != grant.inode {
        return Err("MCP_DIRECTORY_CHANGED");
    }
    let temp = tempfile::tempdir().map_err(|_| "MCP_INSTALL_IO")?;
    let mut budget = Budget {
        count: 0,
        total: 0,
        hash: Sha256::new(),
    };
    copy_tree(&file, temp.path(), Path::new(""), 0, &mut budget, cancel)?;
    Ok((temp, hex::encode(budget.hash.finalize())))
}
#[cfg(not(target_os = "linux"))]
pub(crate) fn snapshot(
    _: &McpDirectoryGrant,
    _: &CancellationToken,
) -> Result<(tempfile::TempDir, String), &'static str> {
    Err("MCP_STDIO_SANDBOX_UNAVAILABLE")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn archive(path: &str, kind: tar::EntryType, content: &[u8]) -> Vec<u8> {
        let gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut builder = tar::Builder::new(gzip);
        let mut header = tar::Header::new_gnu();
        // Raw attacker header, including paths that tar's safe builder rejects.
        header.as_mut_bytes()[..path.len()].copy_from_slice(path.as_bytes());
        header.set_entry_type(kind);
        header.set_size(content.len() as u64);
        header.set_mode(0o777);
        header.set_cksum();
        builder.append(&header, content).unwrap();
        builder.into_inner().unwrap().finish().unwrap()
    }
    #[test]
    fn archives_reject_traversal_absolute_windows_paths_links_and_metadata() {
        let cancel = CancellationToken::new();
        for (path, kind) in [
            ("../escape", tar::EntryType::Regular),
            ("/escape", tar::EntryType::Regular),
            ("C:\\escape", tar::EntryType::Regular),
            ("dir/../../escape", tar::EntryType::Regular),
            ("link", tar::EntryType::Symlink),
            ("hard", tar::EntryType::Link),
            ("fifo", tar::EntryType::Fifo),
            ("metadata", tar::EntryType::XHeader),
            ("long", tar::EntryType::GNULongName),
        ] {
            let dir = tempfile::tempdir().unwrap();
            assert_eq!(
                extract_archive(&archive(path, kind, b""), dir.path(), &cancel),
                Err("MCP_INSTALL_ARCHIVE"),
                "{path}"
            );
        }
        let dir = tempfile::tempdir().unwrap();
        extract_archive(
            &archive("package/main.js", tar::EntryType::Regular, b"hello"),
            dir.path(),
            &cancel,
        )
        .unwrap();
        assert_eq!(
            std::fs::read(dir.path().join("package/main.js")).unwrap(),
            b"hello"
        );
        assert!(extract_archive(
            &archive("package/main.js", tar::EntryType::Regular, b"overwrite"),
            dir.path(),
            &cancel
        )
        .is_err());
        cancel.cancel();
        assert_eq!(
            extract_archive(
                &archive("other", tar::EntryType::Regular, b""),
                dir.path(),
                &cancel
            ),
            Err("MCP_INSTALL_CANCELLED")
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn snapshots_pin_identity_hash_content_and_refuse_links_and_special_files() {
        use std::os::unix::fs::symlink;
        let parent = tempfile::tempdir().unwrap();
        let tree = parent.path().join("project");
        std::fs::create_dir(&tree).unwrap();
        std::fs::write(tree.join("module.js"), b"module").unwrap();
        let grant = directory_grant(&tree).unwrap();
        let cancel = CancellationToken::new();
        let (copy, initial) = snapshot(&grant, &cancel).unwrap();
        std::fs::write(tree.join("module.js"), b"tampered").unwrap();
        assert_eq!(
            std::fs::read(copy.path().join("module.js")).unwrap(),
            b"module"
        );
        assert_ne!(snapshot(&grant, &cancel).unwrap().1, initial);
        symlink("/etc/passwd", tree.join("link")).unwrap();
        assert_eq!(
            snapshot(&grant, &cancel).unwrap_err(),
            "MCP_DIRECTORY_CHANGED"
        );
        std::fs::remove_file(tree.join("link")).unwrap();
        std::fs::hard_link(tree.join("module.js"), tree.join("hard")).unwrap();
        assert_eq!(
            snapshot(&grant, &cancel).unwrap_err(),
            "MCP_DIRECTORY_INVALID"
        );
        std::fs::remove_file(tree.join("hard")).unwrap();
        std::fs::rename(&tree, parent.path().join("old")).unwrap();
        std::fs::create_dir(&tree).unwrap();
        assert_eq!(
            snapshot(&grant, &cancel).unwrap_err(),
            "MCP_DIRECTORY_CHANGED"
        );
        assert!(snapshot(&directory_grant(&tree).unwrap(), &cancel).is_ok());
        cancel.cancel();
        std::fs::write(tree.join("one"), b"one").unwrap();
        assert_eq!(
            snapshot(&directory_grant(&tree).unwrap(), &cancel).unwrap_err(),
            "MCP_INSTALL_CANCELLED"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn custom_grants_refuse_ambient_roots_sensitive_paths_and_aliases() {
        use std::os::unix::fs::symlink;
        for path in ["/", "/home", "/tmp", "/etc", "/usr", "/var", "/proc"] {
            assert!(custom_path(Path::new(path)).is_err(), "{path}");
        }
        if let Some(home) = std::env::var_os("HOME") {
            assert!(custom_path(Path::new(&home)).is_err());
        }
        let dir = tempfile::tempdir().unwrap();
        let safe = dir.path().join("project");
        std::fs::create_dir(&safe).unwrap();
        assert_eq!(custom_path(&safe).unwrap(), safe);
        let secret = safe.join(".ssh");
        std::fs::create_dir(&secret).unwrap();
        assert!(custom_path(&secret).is_err());
        let alias = dir.path().join("alias");
        symlink(&safe, &alias).unwrap();
        assert!(custom_path(&alias).is_err());
    }
}
