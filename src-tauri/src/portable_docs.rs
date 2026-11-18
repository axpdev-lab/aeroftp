// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! The documents a portable update installs beside the exes: the marker,
//! README, LICENSE and third-party notices.
//!
//! The old app stages them, before it hands over to the update helper, in a
//! folder beside the exes (`.aeroftp-pending-docs`), with the SHA-256 of the
//! new app exe they belong to. The folder is written under a temporary name
//! and renamed into place, so it is whole or absent. The new app installs
//! them at its start: each one is renamed over the installed document, an
//! operation on that exact path, which Windows refuses while another process
//! holds the file and which never writes into a directory with the
//! document's name. A document still held stays pending: the held ones share
//! one retry budget, and what is left is installed at a later start; the
//! About dialog opens the pending notices meanwhile. A folder that belongs to
//! another exe (an update whose swap failed, a folder replaced by hand) is
//! removed. Every outcome is logged as what it is.

use std::io;
use std::path::Path;
use std::time::Duration;

/// The folder of the documents waiting to be installed, beside the exes.
pub(crate) const PENDING_DIR: &str = ".aeroftp-pending-docs";
/// The key of the exe the pending documents belong to.
const KEY_FILE: &str = "exe.key";
/// The name a staging folder has until it is whole.
const TMP_PREFIX: &str = ".aeroftp-pending-docs.tmp-";
/// The documents, in the order they are installed.
pub(crate) const DOCS: [&str; 4] = [
    "portable.marker",
    "README.txt",
    "LICENSE.txt",
    "THIRD-PARTY-NOTICES.md",
];

/// Windows error codes of a rename refused because another process holds
/// the target (`ERROR_SHARING_VIOLATION`, `ERROR_LOCK_VIOLATION`) or because
/// access is denied (`ERROR_ACCESS_DENIED`: a handle without delete sharing,
/// a read-only file, an ACL). The module runs on Windows only.
const ERROR_ACCESS_DENIED: i32 = 5;
const ERROR_SHARING_VIOLATION: i32 = 32;
const ERROR_LOCK_VIOLATION: i32 = 33;

/// What one attempt did with a pending document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// The new document replaced the installed one (or created it).
    Installed,
    /// A directory has the document's name: nothing was written into it,
    /// and the pending copy is dropped.
    Directory,
    /// Another process holds the installed document: it is kept, and the
    /// new one stays pending.
    InUse,
    /// Access to the installed document was denied: it is kept, and the new
    /// one stays pending.
    AccessDenied,
    /// The replace failed for another reason, named here: the installed
    /// document is kept and the pending copy is dropped.
    Failed(String),
}

impl Outcome {
    /// Whether the document is still pending after this attempt.
    pub(crate) fn waits(&self) -> bool {
        matches!(self, Outcome::InUse | Outcome::AccessDenied)
    }
}

/// What an install pass found.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Pending {
    /// No pending folder.
    None,
    /// The folder belongs to another exe, has no key, or is no folder: removed.
    Stale,
    /// The outcome of each pending document, in install order.
    Docs(Vec<(&'static str, Outcome)>),
}

/// The key of an exe: the hex SHA-256 of its bytes.
pub(crate) fn exe_key(exe: &Path) -> io::Result<String> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(exe)?;
    let mut hasher = Sha256::new();
    io::copy(&mut file, &mut hasher)?;
    Ok(hex::encode(hasher.finalize()))
}

