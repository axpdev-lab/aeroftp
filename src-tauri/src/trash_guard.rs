//! Predicts, before calling `trash::delete`, whether moving a local path to
//! the trash would turn into a copy into the home trash.
//!
//! Since trash 5.2.9 the freedesktop backend falls back to the home trash
//! when a mount's own trash cannot be used because its root is not writable
//! (the common case for a partition or an external drive whose root belongs
//! to root). That fallback is a copy followed by a delete across devices:
//! trashing a 50 GB folder from such a drive copies 50 GB into the home
//! folder, with no progress and possibly no room for it. Before 5.2.9 the
//! same call failed with `PermissionDenied`.
//!
//! The prediction mirrors the crate's own selection in
//! `freedesktop.rs::delete_all_canonicalized` and
//! `execute_on_mounted_trash_folders` (trash 5.2.9): same mount table, same
//! "first mount point the path starts with" topdir, same `.Trash/$uid` then
//! `.Trash-$uid` order, and the same errors (only `PermissionDenied` falls
//! back). If trash is bumped, re-read those two functions against this file.

use std::path::{Path, PathBuf};

/// What `trash::delete` is going to do with a path, as far as the choice of
/// trash folder is concerned.
// Only Linux ever constructs a route; elsewhere `route_for` answers `None`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TrashRoute {
    /// Home trash on the same mount, or a usable per-volume trash: a rename.
    InPlace,
    /// The per-volume trash is unusable for lack of permission, so the crate
    /// would copy the item into the home trash. Carries the mount's topdir.
    HomeCopy { topdir: PathBuf },
    /// The per-volume trash would be picked but the item's own folder is not
    /// writable: the rename fails with `PermissionDenied`, the crate then
    /// copies into the home trash and cannot delete the original, which
    /// leaves an orphaned copy in the home trash. Refuse before trying.
    ParentNotWritable { parent: PathBuf },
}

