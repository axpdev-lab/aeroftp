// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! The name of the temporary a file is written to before it takes its own.
//!
//! A temporary is the file's name plus a suffix (`.aerotmp`, `.aerosegtmp`,
//! ...) in the same folder: a resumed download finds its part again by that
//! name, and the cleanup and the file watchers know a temporary by its ending.
//! File systems limit one name to 255 bytes (ext4, btrfs, XFS, ZFS, APFS; NTFS
//! and exFAT count 255 UTF-16 units, never more than the UTF-8 bytes), so a
//! name near the limit leaves no room for the suffix, and its temporary could
//! not be created although the file itself can: a 244-byte name failed so,
//! with "File name too long", on every attempt of its download.
//!
//! Such a name is cut at a character boundary and followed by a hash of the
//! whole name. The temporary keeps its folder and its suffix, two names that
//! share the cut part still get two temporaries, and one name gets the same
//! temporary on every run, so a resume still finds its part.
//!
//! `aerorsync` keeps its own copy of [`fit_name`] (`streaming_writer`), since
//! it may not name the application's code; a test below holds the two to the
//! same names, because the delta writer and the download writers share
//! `<file>.aerotmp`.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// The longest name file systems take, in bytes.
pub const NAME_MAX_BYTES: usize = 255;

/// The room a name keeps for any suffix of a download's family:
/// `.aerosegtmp`, `.aerosrctmp` and `.aeroardtmp` are the longest, 11 bytes.
/// All the sidecars of one file then share one stem, and the record found
/// from a part's own name (`with_extension`) is the one
/// `part_record_path_for` names.
const SUFFIX_ROOM: usize = 11;

/// Hex digits of the hash a cut name ends with: 128 bits of SHA-256.
const HASH_HEX_DIGITS: usize = 32;

/// Whether `name_len` bytes of name leave room for `suffix`.
fn fits(name_len: usize, suffix: &str) -> bool {
    name_len + suffix.len().max(SUFFIX_ROOM) <= NAME_MAX_BYTES
}

/// `name` followed by `suffix`, within [`NAME_MAX_BYTES`]: when the two do not
/// fit, `name` is cut at a character boundary and followed by `~` and the
/// hash of the whole name.
pub fn fit_name(name: &OsStr, suffix: &str) -> OsString {
    let mut fitted = OsString::new();
    if fits(name.len(), suffix) {
        fitted.push(name);
    } else {
        // The cut part is only there to be read: a name that is not UTF-8
        // shows its replacement characters, and the hash, taken on the name
        // itself, still tells it from any other.
        let whole = name.to_string_lossy();
        let room = suffix.len().max(SUFFIX_ROOM);
        let mut cut = (NAME_MAX_BYTES - room - 1 - HASH_HEX_DIGITS).min(whole.len());
        while !whole.is_char_boundary(cut) {
            cut -= 1;
        }
        fitted.push(&whole[..cut]);
        fitted.push("~");
        let digest = Sha256::digest(name.as_encoded_bytes());
        for byte in &digest[..HASH_HEX_DIGITS / 2] {
            fitted.push(format!("{byte:02x}"));
        }
    }
    fitted.push(suffix);
    fitted
}

/// The temporary of `path`: its name with `suffix`, in its folder. A name with
/// room for the suffix gets exactly `<path><suffix>`.
pub fn sibling(path: &Path, suffix: &str) -> PathBuf {
    match path.file_name() {
        Some(name) if !fits(name.len(), suffix) => path.with_file_name(fit_name(name, suffix)),
        _ => {
            let mut temp = path.as_os_str().to_owned();
            temp.push(suffix);
            PathBuf::from(temp)
        }
    }
}

