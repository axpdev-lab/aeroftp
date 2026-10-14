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
    scratch_dir_with_room(&scratch_dir_path(), bytes)
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

/// How old a staging entry must be before the startup sweep removes it. A
/// younger file can still belong to a transfer running in another process
/// (the CLI and the GUI share the folder), so only a crash or kill artifact
/// is old enough to collect.
pub const STALE_STAGING_AGE: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// True when `name` is an AeroFTP staging shape the sweep may collect:
/// crypt-overlay ciphertext (`aeroftp_crypt_overlay_*.bin`, the upload name
/// and the `_dl_`/`_ver_` download variants under the same prefix),
/// remote-vault downloads (`aerovault_remote_*.aerovault`), Proton upload
/// folders (`aeroftp_proton_up_*`) and `tempfile` leftovers (`.tmp*`) from
/// the compress overlay, `rcat` and `hashsum`. Everything in the scratch
/// folder is app-created, but the gate keeps the sweep from touching a file
/// it cannot name.
pub fn is_staging_entry(name: &str) -> bool {
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
    /// Entries whose removal or metadata read failed.
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
        Err(_) => return stats,
    };
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !is_staging_entry(name) {
            continue;
        }
        // DirEntry::metadata does not follow symlinks: a link planted in the
        // folder is never traversed, and it matches no staging shape a real
        // transfer writes, so leave it.
        let meta = match entry.metadata() {
            Ok(meta) => meta,
            Err(_) => {
                stats.errors += 1;
                continue;
            }
        };
        let stale = meta
            .modified()
            .ok()
            .and_then(|mtime| now.duration_since(mtime).ok())
            .is_some_and(|age| age > older_than);
        if !stale {
            stats.kept_fresh += 1;
            continue;
        }
        let path = entry.path();
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
            }
            Err(_) => stats.errors += 1,
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
            "Scratch sweep: removed {} stale staging {} ({}), {} kept fresh, {} errors",
            stats.removed,
            if stats.removed == 1 {
                "entry"
            } else {
                "entries"
            },
            super::size::format_size(stats.bytes),
            stats.kept_fresh,
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

        // Whether the link itself is old enough to go is a clock-granularity
        // accident; what is pinned here is that collecting it never touches
        // the target outside the folder.
        let stats = sweep_stale_staging(root.path(), std::time::Duration::ZERO);

        assert_eq!(stats.errors, 0);
        assert!(target.exists(), "the target outside the folder survives");
    }

    #[test]
    fn a_missing_folder_has_nothing_to_sweep() {
        let root = tempfile::tempdir().unwrap();
        let stats = sweep_stale_staging(&root.path().join("absent"), STALE_STAGING_AGE);
        assert_eq!(stats, SweepStats::default());
    }
}
