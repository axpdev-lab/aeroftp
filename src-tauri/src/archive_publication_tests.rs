// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

#![cfg(any(unix, windows))]

use std::io::{Cursor, Write};
use std::path::Path;

fn directory_link(target: &Path, link: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).expect("create destination ancestor fixture");
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(target, link).expect(
        "Windows confinement regressions require Developer Mode or the symlink creation privilege",
    );
}

fn destination() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("out");
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("sentinel"), b"keep outside").unwrap();
    directory_link(&outside, &out.join("sub"));
    (tmp, out, outside)
}

fn tar_bytes() -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(7);
    header.set_mode(0o644);
    header.set_cksum();
    builder
        .append_data(&mut header, "sub/sentinel", &b"changed"[..])
        .unwrap();
    builder.into_inner().unwrap()
}

#[test]
fn tar_does_not_follow_an_existing_destination_ancestor_outside_the_root() {
    let (_tmp, out, outside) = destination();
    assert!(super::tar_unpack(Box::new(Cursor::new(tar_bytes())), &out).is_err());
    assert_eq!(
        std::fs::read(outside.join("sentinel")).unwrap(),
        b"keep outside"
    );
}

#[tokio::test]
async fn zip_does_not_follow_an_existing_destination_ancestor_outside_the_root() {
    let (tmp, out, outside) = destination();
    let path = tmp.path().join("sample.zip");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
    zip.start_file("sub/sentinel", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"changed").unwrap();
    zip.finish().unwrap();
    assert!(super::extract_archive(
        path.to_string_lossy().into_owned(),
        out.to_string_lossy().into_owned(),
        false,
        None
    )
    .await
    .is_err());
    assert_eq!(
        std::fs::read(outside.join("sentinel")).unwrap(),
        b"keep outside"
    );
}

#[tokio::test]
async fn sevenz_does_not_follow_an_existing_destination_ancestor_outside_the_root() {
    use sevenz_rust2::{ArchiveEntry, ArchiveWriter, SourceReader};
    let (tmp, out, outside) = destination();
    let mut writer = ArchiveWriter::new(Cursor::new(Vec::new())).unwrap();
    writer
        .push_archive_entries(
            vec![ArchiveEntry::new_file("sub/sentinel")],
            vec![SourceReader::new(&b"changed"[..])],
        )
        .unwrap();
    let path = tmp.path().join("sample.7z");
    std::fs::write(&path, writer.finish().unwrap().into_inner()).unwrap();
    assert!(super::extract_7z(
        path.to_string_lossy().into_owned(),
        out.to_string_lossy().into_owned(),
        None,
        false
    )
    .await
    .is_err());
    assert_eq!(
        std::fs::read(outside.join("sentinel")).unwrap(),
        b"keep outside"
    );
}

#[test]
fn rar_publication_does_not_follow_an_existing_destination_ancestor_outside_the_root() {
    let (tmp, out, outside) = destination();
    let staging = tmp.path().join("staging");
    std::fs::create_dir_all(staging.join("sub")).unwrap();
    std::fs::write(staging.join("sub/sentinel"), b"changed").unwrap();
    assert!(super::move_staged_tree(&staging, &out).is_err());
    assert_eq!(
        std::fs::read(outside.join("sentinel")).unwrap(),
        b"keep outside"
    );
    assert_eq!(
        std::fs::read(staging.join("sub/sentinel")).unwrap(),
        b"changed"
    );
}

#[test]
fn an_archive_named_subfolder_cannot_redefine_the_selected_root() {
    let (_tmp, out, outside) = destination();
    assert!(super::open_archive_output(&out, &out.join("sub")).is_err());
    assert_eq!(
        std::fs::read(outside.join("sentinel")).unwrap(),
        b"keep outside"
    );
}

#[test]
fn publication_keeps_the_opened_parent_when_its_old_name_is_replaced() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("out");
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(out.join("sub")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("sentinel"), b"keep outside").unwrap();
    let root = super::open_archive_output(&out, &out).unwrap();
    let (parent, name) =
        super::archive_destination_parent(&root, Path::new("sub/sentinel")).unwrap();
    // Unix allows replacement of an opened directory's name, while cap-std's
    // Windows directory handle excludes FILE_SHARE_DELETE and blocks it.
    // Both must retain the original authority through publication.
    #[cfg(unix)]
    {
        std::fs::rename(out.join("sub"), out.join("original")).unwrap();
        directory_link(&outside, &out.join("sub"));
    }
    #[cfg(windows)]
    assert!(std::fs::rename(out.join("sub"), out.join("original")).is_err());
    super::write_archive_entry_in_parent(&parent, &name, |file| {
        file.write_all(b"changed")?;
        Ok(7)
    })
    .unwrap();
    assert_eq!(
        std::fs::read(outside.join("sentinel")).unwrap(),
        b"keep outside"
    );
    #[cfg(unix)]
    assert_eq!(
        std::fs::read(out.join("original/sentinel")).unwrap(),
        b"changed"
    );
    #[cfg(windows)]
    assert_eq!(std::fs::read(out.join("sub/sentinel")).unwrap(), b"changed");
}