/// [`sibling`] for a remote path, whose folders `/` separates: the name of a
/// file on a server is limited the same way by the server's file system.
pub fn remote_sibling(path: &str, suffix: &str) -> String {
    let (folder, name) = match path.rsplit_once('/') {
        Some((folder, name)) => (Some(folder), name),
        None => (None, path),
    };
    if fits(name.len(), suffix) {
        return format!("{path}{suffix}");
    }
    let name = fit_name(OsStr::new(name), suffix)
        .to_string_lossy()
        .into_owned();
    match folder {
        Some(folder) => format!("{folder}/{name}"),
        None => name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The suffixes a download writes next to its file.
    const FAMILY: [&str; 4] = [".aerotmp", ".aeroardtmp", ".aerosegtmp", ".aerosrctmp"];

    /// The name the stress test failed on: 120 two-byte characters and
    /// `.txt`, 244 bytes, whose `.aerotmp` would be 252 and `.aerosegtmp` 255.
    fn stress_name() -> String {
        format!("{}.txt", "ä".repeat(120))
    }

    #[test]
    fn a_name_with_room_keeps_its_temporary() {
        for suffix in FAMILY {
            assert_eq!(
                sibling(Path::new("dir/f.bin"), suffix),
                PathBuf::from(format!("dir/f.bin{suffix}"))
            );
        }
        let name = "n".repeat(NAME_MAX_BYTES - SUFFIX_ROOM);
        assert_eq!(
            sibling(&Path::new("dir").join(&name), ".aerotmp"),
            Path::new("dir").join(format!("{name}.aerotmp"))
        );
        assert_eq!(remote_sibling("/a/f.bin", ".aerotmp"), "/a/f.bin.aerotmp");
        assert_eq!(remote_sibling("f.bin", ".aerotmp"), "f.bin.aerotmp");
    }

    #[test]
    fn the_temporary_of_a_long_name_can_be_created_and_is_the_same_every_time() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(stress_name());
        std::fs::write(&file, b"the file itself fits").unwrap();
        for suffix in FAMILY {
            let temp = sibling(&file, suffix);
            assert_eq!(temp.parent(), Some(dir.path()));
            let name = temp.file_name().unwrap().to_str().unwrap();
            assert!(
                name.len() <= NAME_MAX_BYTES,
                "{suffix}: {} bytes",
                name.len()
            );
            assert!(name.ends_with(suffix), "the cleanup knows it by its ending");
            assert!(
                name.starts_with("ää"),
                "the cut part still reads as the file"
            );
            std::fs::write(&temp, b"part").unwrap();
            assert_eq!(sibling(&file, suffix), temp, "a resume finds the same part");
        }
    }

    #[test]
    fn the_sidecars_of_one_file_share_one_stem() {
        for name in [
            "f.bin".to_string(),
            stress_name(),
            "n".repeat(NAME_MAX_BYTES - SUFFIX_ROOM + 1),
        ] {
            let file = Path::new("dir").join(&name);
            let part = sibling(&file, ".aerotmp");
            for suffix in FAMILY {
                let extension = suffix.trim_start_matches('.');
                assert_eq!(
                    part.with_extension(extension),
                    sibling(&file, suffix),
                    "{name}"
                );
            }
        }
    }

    #[test]
    fn two_long_names_that_share_the_cut_part_get_two_temporaries() {
        let shared = "x".repeat(250);
        let a = sibling(Path::new(&format!("{shared}.a")), ".aerotmp");
        let b = sibling(Path::new(&format!("{shared}.b")), ".aerotmp");
        assert_ne!(a, b);
    }

    #[test]
    fn a_cut_never_splits_a_character() {
        // Characters of one to four bytes, so the cut lands inside one of
        // each width somewhere in the run.
        for unit in ["é", "€", "😀", "a€😀é"] {
            let name = unit.repeat(NAME_MAX_BYTES / unit.len() + 1);
            for suffix in [
                ".aerotmp",
                ".aerotmp.4294967295.18446744073709551615.18446744073709551615",
            ] {
                let fitted = fit_name(OsStr::new(&name), suffix);
                let fitted = fitted
                    .to_str()
                    .expect("a cut on a character boundary is UTF-8");
                assert!(
                    fitted.len() <= NAME_MAX_BYTES,
                    "{unit} {suffix}: {}",
                    fitted.len()
                );
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_name_that_is_not_utf8_is_cut_and_still_told_apart() {
        use std::os::unix::ffi::OsStrExt;
        let mut a = vec![0xff; 250];
        a.push(b'a');
        let mut b = vec![0xff; 250];
        b.push(b'b');
        let a = fit_name(OsStr::from_bytes(&a), ".aerotmp");
        let b = fit_name(OsStr::from_bytes(&b), ".aerotmp");
        assert_ne!(a, b);
        assert!(a.len() <= NAME_MAX_BYTES);
    }

    #[test]
    fn a_long_remote_name_keeps_its_folder() {
        let name = stress_name();
        let suffix = ".aeroedit-00000000-0000-0000-0000-000000000000.tmp";
        let temp = remote_sibling(&format!("/docs/{name}"), suffix);
        let (folder, leaf) = temp.rsplit_once('/').unwrap();
        assert_eq!(folder, "/docs");
        assert!(leaf.len() <= NAME_MAX_BYTES && leaf.ends_with(suffix));
        assert_eq!(remote_sibling(&name, suffix), leaf);
    }

    /// The delta writer and the download writers share `<file>.aerotmp`, so
    /// `aerorsync`'s own copy must name every temporary as this one does.
    #[cfg(feature = "aerorsync")]
    #[test]
    fn aerorsync_names_temporaries_as_the_application_does() {
        let names = [
            "f.bin".to_string(),
            stress_name(),
            "n".repeat(NAME_MAX_BYTES - SUFFIX_ROOM),
            "n".repeat(NAME_MAX_BYTES - SUFFIX_ROOM + 1),
            "😀".repeat(64),
        ];
        for name in &names {
            for suffix in FAMILY
                .iter()
                .copied()
                .chain([".aerotmp.12345.6.1760000000000000000"])
            {
                let file = Path::new("dir").join(name);
                assert_eq!(
                    crate::aerorsync::streaming_writer::temp_sibling(&file, suffix),
                    sibling(&file, suffix),
                    "{name} {suffix}"
                );
            }
        }
    }
}