/// Removes staging folders a stopped update left (`TMP_PREFIX*`).
fn remove_staging_leftovers(exe_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(exe_dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with(TMP_PREFIX) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Removes whatever is at `path`: a folder with its content, or a file.
fn remove_any(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Stages `docs` (source, document name) for the exe at `new_exe`, beside the
/// exes in `exe_dir`. The folder is written under a temporary name
/// (`TMP_PREFIX` + `suffix`) and renamed into place, replacing an older
/// pending folder; any failure removes the temporary folder and leaves the
/// older one as it was.
pub(crate) fn stage_pending_docs(
    exe_dir: &Path,
    new_exe: &Path,
    docs: &[(&Path, &'static str)],
    suffix: &str,
) -> Result<(), String> {
    remove_staging_leftovers(exe_dir);
    if docs.is_empty() {
        return Ok(());
    }
    let tmp = exe_dir.join(format!("{TMP_PREFIX}{suffix}"));
    let staged = (|| -> io::Result<()> {
        // A folder this call creates: nothing else is written into.
        std::fs::create_dir(&tmp)?;
        for (source, name) in docs {
            std::fs::copy(source, tmp.join(name))?;
        }
        std::fs::write(tmp.join(KEY_FILE), exe_key(new_exe)?)?;
        let pending = exe_dir.join(PENDING_DIR);
        remove_any(&pending)?;
        std::fs::rename(&tmp, &pending)
    })();
    staged.map_err(|e| {
        let _ = std::fs::remove_dir_all(&tmp);
        format!("Failed to stage the portable documents: {e}")
    })
}

/// Removes the pending folder of `exe_dir` and any staging leftover: the
/// update did not go through.
pub(crate) fn discard_pending_docs(exe_dir: &Path) {
    remove_staging_leftovers(exe_dir);
    let _ = remove_any(&exe_dir.join(PENDING_DIR));
}

/// The outcome of a failed replace of `target`.
fn classify(target: &Path, e: &io::Error) -> Outcome {
    if target.is_dir() {
        return Outcome::Directory;
    }
    match e.raw_os_error() {
        Some(ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION) => Outcome::InUse,
        Some(ERROR_ACCESS_DENIED) => Outcome::AccessDenied,
        _ => Outcome::Failed(e.to_string()),
    }
}

/// One install pass over the pending folder of `exe_dir` for the exe whose
/// key is `own_key`. `replace` moves a pending document over the installed
/// one (`std::fs::rename` in production).
pub(crate) fn install_pending_docs(
    exe_dir: &Path,
    own_key: &str,
    replace: impl Fn(&Path, &Path) -> io::Result<()>,
) -> Pending {
    remove_staging_leftovers(exe_dir);
    let pending = exe_dir.join(PENDING_DIR);
    match std::fs::symlink_metadata(&pending) {
        Err(_) => return Pending::None,
        Ok(meta) if !meta.is_dir() => {
            let _ = remove_any(&pending);
            return Pending::Stale;
        }
        Ok(_) => {}
    }
    let key = std::fs::read_to_string(pending.join(KEY_FILE)).ok();
    if key.as_deref().map(str::trim) != Some(own_key) {
        let _ = std::fs::remove_dir_all(&pending);
        return Pending::Stale;
    }
    let mut outcomes = Vec::new();
    for name in DOCS {
        let source = pending.join(name);
        if !source.is_file() {
            continue;
        }
        let target = exe_dir.join(name);
        // A directory found first is never touched; one that appears later
        // makes the rename fail, without writing into it.
        let outcome = if target.is_dir() {
            Outcome::Directory
        } else {
            match replace(&source, &target) {
                Ok(()) => Outcome::Installed,
                // Another pass (a second start of the app) moved it first:
                // it is not this pass's to report.
                Err(_) if !source.exists() => continue,
                Err(e) => classify(&target, &e),
            }
        };
        if matches!(outcome, Outcome::Directory | Outcome::Failed(_)) {
            let _ = std::fs::remove_file(&source);
        }
        outcomes.push((name, outcome));
    }
    if !DOCS.iter().any(|name| pending.join(name).is_file()) {
        let _ = std::fs::remove_dir_all(&pending);
    }
    Pending::Docs(outcomes)
}

/// Installs the pending documents: one attempt each, then the held ones
/// share `rounds` more passes, `pause` apart (`sleep`), however many of them
/// are held. Returns the last outcome of each document.
pub(crate) fn install_with_shared_budget(
    exe_dir: &Path,
    own_key: &str,
    rounds: u32,
    pause: Duration,
    replace: impl Fn(&Path, &Path) -> io::Result<()>,
    sleep: impl Fn(Duration),
) -> Pending {
    let mut last = match install_pending_docs(exe_dir, own_key, &replace) {
        Pending::Docs(outcomes) => outcomes,
        other => return other,
    };
    let pending = exe_dir.join(PENDING_DIR);
    let mut left = rounds;
    while left > 0 && last.iter().any(|(_, outcome)| outcome.waits()) {
        left -= 1;
        sleep(pause);
        let retried = install_pending_docs(exe_dir, own_key, &replace);
        if let Pending::Docs(retried) = &retried {
            for (name, outcome) in retried {
                if let Some(entry) = last.iter_mut().find(|(n, _)| n == name) {
                    entry.1 = outcome.clone();
                }
            }
        }
        // A waiting document that is no longer pending was handled by
        // another pass: this one has nothing true to say about it.
        last.retain(|(name, outcome)| !outcome.waits() || pending.join(name).is_file());
        if !matches!(retried, Pending::Docs(_)) {
            break;
        }
    }
    Pending::Docs(last)
}

/// The log line of an outcome, as it happened.
pub(crate) fn outcome_line(name: &str, outcome: &Outcome) -> String {
    match outcome {
        Outcome::Installed => format!("portable update: {name} installed"),
        Outcome::Directory => {
            format!("portable update: {name} is a directory, nothing was written into it")
        }
        Outcome::InUse => format!(
            "portable update: {name} is in use, kept the installed one; the new one is installed at a later start"
        ),
        Outcome::AccessDenied => format!(
            "portable update: access to {name} was denied, kept the installed one; the new one is tried again at a later start"
        ),
        Outcome::Failed(e) => {
            format!("portable update: {name} could not be replaced ({e}), kept the installed one")
        }
    }
}

/// The pending notices beside `exe_dir`, which the About dialog opens first.
#[cfg(test)]
pub(crate) fn pending_notices(exe_dir: &Path) -> std::path::PathBuf {
    exe_dir.join(PENDING_DIR).join("THIRD-PARTY-NOTICES.md")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    const KEY: &str = "new-exe-key";

    /// An app folder with installed documents and a pending folder for the
    /// exe whose key is `key` (none: no key file).
    fn folder(key: Option<&str>) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp dir");
        let pending = dir.path().join(PENDING_DIR);
        std::fs::create_dir(&pending).expect("pending dir");
        for name in DOCS {
            std::fs::write(dir.path().join(name), format!("old {name}")).expect("installed");
            std::fs::write(pending.join(name), format!("new {name}")).expect("pending");
        }
        if let Some(key) = key {
            std::fs::write(pending.join(KEY_FILE), format!("{key}\r\n")).expect("key");
        }
        dir
    }

    fn read(dir: &Path, name: &str) -> String {
        std::fs::read_to_string(dir.join(name)).expect("document")
    }

    fn rename(from: &Path, to: &Path) -> io::Result<()> {
        std::fs::rename(from, to)
    }

    /// A replace that Windows refuses with `code` for the documents in `held`.
    fn held<'a>(
        held: &'a RefCell<Vec<&'static str>>,
        code: i32,
    ) -> impl Fn(&Path, &Path) -> io::Result<()> + 'a {
        move |from: &Path, to: &Path| {
            let name = to.file_name().unwrap().to_string_lossy().to_string();
            if held.borrow().iter().any(|h| *h == name) {
                Err(io::Error::from_raw_os_error(code))
            } else {
                rename(from, to)
            }
        }
    }

    #[test]
    fn every_pending_document_is_installed_and_the_folder_goes() {
        let dir = folder(Some(KEY));
        let outcome = install_pending_docs(dir.path(), KEY, rename);
        assert_eq!(
            outcome,
            Pending::Docs(DOCS.iter().map(|n| (*n, Outcome::Installed)).collect())
        );
        for name in DOCS {
            assert_eq!(read(dir.path(), name), format!("new {name}"));
        }
        assert!(!dir.path().join(PENDING_DIR).exists());
        assert_eq!(install_pending_docs(dir.path(), KEY, rename), Pending::None);
    }

    #[test]
    fn a_held_document_waits_until_it_is_released_and_the_others_install() {
        let dir = folder(Some(KEY));
        let busy = RefCell::new(vec!["THIRD-PARTY-NOTICES.md"]);
        let Pending::Docs(first) =
            install_pending_docs(dir.path(), KEY, held(&busy, ERROR_SHARING_VIOLATION))
        else {
            panic!("docs");
        };
        assert_eq!(first[3], ("THIRD-PARTY-NOTICES.md", Outcome::InUse));
        assert_eq!(
            read(dir.path(), "THIRD-PARTY-NOTICES.md"),
            "old THIRD-PARTY-NOTICES.md"
        );
        assert_eq!(read(dir.path(), "README.txt"), "new README.txt");
        assert!(
            pending_notices(dir.path()).is_file(),
            "About opens the pending notices meanwhile"
        );
        busy.borrow_mut().clear();
        assert_eq!(
            install_pending_docs(dir.path(), KEY, held(&busy, ERROR_SHARING_VIOLATION)),
            Pending::Docs(vec![("THIRD-PARTY-NOTICES.md", Outcome::Installed)])
        );
        assert_eq!(
            read(dir.path(), "THIRD-PARTY-NOTICES.md"),
            "new THIRD-PARTY-NOTICES.md"
        );
        assert!(!dir.path().join(PENDING_DIR).exists());
    }

    #[test]
    fn held_documents_share_one_retry_budget_and_a_writable_one_installs_at_once() {
        let dir = folder(Some(KEY));
        // README and LICENSE stay held; the notices are writable.
        let busy = RefCell::new(vec!["README.txt", "LICENSE.txt"]);
        let pauses = Cell::new(0);
        let outcome = install_with_shared_budget(
            dir.path(),
            KEY,
            3,
            Duration::from_secs(1),
            held(&busy, ERROR_SHARING_VIOLATION),
            |_| pauses.set(pauses.get() + 1),
        );
        assert_eq!(
            pauses.get(),
            3,
            "two held documents use one budget, not one each"
        );
        assert_eq!(
            outcome,
            Pending::Docs(vec![
                ("portable.marker", Outcome::Installed),
                ("README.txt", Outcome::InUse),
                ("LICENSE.txt", Outcome::InUse),
                ("THIRD-PARTY-NOTICES.md", Outcome::Installed),
            ])
        );
        assert_eq!(
            read(dir.path(), "THIRD-PARTY-NOTICES.md"),
            "new THIRD-PARTY-NOTICES.md"
        );
        // Held forever: they stay pending for the next start, which installs
        // them once they are released.
        assert!(dir.path().join(PENDING_DIR).join("README.txt").is_file());
        busy.borrow_mut().clear();
        assert_eq!(
            install_pending_docs(dir.path(), KEY, rename),
            Pending::Docs(vec![
                ("README.txt", Outcome::Installed),
                ("LICENSE.txt", Outcome::Installed)
            ])
        );
    }

    #[test]
    fn the_budget_stops_as_soon_as_the_lock_clears() {
        let dir = folder(Some(KEY));
        let tries = Cell::new(0);
        let replace = |from: &Path, to: &Path| {
            if to.ends_with("LICENSE.txt") {
                tries.set(tries.get() + 1);
                if tries.get() < 3 {
                    return Err(io::Error::from_raw_os_error(ERROR_LOCK_VIOLATION));
                }
            }
            rename(from, to)
        };
        let pauses = Cell::new(0);
        let outcome =
            install_with_shared_budget(dir.path(), KEY, 10, Duration::ZERO, replace, |_| {
                pauses.set(pauses.get() + 1)
            });
        assert_eq!(pauses.get(), 2);
        let Pending::Docs(outcomes) = outcome else {
            panic!("docs")
        };
        assert!(outcomes.iter().all(|(_, o)| *o == Outcome::Installed));
        assert!(!dir.path().join(PENDING_DIR).exists());
    }

    #[test]
    fn a_document_another_start_installed_is_not_reported_by_this_one() {
        // A second start of the app installs the same folder at the same
        // time. Its rename of the README wins the race; this pass finds the
        // pending copy gone and says nothing about it.
        let dir = folder(Some(KEY));
        let replace = |from: &Path, to: &Path| {
            if to.ends_with("README.txt") {
                rename(from, to)?;
                return Err(io::Error::from_raw_os_error(2));
            }
            rename(from, to)
        };
        let Pending::Docs(outcomes) = install_pending_docs(dir.path(), KEY, replace) else {
            panic!("docs")
        };
        assert_eq!(
            outcomes.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
            vec!["portable.marker", "LICENSE.txt", "THIRD-PARTY-NOTICES.md"]
        );
        assert!(outcomes.iter().all(|(_, o)| *o == Outcome::Installed));
        assert_eq!(read(dir.path(), "README.txt"), "new README.txt");
        // The other start installs a held document during this one's pause:
        // this pass stops waiting for it and does not call it in use.
        let dir = folder(Some(KEY));
        let busy = RefCell::new(vec!["LICENSE.txt"]);
        let other_start = |_: Duration| {
            let pending = dir.path().join(PENDING_DIR);
            if pending.join("LICENSE.txt").is_file() {
                rename(
                    &pending.join("LICENSE.txt"),
                    &dir.path().join("LICENSE.txt"),
                )
                .unwrap();
            }
        };
        let pauses = Cell::new(0);
        let outcome = install_with_shared_budget(
            dir.path(),
            KEY,
            5,
            Duration::ZERO,
            held(&busy, ERROR_SHARING_VIOLATION),
            |d| {
                pauses.set(pauses.get() + 1);
                other_start(d)
            },
        );
        assert_eq!(pauses.get(), 1, "the budget ends once nothing waits");
        let Pending::Docs(outcomes) = outcome else {
            panic!("docs")
        };
        assert!(outcomes
            .iter()
            .all(|(n, o)| *n != "LICENSE.txt" && *o == Outcome::Installed));
        assert_eq!(read(dir.path(), "LICENSE.txt"), "new LICENSE.txt");
        assert!(!dir.path().join(PENDING_DIR).exists());
    }

    #[test]
    fn a_directory_at_a_document_path_is_never_written_into() {
        // Found before the attempt, empty or not.
        for nested in [true, false] {
            let dir = folder(Some(KEY));
            let target = dir.path().join("THIRD-PARTY-NOTICES.md");
            std::fs::remove_file(&target).unwrap();
            std::fs::create_dir(&target).unwrap();
            if nested {
                std::fs::write(target.join("THIRD-PARTY-NOTICES.md"), b"sentinel").unwrap();
            }
            let Pending::Docs(outcomes) = install_pending_docs(dir.path(), KEY, rename) else {
                panic!("docs")
            };
            assert_eq!(outcomes[3], ("THIRD-PARTY-NOTICES.md", Outcome::Directory));
            let inside: Vec<_> = std::fs::read_dir(&target)
                .unwrap()
                .flatten()
                .map(|e| e.file_name())
                .collect();
            assert_eq!(inside.len(), usize::from(nested));
            if nested {
                assert_eq!(
                    std::fs::read(target.join("THIRD-PARTY-NOTICES.md")).unwrap(),
                    b"sentinel"
                );
            }
            assert_eq!(read(dir.path(), "README.txt"), "new README.txt");
            assert!(
                !dir.path().join(PENDING_DIR).exists(),
                "nothing is left pending"
            );
        }
    }

    #[test]
    fn a_directory_that_appears_between_attempts_is_reported_and_untouched() {
        let dir = folder(Some(KEY));
        let target = dir.path().join("LICENSE.txt");
        // The first attempt finds the document held; during the pause a
        // directory with a file inside replaces it; the retry finds the
        // directory before writing anything.
        let busy = RefCell::new(vec!["LICENSE.txt"]);
        let replace = held(&busy, ERROR_SHARING_VIOLATION);
        let swap = |_: Duration| {
            if !target.is_dir() {
                std::fs::remove_file(&target).unwrap();
                std::fs::create_dir(&target).unwrap();
                std::fs::write(target.join("LICENSE.txt"), b"sentinel").unwrap();
            }
        };
        let outcome = install_with_shared_budget(dir.path(), KEY, 2, Duration::ZERO, replace, swap);
        let Pending::Docs(outcomes) = outcome else {
            panic!("docs")
        };
        assert_eq!(outcomes[2], ("LICENSE.txt", Outcome::Directory));
        assert_eq!(
            std::fs::read(target.join("LICENSE.txt")).unwrap(),
            b"sentinel"
        );
        assert!(!dir.path().join(PENDING_DIR).exists());
    }

    #[test]
    fn a_directory_that_appears_during_the_rename_is_reported_as_a_directory() {
        let dir = folder(Some(KEY));
        let target = dir.path().join("LICENSE.txt");
        // The rename fails because a directory took the path at that moment
        // (Windows: access denied): the outcome says directory, not denied.
        let replace = |from: &Path, to: &Path| {
            if to == target {
                std::fs::remove_file(to).unwrap();
                std::fs::create_dir(to).unwrap();
                return Err(io::Error::from_raw_os_error(ERROR_ACCESS_DENIED));
            }
            rename(from, to)
        };
        let Pending::Docs(outcomes) = install_pending_docs(dir.path(), KEY, replace) else {
            panic!("docs")
        };
        assert_eq!(outcomes[2], ("LICENSE.txt", Outcome::Directory));
        assert!(target.is_dir());
    }

    #[test]
    fn a_failure_that_is_not_a_lock_is_named_and_the_installed_document_kept() {
        let dir = folder(Some(KEY));
        let replace = |from: &Path, to: &Path| {
            if to.ends_with("README.txt") {
                return Err(io::Error::new(
                    io::ErrorKind::StorageFull,
                    "There is not enough space on the disk.",
                ));
            }
            rename(from, to)
        };
        let Pending::Docs(outcomes) = install_pending_docs(dir.path(), KEY, replace) else {
            panic!("docs")
        };
        assert_eq!(
            outcomes[1],
            (
                "README.txt",
                Outcome::Failed("There is not enough space on the disk.".into())
            )
        );
        assert_eq!(read(dir.path(), "README.txt"), "old README.txt");
        assert_eq!(
            outcome_line("README.txt", &outcomes[1].1),
            "portable update: README.txt could not be replaced (There is not enough space on the disk.), kept the installed one"
        );
        assert!(!outcome_line("README.txt", &outcomes[1].1).contains("in use"));
        // Access denied is said as such, and waits.
        let dir = folder(Some(KEY));
        let busy = RefCell::new(vec!["README.txt"]);
        let Pending::Docs(outcomes) =
            install_pending_docs(dir.path(), KEY, held(&busy, ERROR_ACCESS_DENIED))
        else {
            panic!("docs")
        };
        assert_eq!(outcomes[1], ("README.txt", Outcome::AccessDenied));
        assert!(outcome_line("README.txt", &Outcome::AccessDenied)
            .contains("access to README.txt was denied"));
    }

    #[test]
    fn a_folder_of_another_exe_without_a_key_or_that_is_a_file_is_removed() {
        for key in [Some("other-exe"), None] {
            let dir = folder(key);
            let replace = |_: &Path, _: &Path| -> io::Result<()> {
                panic!("a stale document is never installed")
            };
            assert_eq!(
                install_pending_docs(dir.path(), KEY, replace),
                Pending::Stale
            );
            assert!(!dir.path().join(PENDING_DIR).exists(), "{key:?}");
            assert_eq!(read(dir.path(), "README.txt"), "old README.txt");
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(PENDING_DIR), b"not a folder").unwrap();
        assert_eq!(
            install_pending_docs(dir.path(), KEY, rename),
            Pending::Stale
        );
        assert!(!dir.path().join(PENDING_DIR).exists());
    }

    #[test]
    fn staging_is_whole_or_absent_and_a_stopped_one_leaves_nothing_at_the_next_start() {
        let dir = tempfile::tempdir().unwrap();
        let stage = tempfile::tempdir().unwrap();
        let exe = stage.path().join("AeroFTP.exe");
        std::fs::write(&exe, b"abc").unwrap();
        let readme = stage.path().join("README.txt");
        std::fs::write(&readme, b"new readme").unwrap();
        let missing = stage.path().join("LICENSE.txt");
        // A source that cannot be read: nothing is staged, an older folder stays.
        std::fs::create_dir(dir.path().join(PENDING_DIR)).unwrap();
        std::fs::write(dir.path().join(PENDING_DIR).join("README.txt"), b"older").unwrap();
        assert!(stage_pending_docs(
            dir.path(),
            &exe,
            &[(&readme, "README.txt"), (&missing, "LICENSE.txt")],
            "a"
        )
        .is_err());
        assert_eq!(
            std::fs::read(dir.path().join(PENDING_DIR).join("README.txt")).unwrap(),
            b"older"
        );
        assert!(!dir.path().join(format!("{TMP_PREFIX}a")).exists());
        // A whole stage replaces it, with the key of the new exe.
        stage_pending_docs(dir.path(), &exe, &[(&readme, "README.txt")], "b").unwrap();
        assert_eq!(
            std::fs::read(dir.path().join(PENDING_DIR).join("README.txt")).unwrap(),
            b"new readme"
        );
        let key = exe_key(&exe).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join(PENDING_DIR).join(KEY_FILE)).unwrap(),
            key
        );
        // A staging folder a crash left is removed at the next start, and
        // never read as pending.
        std::fs::create_dir(dir.path().join(format!("{TMP_PREFIX}c"))).unwrap();
        std::fs::write(
            dir.path().join(format!("{TMP_PREFIX}c")).join("README.txt"),
            b"partial",
        )
        .unwrap();
        assert_eq!(
            install_pending_docs(dir.path(), &key, rename),
            Pending::Docs(vec![("README.txt", Outcome::Installed)])
        );
        assert_eq!(
            std::fs::read(dir.path().join("README.txt")).unwrap(),
            b"new readme"
        );
        assert!(!dir.path().join(format!("{TMP_PREFIX}c")).exists());
        // Discarding an update removes its folder.
        stage_pending_docs(dir.path(), &exe, &[(&readme, "README.txt")], "d").unwrap();
        discard_pending_docs(dir.path());
        assert!(!dir.path().join(PENDING_DIR).exists());
    }

    #[test]
    fn the_key_of_an_exe_is_the_sha256_of_its_bytes() {
        let dir = tempfile::tempdir().expect("temp dir");
        let exe = dir.path().join("AeroFTP.exe");
        std::fs::write(&exe, b"abc").expect("exe");
        assert_eq!(
            exe_key(&exe).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
