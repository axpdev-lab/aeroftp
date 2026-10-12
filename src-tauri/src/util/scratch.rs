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
    let dir = match dirs::cache_dir() {
        Some(cache) => cache
            .join(crate::portable::aeroftp_data_leaf_for_debug(cfg!(
                debug_assertions
            )))
            .join("scratch"),
        None => std::env::temp_dir().join("aeroftp-scratch"),
    };
    scratch_dir_with_room(&dir, bytes)
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
}
