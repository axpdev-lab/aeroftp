// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Where any transfer that has to land in a file before it is read or sent
//! goes: downloads and upload staging alike.

use std::path::{Path, PathBuf};

/// Room left on the disk beyond the transfer itself, so a scratch file does
/// not take the last bytes the rest of the system writes to.
pub const SCRATCH_HEADROOM: u64 = 256 * 1024 * 1024;

/// A folder on disk with room for a `bytes` transfer, created if missing.
///
/// Not the temporary folder: on many Linux systems that is a tmpfs sized from
/// RAM, and a 25.2 GB download for `hashsum` filled a 27 GB one after 21
/// minutes, with no hash at the end (#658 re-test, 2026-10-10). The cache
/// folder is on disk. The room is checked before the transfer starts, so a
/// file that cannot fit fails at once and says how much it needs.
pub fn staging_dir(bytes: u64) -> Result<PathBuf, String> {
    let dir = scratch_dir_with_room(&scratch_dir_path(), bytes)?;
    protect_scratch_process(&dir)
        .map_err(|e| format!("{} could not be protected from cleanup: {e}", dir.display()))?;
    Ok(dir)
}

/// Where [`staging_dir`] stages, resolved only: not created, no room check.
/// For callers that need to know where a staged file would be (the vault
/// confinement roots), or to attempt a metadata-only hardlink. A caller that
/// falls back to copying must check room before writing the payload.
pub fn scratch_dir_path() -> PathBuf {
    match dirs::cache_dir() {
        Some(cache) => cache
            .join(crate::portable::aeroftp_data_leaf_for_debug(cfg!(
                debug_assertions
            )))
            .join("scratch"),
        None => std::env::temp_dir().join("aeroftp-scratch"),
    }
}

/// [`staging_dir`] on a given folder.
pub fn scratch_dir_with_room(dir: &Path, bytes: u64) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir)
        .map_err(|e| format!("{} could not be created: {e}", dir.display()))?;
    let space = crate::filesystem::filesystem_space(dir)
        .map_err(|e| format!("the free space of {} could not be read: {e}", dir.display()))?;
    let needed = bytes.saturating_add(SCRATCH_HEADROOM);
    if space.available < needed {
        return Err(format!(
            "{} has {} free and a {} file needs {} there (with {} to spare)",
            dir.display(),
            super::size::format_size(space.available),
            super::size::format_size(bytes),
            super::size::format_size(needed),
            super::size::format_size(SCRATCH_HEADROOM),
        ));
    }
    Ok(dir.to_path_buf())
}

/// Minimum age for collecting staging leftovers. Age alone cannot establish
/// liveness: crypt uploads retain the source mtime and hold a staging lease.
pub const STALE_STAGING_AGE: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// A download's part sidecars: the resumable part, the read-ahead and
/// segmented temporaries, and the record of the version the part started from.
const DOWNLOAD_PART_SUFFIXES: [&str; 4] = [".aerotmp", ".aeroardtmp", ".aerosegtmp", ".aerosrctmp"];