/// The trash route for `path` on this machine. `None` when the prediction is
/// not possible (no mount table, no home): the caller then lets the crate
/// decide, which is what happened before this guard existed.
#[cfg(target_os = "linux")]
pub(crate) fn route_for(path: &Path) -> Option<TrashRoute> {
    let mounts = imp::read_sorted_mount_points()?;
    let home_trash = imp::canonicalize_or_parents(&imp::home_trash()?)?;
    let full = imp::canonicalize_like_trash(path)?;
    // SAFETY: getuid never fails and has no preconditions.
    let uid = unsafe { libc::getuid() };
    Some(imp::route(&full, &home_trash, &mounts, uid))
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn route_for(_path: &Path) -> Option<TrashRoute> {
    // macOS and Windows go through the OS trash APIs, which never copy into
    // another volume behind the caller's back.
    None
}

#[cfg(target_os = "linux")]
mod imp {
    use super::TrashRoute;
    use std::path::{Path, PathBuf};
    /// The decision itself, over an explicit mount table so tests can place a
    /// "mount point" on a temporary directory.
    pub(super) fn route(
        full: &Path,
        home_trash: &Path,
        mounts: &[PathBuf],
        uid: u32,
    ) -> TrashRoute {
        let topdir = topdir_of(full, mounts);
        if topdir == topdir_of(home_trash, mounts) {
            return TrashRoute::InPlace;
        }
        let parent_writable = || match full.parent() {
            Some(parent) => writable(parent),
            None => Writable::Yes,
        };

        // 1. `$topdir/.Trash/$uid`, only when `.Trash` is a real sticky directory
        //    and the per-user folder already exists (the crate never creates it).
        let admin_trash = topdir.join(".Trash");
        if is_valid_admin_trash(&admin_trash) {
            let users = admin_trash.join(uid.to_string());
            if users.is_dir() {
                return per_volume_outcome(topdir, full, writable(&users), parent_writable());
            }
        }

        // 2. `$topdir/.Trash-$uid`, created when missing.
        let own = topdir.join(format!(".Trash-{uid}"));
        if own.is_dir() {
            return per_volume_outcome(topdir, full, writable(&own), parent_writable());
        }
        if own.symlink_metadata().is_ok() {
            // Exists and is not a directory: `create_dir` fails with
            // AlreadyExists, which the crate reports rather than falling back.
            return TrashRoute::InPlace;
        }
        per_volume_outcome(topdir, full, writable(topdir), parent_writable())
    }

    fn per_volume_outcome(
        topdir: &Path,
        full: &Path,
        trash_dir: Writable,
        parent: Writable,
    ) -> TrashRoute {
        match trash_dir {
            Writable::Denied => TrashRoute::HomeCopy {
                topdir: topdir.to_path_buf(),
            },
            // Read-only filesystem or another error: the crate fails without
            // falling back, which is already an honest outcome.
            Writable::Other => TrashRoute::InPlace,
            Writable::Yes => match parent {
                Writable::Denied => TrashRoute::ParentNotWritable {
                    parent: full.parent().unwrap_or(full).to_path_buf(),
                },
                _ => TrashRoute::InPlace,
            },
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Writable {
        Yes,
        Denied,
        Other,
    }

    fn writable(path: &Path) -> Writable {
        use std::os::unix::ffi::OsStrExt;
        let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
            return Writable::Other;
        };
        // SAFETY: `c_path` is a valid NUL-terminated string for the whole call.
        if unsafe { libc::access(c_path.as_ptr(), libc::W_OK | libc::X_OK) } == 0 {
            return Writable::Yes;
        }
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::EACCES) | Some(libc::EPERM) => Writable::Denied,
            _ => Writable::Other,
        }
    }

    fn is_valid_admin_trash(path: &Path) -> bool {
        use std::os::unix::fs::PermissionsExt;
        const S_ISVTX: u32 = 0o1000;
        match path.symlink_metadata() {
            Ok(meta) => meta.is_dir() && meta.permissions().mode() & S_ISVTX != 0,
            Err(_) => false,
        }
    }

    /// The crate's `get_first_topdir_containing_path`: the mount table is sorted
    /// longest path first, and `/` is the answer when nothing matches.
    fn topdir_of<'a>(path: &Path, mounts: &'a [PathBuf]) -> &'a Path {
        mounts
            .iter()
            .map(PathBuf::as_path)
            .find(|mount| path.starts_with(mount))
            .unwrap_or_else(|| Path::new("/"))
    }

    pub(super) fn home_trash() -> Option<PathBuf> {
        if let Some(data_home) = std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
            return Some(PathBuf::from(data_home).join("Trash"));
        }
        let home = std::env::var_os("HOME").filter(|v| !v.is_empty())?;
        Some(PathBuf::from(home).join(".local/share/Trash"))
    }

    /// The crate's `canonicalize_paths`: canonical parent, file name kept, so a
    /// symlink is trashed as a link rather than followed.
    pub(super) fn canonicalize_like_trash(path: &Path) -> Option<PathBuf> {
        let absolute = if path.is_relative() {
            std::env::current_dir().ok()?.join(path)
        } else {
            path.to_path_buf()
        };
        let parent = absolute.parent()?.canonicalize().ok()?;
        Some(match absolute.file_name() {
            Some(name) => parent.join(name),
            None => parent,
        })
    }

    /// The crate's `canonicalize_path_or_parents`, for a home trash that may not
    /// exist yet.
    pub(super) fn canonicalize_or_parents(path: &Path) -> Option<PathBuf> {
        let mut current = path;
        let mut popped = Vec::new();
        loop {
            match current.canonicalize() {
                Ok(canonical) => {
                    return Some(popped.iter().rev().fold(canonical, |acc, c| acc.join(c)));
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    popped.push(current.file_name()?.to_owned());
                    current = current.parent()?;
                }
                Err(_) => return None,
            }
        }
    }

    /// `/proc/mounts` (falling back to `/etc/mtab`, as the crate does), mount
    /// directories only, longest first. The file escapes blanks and backslashes
    /// in octal (`\040`), which `getmntent` decodes and a split does not.
    pub(super) fn read_sorted_mount_points() -> Option<Vec<PathBuf>> {
        let table = std::fs::read_to_string("/proc/mounts")
            .or_else(|_| std::fs::read_to_string("/etc/mtab"))
            .ok()?;
        let mut mounts = parse_mount_table(&table);
        if mounts.is_empty() {
            return None;
        }
        mounts.sort_by_key(|m| std::cmp::Reverse(m.as_os_str().len()));
        Some(mounts)
    }

    fn parse_mount_table(table: &str) -> Vec<PathBuf> {
        table
            .lines()
            .filter_map(|line| line.split_whitespace().nth(1))
            .filter(|dir| !dir.is_empty())
            .map(|dir| PathBuf::from(unescape_octal(dir)))
            .collect()
    }

    fn unescape_octal(field: &str) -> String {
        let bytes = field.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'\\'
                && i + 3 < bytes.len()
                && bytes[i + 1..i + 4]
                    .iter()
                    .all(|b| (b'0'..=b'7').contains(b))
            {
                let value =
                    (bytes[i + 1] - b'0') * 64 + (bytes[i + 2] - b'0') * 8 + (bytes[i + 3] - b'0');
                out.push(value);
                i += 4;
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::os::unix::fs::PermissionsExt;

        fn running_as_root() -> bool {
            // SAFETY: geteuid never fails and has no preconditions.
            unsafe { libc::geteuid() == 0 }
        }

        /// A temporary tree with a "home" mount and a separate "drive" mount,
        /// declared through the explicit mount table the decision takes.
        struct Fixture {
            _root: tempfile::TempDir,
            home_trash: PathBuf,
            drive: PathBuf,
            mounts: Vec<PathBuf>,
        }

        fn fixture() -> Fixture {
            let root = tempfile::tempdir().unwrap();
            let base = root.path().canonicalize().unwrap();
            let home = base.join("home");
            let drive = base.join("drive");
            std::fs::create_dir_all(home.join(".local/share")).unwrap();
            std::fs::create_dir_all(&drive).unwrap();
            let mut mounts = vec![home.clone(), drive.clone()];
            mounts.sort_by_key(|m| std::cmp::Reverse(m.as_os_str().len()));
            Fixture {
                home_trash: home.join(".local/share/Trash"),
                drive,
                mounts,
                _root: root,
            }
        }

        fn set_mode(path: &Path, mode: u32) {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        }

        const UID: u32 = 4242;

        #[test]
        fn a_drive_whose_root_is_not_writable_would_copy_into_the_home_trash() {
            if running_as_root() {
                eprintln!("SKIPPED: running as root, a read-only mode does not deny access");
                return;
            }
            let fx = fixture();
            let folder = fx.drive.join("photos");
            std::fs::create_dir_all(&folder).unwrap();
            let item = folder.join("big.bin");
            std::fs::write(&item, b"x").unwrap();
            set_mode(&fx.drive, 0o555);
            let got = route(&item, &fx.home_trash, &fx.mounts, UID);
            set_mode(&fx.drive, 0o755);
            assert_eq!(
                got,
                TrashRoute::HomeCopy {
                    topdir: fx.drive.clone()
                }
            );
        }

        #[test]
        fn a_drive_whose_root_is_writable_keeps_its_own_trash() {
            let fx = fixture();
            let item = fx.drive.join("file.txt");
            std::fs::write(&item, b"x").unwrap();
            assert_eq!(
                route(&item, &fx.home_trash, &fx.mounts, UID),
                TrashRoute::InPlace
            );
        }

        #[test]
        fn an_existing_per_user_trash_is_used_even_under_a_read_only_root() {
            if running_as_root() {
                eprintln!("SKIPPED: running as root, a read-only mode does not deny access");
                return;
            }
            let fx = fixture();
            std::fs::create_dir(fx.drive.join(format!(".Trash-{UID}"))).unwrap();
            let item = fx.drive.join("file.txt");
            std::fs::write(&item, b"x").unwrap();
            set_mode(&fx.drive, 0o555);
            let got = route(&item, &fx.home_trash, &fx.mounts, UID);
            set_mode(&fx.drive, 0o755);
            // The item sits directly under the read-only root, so the rename out
            // of it is what fails: the guard must say so rather than "in place".
            assert_eq!(
                got,
                TrashRoute::ParentNotWritable {
                    parent: fx.drive.clone()
                }
            );
        }

        #[test]
        fn a_sticky_admin_trash_with_the_users_folder_is_used() {
            if running_as_root() {
                eprintln!("SKIPPED: running as root, a read-only mode does not deny access");
                return;
            }
            let fx = fixture();
            let admin = fx.drive.join(".Trash");
            std::fs::create_dir(&admin).unwrap();
            std::fs::create_dir(admin.join(UID.to_string())).unwrap();
            set_mode(&admin, 0o1777);
            let folder = fx.drive.join("docs");
            std::fs::create_dir(&folder).unwrap();
            let item = folder.join("a.txt");
            std::fs::write(&item, b"x").unwrap();
            set_mode(&fx.drive, 0o555);
            let got = route(&item, &fx.home_trash, &fx.mounts, UID);
            set_mode(&fx.drive, 0o755);
            assert_eq!(got, TrashRoute::InPlace);
        }

        #[test]
        fn an_admin_trash_without_the_sticky_bit_is_ignored() {
            if running_as_root() {
                eprintln!("SKIPPED: running as root, a read-only mode does not deny access");
                return;
            }
            let fx = fixture();
            let admin = fx.drive.join(".Trash");
            std::fs::create_dir(&admin).unwrap();
            std::fs::create_dir(admin.join(UID.to_string())).unwrap();
            set_mode(&admin, 0o777);
            let item = fx.drive.join("docs.txt");
            std::fs::write(&item, b"x").unwrap();
            set_mode(&fx.drive, 0o555);
            let got = route(&item, &fx.home_trash, &fx.mounts, UID);
            set_mode(&fx.drive, 0o755);
            assert_eq!(
                got,
                TrashRoute::HomeCopy {
                    topdir: fx.drive.clone()
                }
            );
        }

        #[test]
        fn an_item_on_the_home_mount_is_a_rename() {
            let fx = fixture();
            let home = fx
                .home_trash
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .parent()
                .unwrap();
            let item = home.join("notes.txt");
            std::fs::write(&item, b"x").unwrap();
            assert_eq!(
                route(&item, &fx.home_trash, &fx.mounts, UID),
                TrashRoute::InPlace
            );
        }

        #[test]
        fn mount_table_decodes_octal_escapes_in_the_mount_directory() {
            let table = "/dev/sdb1 /media/me/My\\040Drive vfat rw 0 0\nproc /proc proc rw 0 0\n";
            assert_eq!(
                parse_mount_table(table),
                vec![PathBuf::from("/media/me/My Drive"), PathBuf::from("/proc")]
            );
        }

        #[test]
        fn the_topdir_is_the_longest_mount_that_contains_the_path() {
            let mounts = vec![PathBuf::from("/media/me/drive"), PathBuf::from("/media")];
            assert_eq!(
                topdir_of(Path::new("/media/me/drive/x"), &mounts),
                Path::new("/media/me/drive")
            );
            assert_eq!(topdir_of(Path::new("/opt/x"), &mounts), Path::new("/"));
        }
    }
}