#[cfg(unix)]
#[test]
fn a_missing_tar_hardlink_source_keeps_the_existing_destination() {
    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("existing");
    std::fs::write(&destination, b"keep existing content").unwrap();
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Link);
    header.set_size(0);
    header.set_mode(0o644);
    builder
        .append_link(&mut header, "existing", "missing-source")
        .unwrap();
    let bytes = builder.into_inner().unwrap();
    assert!(super::tar_unpack(Box::new(Cursor::new(bytes)), directory.path()).is_err());
    assert_eq!(
        std::fs::read(&destination).unwrap(),
        b"keep existing content"
    );
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[cfg(unix)]
#[test]
fn reextracting_a_tar_hardlink_does_not_leave_an_extra_name() {
    use std::os::unix::fs::MetadataExt;
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    let destination = directory.path().join("existing");
    std::fs::write(&source, b"shared content").unwrap();
    std::fs::hard_link(&source, &destination).unwrap();
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Link);
    header.set_size(0);
    header.set_mode(0o644);
    builder
        .append_link(&mut header, "existing", "source")
        .unwrap();
    let bytes = builder.into_inner().unwrap();
    for _ in 0..2 {
        super::tar_unpack(Box::new(Cursor::new(bytes.clone())), directory.path()).unwrap();
        assert_eq!(
            std::fs::metadata(&source).unwrap().ino(),
            std::fs::metadata(&destination).unwrap().ino()
        );
        assert_eq!(std::fs::read(&destination).unwrap(), b"shared content");
        assert_eq!(std::fs::metadata(&source).unwrap().nlink(), 2);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 2);
    }
}

/// An entry that replaces a file narrower than a new file's default mode
/// arrives owner-only: written at the default mode, the new contents were
/// readable by other accounts until the file's own permissions were applied.
#[cfg(unix)]
#[test]
fn an_entry_replacing_a_private_file_stays_owner_only_while_it_is_written() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let existing = directory.path().join("private");
    std::fs::write(&existing, b"old").unwrap();
    std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o600)).unwrap();
    let root = super::open_archive_output(directory.path(), directory.path()).unwrap();
    let (parent, name) = super::archive_destination_parent(&root, Path::new("private")).unwrap();
    let mut during = None;
    super::write_archive_entry_in_parent(&parent, &name, |file| {
        during = Some(file.metadata()?.permissions().mode() & 0o777);
        file.write_all(b"new")?;
        Ok(3)
    })
    .unwrap();
    let during = during.unwrap();
    assert_eq!(during & 0o077, 0, "the temporary was {during:o}");
    assert_eq!(std::fs::read(&existing).unwrap(), b"new");
    let after = std::fs::metadata(&existing).unwrap().permissions().mode() & 0o777;
    assert_eq!(after, 0o600, "{after:o}");
}

/// A reference copied out of a RAR archive stays owner-only while its data
/// arrives and takes its source's mode after: applied first, a wider mode
/// opened the partial entry to other accounts.
#[cfg(unix)]
#[test]
fn a_copied_entry_takes_its_mode_after_its_data() {
    use std::os::unix::fs::PermissionsExt;
    struct ModeAtFirstRead {
        writer: std::fs::File,
        seen: Option<u32>,
    }
    impl std::io::Read for ModeAtFirstRead {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.seen.is_some() {
                return Ok(0);
            }
            self.seen = Some(self.writer.metadata()?.permissions().mode() & 0o777);
            buf[..4].copy_from_slice(b"data");
            Ok(4)
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("entry");
    // As a new entry's temporary is made: the default mode.
    let mut writer = std::fs::File::create(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let mut reader = ModeAtFirstRead {
        writer: writer.try_clone().unwrap(),
        seen: None,
    };
    let copied = super::copy_then_apply_permissions(
        &mut reader,
        &mut writer,
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert_eq!(copied, 4);
    let during = reader.seen.unwrap();
    assert_eq!(during & 0o077, 0, "the entry was {during:o} while written");
    let after = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(after, 0o755, "{after:o}");
}

/// An entry replacing a file other accounts can read takes that file's mode
/// only once it holds the name: given before the rename, a rename that
/// failed left the new contents readable until the temporary was removed.
#[cfg(unix)]
#[test]
fn an_entry_takes_the_mode_it_keeps_only_once_published() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let existing = directory.path().join("shared");
    std::fs::write(&existing, b"old").unwrap();
    std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o644)).unwrap();
    let root = super::open_archive_output(directory.path(), directory.path()).unwrap();
    let (parent, name) = super::archive_destination_parent(&root, Path::new("shared")).unwrap();
    let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let observed = seen.clone();
    super::local_copy_publication::OBSERVER.with(|observer| {
        *observer.borrow_mut() = Some(Box::new(move |stage, mode| {
            observed.borrow_mut().push((stage, mode));
        }));
    });
    let written = super::write_archive_entry_in_parent(&parent, &name, |file| {
        file.write_all(b"new")?;
        Ok(3)
    });
    super::local_copy_publication::OBSERVER.with(|observer| observer.borrow_mut().take());
    written.unwrap();
    let seen = seen.borrow().clone();
    assert_eq!(seen.len(), 1, "one publication observed: {seen:?}");
    let (_, staged) = seen[0];
    assert_eq!(
        staged & 0o077,
        0,
        "the entry was {staged:o} as it was renamed into place"
    );
    assert_eq!(std::fs::read(&existing).unwrap(), b"new");
    let after = std::fs::metadata(&existing).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        after, 0o644,
        "the kept mode after the publication: {after:o}"
    );
}