fn open_process_lock(root: &Path) -> std::io::Result<std::fs::File> {
    let path = root.join("aeroftp_staging_live.lock");
    match std::fs::symlink_metadata(&path) {
        Ok(meta) if !meta.is_file() || meta.file_type().is_symlink() => {
            return Err(std::io::Error::other(
                "scratch process lock is not a regular file",
            ));
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}

/// Keep unleased staging (compress, CLI, remote vault, Proton) safe even when
/// it is older than the age gate. Shared handles live until process exit;
/// a sweep may collect unleased entries only while it owns the exclusive
/// process lock. Conservatively keep them for the whole staging process's
/// lifetime, including a vault handed back to the GUI between commands.
pub(crate) fn protect_scratch_process(root: &Path) -> std::io::Result<()> {
    use std::collections::{hash_map::Entry, HashMap};
    use std::sync::{LazyLock, Mutex};
    static LEASES: LazyLock<Mutex<HashMap<PathBuf, std::fs::File>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    let mut leases = LEASES
        .lock()
        .map_err(|_| std::io::Error::other("scratch process lease registry is poisoned"))?;
    if let Entry::Vacant(entry) = leases.entry(root.to_path_buf()) {
        let file = open_process_lock(root)?;
        // Startup sweeps are short and never wait for a staging process. A
        // new stager waits for an already-running sweep before making files.
        file.lock_shared()?;
        entry.insert(file);
    }
    Ok(())
}

fn staging_payload_name(name: &str) -> &str {
    DOWNLOAD_PART_SUFFIXES
        .iter()
        .find_map(|suffix| name.strip_suffix(suffix))
        .unwrap_or(name)
}

fn staging_lock_path(path: &Path) -> PathBuf {
    if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
        return path.with_file_name(format!("{}.lock", staging_payload_name(name)));
    }
    let mut name = path.as_os_str().to_owned();
    name.push(".lock");
    PathBuf::from(name)
}

/// Claim a unique staging path BEFORE creating its payload. The separate
/// sidecar avoids mandatory payload locks on Windows/SMB: providers may open,
/// write, rename and read the payload while this handle protects it from the
/// sweep. The OS releases the lock even when the process is killed.
pub(crate) struct StagingLease {
    path: PathBuf,
    lock_path: PathBuf,
    _lock: std::fs::File,
}

impl StagingLease {
    pub(crate) fn new(path: &Path) -> std::io::Result<Self> {
        let lock_path = staging_lock_path(path);
        let lock = std::fs::File::create_new(&lock_path)?;
        if let Err(e) = lock.try_lock() {
            let _ = std::fs::remove_file(&lock_path);
            return Err(e.into());
        }
        Ok(Self {
            path: path.to_path_buf(),
            lock_path,
            _lock: lock,
        })
    }
}

impl Drop for StagingLease {
    fn drop(&mut self) {
        // Also covers a cancelled future. Remove the payload before releasing
        // its lease; normal callers may already have removed it asynchronously.
        let _ = std::fs::remove_file(&self.path);
        for suffix in DOWNLOAD_PART_SUFFIXES {
            let mut part = self.path.as_os_str().to_owned();
            part.push(suffix);
            let _ = std::fs::remove_file(Path::new(&part));
        }
        let _ = std::fs::remove_file(&self.lock_path);
    }
}

/// True when `name` is an AeroFTP staging shape the sweep may collect:
/// crypt-overlay ciphertext (`aeroftp_crypt_overlay_*.bin`, the upload name
/// and the `_dl_`/`_ver_` download variants under the same prefix),
/// remote-vault downloads (`aerovault_remote_*.aerovault`), Proton upload
/// folders (`aeroftp_proton_up_*`) and `tempfile` leftovers (`.tmp*`) from
/// the compress overlay, `rcat` and `hashsum`. Everything in the scratch
/// folder is app-created, but the gate keeps the sweep from touching a file
/// it cannot name. Atomic-download partials of these paths use the same gate
/// and the same payload lease.
pub fn is_staging_entry(name: &str) -> bool {
    let name = staging_payload_name(name);
    (name.starts_with("aeroftp_crypt_overlay_") && name.ends_with(".bin"))
        || (name.starts_with("aerovault_remote_") && name.ends_with(".aerovault"))
        || name.starts_with("aeroftp_proton_up_")
        || name.starts_with(".tmp")
}

/// Outcome of one [`sweep_stale_staging`] pass.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SweepStats {
    /// Staging entries removed (a removed folder counts once).
    pub removed: u64,
    /// Bytes reclaimed, files only: a folder's contents are not summed.
    pub bytes: u64,
    /// Known staging shapes left in place because they are too young.
    pub kept_fresh: u64,
    /// Staging entries protected by a live process's lease.
    pub kept_active: u64,
    /// Directory/entry reads, lock probes, metadata reads or removals that failed.
    pub errors: u64,
}

/// Remove AeroFTP staging entries under `root` whose mtime is older than
/// `older_than`. Since #1199 the crypt and compress overlays, the remote
/// vault and Proton stage here instead of the temporary folder, which a
/// tmpfs cleared at reboot; nothing removed those leftovers, so a crash or a
/// failed upload left them until removed by hand. Entries that match no
/// known staging shape, symlinks, entries younger than the age gate and
/// entries whose metadata cannot be read are left alone. A missing folder
/// has nothing to sweep.
pub fn sweep_stale_staging(root: &Path, older_than: std::time::Duration) -> SweepStats {
    let mut stats = SweepStats::default();
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                stats.errors += 1;
            }
            return stats;
        }
    };
    sweep_staging_entries(root, entries, older_than)
}

fn sweep_staging_entries(
    root: &Path,
    entries: impl IntoIterator<Item = std::io::Result<std::fs::DirEntry>>,
    older_than: std::time::Duration,
) -> SweepStats {
    let mut stats = SweepStats::default();
    // The control file is permanent: unlinking it would let a new process
    // lock a different inode while another still holds the old one.
    let process_lock = match open_process_lock(root) {
        Ok(file) => match file.try_lock() {
            Ok(()) => Some(file),
            Err(std::fs::TryLockError::WouldBlock) => None,
            Err(std::fs::TryLockError::Error(_)) => {
                stats.errors += 1;
                None
            }
        },
        Err(_) => {
            stats.errors += 1;
            None
        }
    };
    let now = std::time::SystemTime::now();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                stats.errors += 1;
                continue;
            }
        };
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        // A killed process may leave both a payload and its sidecar, or only
        // the sidecar if it died before creating/after removing the payload.
        let is_lock = name.strip_suffix(".lock").is_some_and(is_staging_entry);
        if !is_staging_entry(name) && !is_lock {
            continue;
        }
        // DirEntry::metadata does not follow symlinks: a link planted in the
        // folder is never traversed, and it matches no staging shape a real
        // transfer writes, so leave it.
        let meta = match entry.metadata() {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                stats.errors += 1;
                continue;
            }
        };
        if meta.file_type().is_symlink() || !(meta.is_file() || meta.is_dir()) {
            continue;
        }
        let mtime = match meta.modified() {
            Ok(mtime) => mtime,
            Err(_) => {
                stats.errors += 1;
                continue;
            }
        };
        let stale = now
            .duration_since(mtime)
            .ok()
            .is_some_and(|age| age > older_than);
        if !stale {
            stats.kept_fresh += 1;
            continue;
        }
        let path = entry.path();
        let lock_path = if is_lock {
            path.clone()
        } else {
            staging_lock_path(&path)
        };
        // Missing sidecars are legacy/unleased entries. All other failures
        // keep the entry: inability to probe must never mean "not active".
        let held_lock = match std::fs::symlink_metadata(&lock_path) {
            Ok(meta) if !meta.is_file() || meta.file_type().is_symlink() => {
                stats.errors += 1;
                continue;
            }
            Ok(_) => {
                let lock = match std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&lock_path)
                {
                    Ok(lock) => lock,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(_) => {
                        stats.errors += 1;
                        continue;
                    }
                };
                match lock.try_lock() {
                    Ok(()) => Some(lock),
                    Err(std::fs::TryLockError::WouldBlock) => {
                        stats.kept_active += 1;
                        continue;
                    }
                    Err(std::fs::TryLockError::Error(_)) => {
                        stats.errors += 1;
                        continue;
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => {
                stats.errors += 1;
                continue;
            }
        };
        if held_lock.is_none() && process_lock.is_none() {
            stats.kept_active += 1;
            continue;
        }
        // Never remove an orphan sidecar before its payload: otherwise another
        // sweep could mistake a still-leased payload for a legacy entry.
        if is_lock {
            let payload = root.join(name.strip_suffix(".lock").unwrap());
            match std::fs::symlink_metadata(payload) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Ok(_) => continue,
                Err(_) => {
                    stats.errors += 1;
                    continue;
                }
            }
        }
        let gone = if meta.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        match gone {
            Ok(()) => {
                stats.removed += 1;
                if meta.is_file() {
                    stats.bytes += meta.len();
                }
                if !is_lock && held_lock.is_some() {
                    if let Err(e) = std::fs::remove_file(&lock_path) {
                        if e.kind() != std::io::ErrorKind::NotFound {
                            stats.errors += 1;
                        }
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => stats.errors += 1,
        }
    }
    // Unlock explicitly so a concurrent fork cannot retain the sweep's
    // exclusive lock after this pass has finished.
    if let Some(file) = process_lock {
        if file.unlock().is_err() {
            stats.errors += 1;
        }
    }
    stats
}

/// Sweep the process's own scratch folder with [`STALE_STAGING_AGE`], once
/// per startup. Never fails the caller; a folder that cannot be read simply
/// reports through the log.
pub fn sweep_staging_on_startup() {
    let stats = sweep_stale_staging(&scratch_dir_path(), STALE_STAGING_AGE);
    if stats.removed > 0 || stats.errors > 0 {
        log::info!(
            "Scratch sweep: removed {} stale staging {} ({}), {} kept fresh, {} kept active, {} errors",
            stats.removed,
            if stats.removed == 1 {
                "entry"
            } else {
                "entries"
            },
            super::size::format_size(stats.bytes),
            stats.kept_fresh,
            stats.kept_active,
            stats.errors,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_with_room_is_created_and_returned() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("nested").join("scratch");
        assert_eq!(scratch_dir_with_room(&dir, 1).unwrap(), dir);
        assert!(dir.is_dir());
    }

    #[test]
    fn a_file_larger_than_the_disk_is_refused_before_it_starts() {
        let root = tempfile::tempdir().unwrap();
        let why = scratch_dir_with_room(root.path(), u64::MAX / 2).unwrap_err();
        assert!(why.contains("free and a"), "{why}");
        assert!(why.contains(&root.path().display().to_string()), "{why}");
    }

    fn backdate(path: &Path, age: std::time::Duration) {
        let mtime = filetime::FileTime::from_system_time(std::time::SystemTime::now() - age);
        filetime::set_file_mtime(path, mtime).unwrap();
    }

    #[test]
    fn the_name_gate_covers_every_shape_the_app_stages() {
        for name in [
            "aeroftp_crypt_overlay_1760000000000_550e8400.bin",
            "aeroftp_crypt_overlay_dl_1760000000000_550e8400.bin",
            "aeroftp_crypt_overlay_ver_1760000000000_550e8400.bin",
            "aerovault_remote_550e8400.aerovault",
            "aeroftp_crypt_overlay_dl_1760000000000_550e8400.bin.aerotmp",
            "aeroftp_crypt_overlay_dl_1760000000000_550e8400.bin.aeroardtmp",
            "aerovault_remote_550e8400.aerovault.aerosegtmp",
            "aeroftp_proton_up_550e8400",
            ".tmpA1b2C3",
        ] {
            assert!(is_staging_entry(name), "{name}");
        }
        for name in [
            "notes.txt",
            "aeroftp_crypt_overlay_1760000000000_550e8400.bin.bak",
            "aerovault_remote_550e8400.aerovault.tmp",
            "aeroftp_proton_upload_550e8400",
            "tmpA1b2C3",
            "aeroftp-scratch",
            "notes.txt.aerotmp",
            "aeroftp_crypt_overlay_dl_1760000000000_550e8400.bin.aerotmp.bak",
        ] {
            assert!(!is_staging_entry(name), "{name}");
        }
    }

    #[test]
    fn the_sweep_removes_only_stale_known_shapes() {
        let root = tempfile::tempdir().unwrap();
        let old_crypt = root.path().join("aeroftp_crypt_overlay_1_abc.bin");
        let fresh_crypt = root.path().join("aeroftp_crypt_overlay_2_def.bin");
        let old_vault = root.path().join("aerovault_remote_abc.aerovault");
        let old_tmp = root.path().join(".tmpA1b2C3");
        let old_proton = root.path().join("aeroftp_proton_up_abc");
        let unrelated = root.path().join("notes.txt");
        std::fs::write(&old_crypt, b"ciphertext").unwrap();
        std::fs::write(&fresh_crypt, b"ciphertext").unwrap();
        std::fs::write(&old_vault, b"vault").unwrap();
        std::fs::write(&old_tmp, b"partial").unwrap();
        std::fs::create_dir(&old_proton).unwrap();
        std::fs::write(old_proton.join("payload.bin"), b"proton").unwrap();
        std::fs::write(&unrelated, b"mine").unwrap();
        for path in [&old_crypt, &old_vault, &old_tmp, &unrelated] {
            backdate(path, STALE_STAGING_AGE + std::time::Duration::from_secs(60));
        }
        backdate(
            &old_proton,
            STALE_STAGING_AGE + std::time::Duration::from_secs(60),
        );

        let stats = sweep_stale_staging(root.path(), STALE_STAGING_AGE);

        assert_eq!(stats.errors, 0);
        assert_eq!(stats.removed, 4);
        assert_eq!(stats.kept_fresh, 1);
        assert_eq!(stats.bytes, 10 + 5 + 7);
        assert!(!old_crypt.exists());
        assert!(fresh_crypt.exists());
        assert!(!old_vault.exists());
        assert!(!old_tmp.exists());
        assert!(!old_proton.exists());
        assert!(unrelated.exists(), "an unknown shape is never collected");
    }

    #[cfg(unix)]
    #[test]
    fn the_sweep_never_follows_a_symlink() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("keep.bin");
        std::fs::write(&target, b"keep").unwrap();
        let link = root.path().join("aeroftp_crypt_overlay_1_link.bin");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let old = filetime::FileTime::from_system_time(
            std::time::SystemTime::now() - STALE_STAGING_AGE - std::time::Duration::from_secs(60),
        );
        filetime::set_symlink_file_times(&link, old, old).unwrap();
        let dir_link = root.path().join("aeroftp_proton_up_link");
        std::os::unix::fs::symlink(outside.path(), &dir_link).unwrap();
        filetime::set_symlink_file_times(&dir_link, old, old).unwrap();
        let stats = sweep_stale_staging(root.path(), STALE_STAGING_AGE);

        assert_eq!(stats.errors, 0);
        assert_eq!(stats.removed, 0);
        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(std::fs::symlink_metadata(&dir_link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(target.exists(), "the target outside the folder survives");
    }

    #[test]
    fn a_directory_read_failure_is_reported() {
        let root = tempfile::NamedTempFile::new().unwrap();
        let stats = sweep_stale_staging(root.path(), STALE_STAGING_AGE);
        assert_eq!(
            stats.errors, 1,
            "a non-directory is not a missing directory"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_permission_failure_is_reported() {
        use std::os::unix::fs::PermissionsExt;
        // SAFETY: geteuid takes no pointers and has no preconditions.
        if unsafe { libc::geteuid() } == 0 {
            return; // root bypasses directory permissions
        }
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o0)).unwrap();
        let stats = sweep_stale_staging(root.path(), STALE_STAGING_AGE);
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(stats.errors, 1);
    }

    #[test]
    fn an_iterator_error_is_reported_and_does_not_stop_the_sweep() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(".tmpOld");
        std::fs::write(&path, b"old").unwrap();
        backdate(
            &path,
            STALE_STAGING_AGE + std::time::Duration::from_secs(60),
        );
        let entry = std::fs::read_dir(root.path()).unwrap().next().unwrap();
        let stats = sweep_staging_entries(
            root.path(),
            [
                Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
                entry,
            ],
            STALE_STAGING_AGE,
        );
        assert_eq!(stats.errors, 1);
        assert_eq!(stats.removed, 1);
        assert!(!path.exists());
    }

    #[test]
    fn a_lease_keeps_old_ciphertext_and_cleans_up_when_dropped() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("aeroftp_crypt_overlay_1_live.bin");
        let lease = StagingLease::new(&path).unwrap();
        std::fs::write(&path, b"ciphertext").unwrap();
        backdate(
            &path,
            STALE_STAGING_AGE + std::time::Duration::from_secs(60),
        );
        let stats = sweep_stale_staging(root.path(), STALE_STAGING_AGE);
        assert_eq!(stats.errors, 0);
        assert_eq!(stats.removed, 0);
        assert_eq!(stats.kept_active, 1);
        assert_eq!(std::fs::read(&path).unwrap(), b"ciphertext");
        drop(lease);
        assert!(!path.exists());
        assert!(!staging_lock_path(&path).exists());
    }

    #[test]
    fn download_partials_share_the_payload_lease_and_are_collected_after_a_crash() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("aeroftp_crypt_overlay_dl_1_live.bin");
        let lease = StagingLease::new(&path).unwrap();
        let parts: Vec<_> = DOWNLOAD_PART_SUFFIXES
            .iter()
            .map(|suffix| {
                root.path()
                    .join(format!("aeroftp_crypt_overlay_dl_1_live.bin{suffix}"))
            })
            .collect();
        for part in &parts {
            std::fs::write(part, b"partial").unwrap();
            backdate(part, STALE_STAGING_AGE + std::time::Duration::from_secs(60));
        }
        let stats = sweep_stale_staging(root.path(), STALE_STAGING_AGE);
        assert_eq!(stats.errors, 0);
        assert_eq!(stats.removed, 0);
        assert_eq!(stats.kept_active, parts.len() as u64);
        assert!(parts.iter().all(|part| part.exists()));
        drop(lease);
        assert!(parts.iter().all(|part| !part.exists()));

        // A killed process can leave only the partial, with no final payload.
        for part in &parts {
            std::fs::write(part, b"partial").unwrap();
            backdate(part, STALE_STAGING_AGE + std::time::Duration::from_secs(60));
        }
        let stats = sweep_stale_staging(root.path(), STALE_STAGING_AGE);
        assert_eq!(stats.errors, 0);
        assert_eq!(stats.removed, parts.len() as u64);
        assert!(parts.iter().all(|part| !part.exists()));
    }

    #[test]
    fn an_unreadable_lease_keeps_the_payload_and_reports_an_error() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("aeroftp_crypt_overlay_1_unknown.bin");
        std::fs::write(&path, b"keep").unwrap();
        backdate(
            &path,
            STALE_STAGING_AGE + std::time::Duration::from_secs(60),
        );
        std::fs::create_dir(staging_lock_path(&path)).unwrap();
        let stats = sweep_stale_staging(root.path(), STALE_STAGING_AGE);
        assert_eq!(stats.errors, 1);
        assert_eq!(stats.removed, 0);
        assert!(path.exists());
    }

    fn sweep_child(root: &Path, mode: &str) {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "util::scratch::tests::staging_lease_child",
                "--nocapture",
            ])
            .env("AEROFTP_SCRATCH_TEST_ROOT", root)
            .env("AEROFTP_SCRATCH_TEST_MODE", mode)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn staging_lease_child() {
        let Some(root) = std::env::var_os("AEROFTP_SCRATCH_TEST_ROOT") else {
            return;
        };
        let root = PathBuf::from(root);
        let mode = std::env::var("AEROFTP_SCRATCH_TEST_MODE").unwrap();
        let path = root.join(if mode.starts_with("unleased") {
            ".tmpOld"
        } else {
            "aeroftp_crypt_overlay_1_live.bin"
        });
        if mode == "unleased-crash" {
            protect_scratch_process(&root).unwrap();
            std::fs::write(&path, b"partial").unwrap();
            backdate(
                &path,
                STALE_STAGING_AGE + std::time::Duration::from_secs(60),
            );
            std::process::exit(0);
        }
        if mode == "crash" {
            let _lease = StagingLease::new(&path).unwrap();
            std::fs::write(&path, b"ciphertext").unwrap();
            backdate(
                &path,
                STALE_STAGING_AGE + std::time::Duration::from_secs(60),
            );
            // Process exit deliberately skips Drop, like a killed transfer.
            std::process::exit(0);
        }
        let stats = sweep_stale_staging(&root, STALE_STAGING_AGE);
        assert_eq!(stats.errors, 0);
        assert_eq!(stats.removed, 0);
        assert_eq!(stats.kept_active, 1);
        assert!(path.exists());
    }

    #[test]
    fn unleased_staging_survives_a_live_process_even_beyond_the_age_gate() {
        let root = tempfile::tempdir().unwrap();
        let live = open_process_lock(root.path()).unwrap();
        live.lock_shared().unwrap();
        let path = root.path().join(".tmpOld");
        std::fs::write(&path, b"partial").unwrap();
        backdate(
            &path,
            STALE_STAGING_AGE + std::time::Duration::from_secs(60),
        );
        sweep_child(root.path(), "unleased-sweep");
        live.unlock().unwrap();
        drop(live);
        let stats = sweep_stale_staging(root.path(), STALE_STAGING_AGE);
        assert_eq!(stats.errors, 0);
        assert_eq!(stats.removed, 1);
        assert!(!path.exists());

        // Registration in the actual staging API also releases on process exit.
        sweep_child(root.path(), "unleased-crash");
        let stats = sweep_stale_staging(root.path(), STALE_STAGING_AGE);
        assert_eq!(stats.errors, 0);
        assert_eq!(stats.removed, 1);
        assert!(!path.exists());
    }

    #[test]
    fn another_process_keeps_live_staging_but_collects_a_crashed_owner() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("aeroftp_crypt_overlay_1_live.bin");
        let lease = StagingLease::new(&path).unwrap();
        std::fs::write(&path, b"ciphertext").unwrap();
        backdate(
            &path,
            STALE_STAGING_AGE + std::time::Duration::from_secs(60),
        );
        sweep_child(root.path(), "sweep");
        drop(lease);

        sweep_child(root.path(), "crash");
        assert!(path.exists());
        assert!(staging_lock_path(&path).exists());
        let stats = sweep_stale_staging(root.path(), STALE_STAGING_AGE);
        assert_eq!(stats.errors, 0);
        assert_eq!(stats.removed, 1);
        assert!(!path.exists());
        assert!(!staging_lock_path(&path).exists());
    }

    #[test]
    fn stale_orphan_sidecars_are_collected_but_unknown_locks_survive() {
        let root = tempfile::tempdir().unwrap();
        let orphan = root.path().join("aeroftp_crypt_overlay_1_orphan.bin.lock");
        let unknown = root.path().join("notes.txt.lock");
        for path in [&orphan, &unknown] {
            std::fs::write(path, b"").unwrap();
            backdate(path, STALE_STAGING_AGE + std::time::Duration::from_secs(60));
        }
        let stats = sweep_stale_staging(root.path(), STALE_STAGING_AGE);
        assert_eq!(stats.errors, 0);
        assert_eq!(stats.removed, 1);
        assert!(!orphan.exists());
        assert!(unknown.exists());
    }

    #[test]
    fn a_missing_folder_has_nothing_to_sweep() {
        let root = tempfile::tempdir().unwrap();
        let stats = sweep_stale_staging(&root.path().join("absent"), STALE_STAGING_AGE);
        assert_eq!(stats, SweepStats::default());
    }
}
