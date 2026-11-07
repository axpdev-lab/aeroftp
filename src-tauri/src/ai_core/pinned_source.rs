// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! A local upload source the path policy saw, pinned for the upload.
//!
//! An AI tool checks a local path and the backend then opens that path again
//! by name: between the two the path could name another file, and the policy
//! would have approved one file while another one was sent (deep scan item
//! 27, CWE-367). `PinnedSource::pin` opens the file once, runs the policy on
//! the canonical path, and requires the canonical path to still name the
//! opened file. `verify_unchanged` repeats the identity check after the
//! upload, so a source replaced or rewritten during it is reported as an
//! error instead of a success.
//!
//! The backends keep receiving a path: forty provider uploads and three
//! external programs (MEGAcmd, the Proton CLI, libmtp) open it themselves, so
//! a file-descriptor path would change what every one of them reads.
//!
//! On Windows the opened handle stays open for the whole upload with
//! `FILE_SHARE_READ` only: while it is held, the file cannot be written,
//! renamed or deleted, and neither can a folder above it be renamed.
//!
//! Residual: on Unix a swap of the path to another file and back inside the
//! upload window is not seen. The threat model trusts the local user; what
//! is closed here is the policy and the open seeing different files.

use super::tools::ToolError;

/// What identifies the opened file: the inode, its size and its mtime on
/// Unix; the size and the creation and write times on Windows, where the
/// share mode keeps the path on the same file.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Identity {
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    size: u64,
    #[cfg(unix)]
    mtime: (i64, i64),
    #[cfg(windows)]
    created: u64,
    #[cfg(windows)]
    written: u64,
}

impl Identity {
    fn of(meta: &std::fs::Metadata) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Identity {
                dev: meta.dev(),
                ino: meta.ino(),
                size: meta.size(),
                mtime: (meta.mtime(), meta.mtime_nsec()),
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            Identity {
                size: meta.file_size(),
                created: meta.creation_time(),
                written: meta.last_write_time(),
            }
        }
        #[cfg(not(any(unix, windows)))]
        {
            Identity { size: meta.len() }
        }
    }
}

/// A local file the path policy approved, held for one upload.
#[derive(Debug)]
pub(crate) struct PinnedSource {
    path: String,
    canonical: std::path::PathBuf,
    identity: Identity,
    /// Held open on Windows for its share mode; closed right away elsewhere.
    #[cfg(windows)]
    _handle: std::fs::File,
}

impl PinnedSource {
    /// Open `path`, run `policy` on its canonical form, and require that
    /// form to name the opened file. Only a regular file can be pinned.
    pub(crate) fn pin(
        path: &str,
        label: &str,
        policy: impl Fn(&str, &str) -> Result<(), ToolError>,
    ) -> Result<Self, ToolError> {
        // Said before the open as well: Windows refuses to open a folder this
        // way, with an error that would not say why.
        if std::fs::metadata(path).is_ok_and(|meta| !meta.is_file()) {
            return Err(ToolError::Exec(format!(
                "{label}: {path} is not a regular file"
            )));
        }
        let file = open_source(path).map_err(|e| ToolError::Exec(format!("{label}: {e}")))?;
        let opened = file
            .metadata()
            .map_err(|e| ToolError::Exec(format!("{label}: {e}")))?;
        if !opened.is_file() {
            return Err(ToolError::Exec(format!(
                "{label}: {path} is not a regular file"
            )));
        }
        let canonical =
            std::fs::canonicalize(path).map_err(|e| ToolError::Exec(format!("{label}: {e}")))?;
        policy(&canonical.to_string_lossy(), label)?;
        let pinned = PinnedSource {
            path: path.to_string(),
            canonical,
            identity: Identity::of(&opened),
            #[cfg(windows)]
            _handle: file,
        };
        if !pinned.still_names_the_file() {
            return Err(ToolError::Exec(format!(
                "{label}: {path} changed while it was checked; nothing was uploaded"
            )));
        }
        Ok(pinned)
    }

    /// The path the backend opens, as the caller gave it.
    pub(crate) fn path(&self) -> &str {
        &self.path
    }

    /// The size of the file the policy saw.
    pub(crate) fn size(&self) -> u64 {
        self.identity.size
    }

    /// After the upload: an error naming the path when it no longer names
    /// the file pinned before, or that file was rewritten.
    pub(crate) fn verify_unchanged(&self) -> Result<(), ToolError> {
        if self.still_names_the_file() {
            Ok(())
        } else {
            Err(ToolError::Exec(format!(
                "{} changed during the upload: what reached the server may not be the file that was checked",
                self.path
            )))
        }
    }

    /// The answer of an upload of this source: the upload's own error, or,
    /// when it went through, an error when the source changed meanwhile.
    pub(crate) fn settle_upload(&self, sent: Result<(), String>) -> Result<(), String> {
        sent.and_then(|()| self.verify_unchanged().map_err(|e| e.to_string()))
    }

    fn still_names_the_file(&self) -> bool {
        let names = |p: &std::path::Path| {
            std::fs::metadata(p).is_ok_and(|meta| Identity::of(&meta) == self.identity)
        };
        names(std::path::Path::new(&self.path)) && names(&self.canonical)
    }
}

#[cfg(unix)]
fn open_source(path: &str) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    // O_NONBLOCK: opening a FIFO for reading would otherwise wait for a
    // writer; the regular-file check refuses it right after.
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
}

#[cfg(windows)]
fn open_source(path: &str) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(path)
}

#[cfg(not(any(unix, windows)))]
fn open_source(path: &str) -> std::io::Result<std::fs::File> {
    std::fs::File::open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allow(_: &str, _: &str) -> Result<(), ToolError> {
        Ok(())
    }

    #[test]
    fn a_regular_file_pins_and_stays_verified() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("report.pdf");
        std::fs::write(&file, b"report").unwrap();
        let pinned = PinnedSource::pin(&file.to_string_lossy(), "local_path", allow).unwrap();
        assert_eq!(pinned.size(), 6);
        assert_eq!(pinned.path(), file.to_string_lossy());
        pinned.verify_unchanged().unwrap();
    }

    #[test]
    fn the_policy_judges_the_canonical_path() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("plain.txt");
        std::fs::write(&file, b"x").unwrap();
        let seen = std::sync::Mutex::new(String::new());
        let spelled = dir.path().join(".").join("plain.txt");
        PinnedSource::pin(&spelled.to_string_lossy(), "local_path", |p, _| {
            *seen.lock().unwrap() = p.to_string();
            Ok(())
        })
        .unwrap();
        let canonical = std::fs::canonicalize(&file).unwrap();
        assert_eq!(*seen.lock().unwrap(), canonical.to_string_lossy());
        let refused = PinnedSource::pin(&file.to_string_lossy(), "local_path", |_, _| {
            Err(ToolError::Denied("refused".into()))
        });
        assert!(matches!(refused, Err(ToolError::Denied(_))));
    }

    #[test]
    fn a_folder_is_not_pinned() {
        let dir = tempfile::tempdir().unwrap();
        let err =
            PinnedSource::pin(&dir.path().to_string_lossy(), "local_path", allow).unwrap_err();
        assert!(err.to_string().contains("not a regular file"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn a_swap_between_the_open_and_the_check_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("notes.txt");
        let secret = dir.path().join("id_fixture");
        std::fs::write(&file, b"notes").unwrap();
        std::fs::write(&secret, b"secret").unwrap();
        // The policy runs after the open: swapping the path inside it is the
        // window between what was opened and what the policy approves.
        let err = PinnedSource::pin(&file.to_string_lossy(), "local_path", |_, _| {
            std::fs::remove_file(&file).unwrap();
            std::os::unix::fs::symlink(&secret, &file).unwrap();
            Ok(())
        })
        .unwrap_err();
        assert!(
            err.to_string().contains("changed while it was checked"),
            "{err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_source_replaced_or_rewritten_during_the_upload_fails_the_check_after() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("data.bin");
        std::fs::write(&file, b"original").unwrap();
        let path = file.to_string_lossy().to_string();
        let replaced = PinnedSource::pin(&path, "local_path", allow).unwrap();
        let other = dir.path().join("other.bin");
        std::fs::write(&other, b"original").unwrap();
        std::fs::rename(&other, &file).unwrap();
        let err = replaced.verify_unchanged().unwrap_err();
        assert!(
            err.to_string().contains("changed during the upload"),
            "{err}"
        );
        assert!(err.to_string().contains(&path), "{err}");

        let rewritten = PinnedSource::pin(&path, "local_path", allow).unwrap();
        std::fs::write(&file, b"rewritten, longer").unwrap();
        assert!(rewritten.verify_unchanged().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_is_refused_without_waiting_for_a_writer() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("pipe");
        let c_path = std::ffi::CString::new(fifo.to_string_lossy().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
        let err = PinnedSource::pin(&fifo.to_string_lossy(), "local_path", allow).unwrap_err();
        assert!(err.to_string().contains("not a regular file"), "{err}");
    }

    /// The GUI's `upload_files` reports each file through
    /// `record_upload_outcome`: a file that went through unchanged is
    /// uploaded, the upload's own error stays its own, and a source replaced
    /// while it was sent is an error naming it, never a file reported as
    /// uploaded.
    #[cfg(unix)]
    #[test]
    fn the_gui_upload_reports_a_source_replaced_after_the_pin_as_an_error() {
        use crate::ai_core::gui_tools::record_upload_outcome;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("data.bin");
        std::fs::write(&file, b"original").unwrap();
        let path = file.to_string_lossy().to_string();
        let pinned = PinnedSource::pin(&path, "path", allow).unwrap();
        let (mut uploaded, mut errors) = (Vec::new(), Vec::new());

        record_upload_outcome(&pinned, Ok(()), "sent".into(), &mut uploaded, &mut errors);
        let quota = Err("quota exceeded".into());
        record_upload_outcome(&pinned, quota, "full".into(), &mut uploaded, &mut errors);
        let other = dir.path().join("other.bin");
        std::fs::write(&other, b"original").unwrap();
        std::fs::rename(&other, &file).unwrap();
        record_upload_outcome(
            &pinned,
            Ok(()),
            "replaced".into(),
            &mut uploaded,
            &mut errors,
        );

        assert_eq!(uploaded, vec!["sent".to_string()]);
        assert_eq!(
            errors[0],
            serde_json::json!({"file": "full", "error": "quota exceeded"})
        );
        assert_eq!(errors[1]["file"], "replaced");
        let error = errors[1]["error"].as_str().unwrap();
        assert!(error.contains("changed during the upload"), "{error}");
        assert!(error.contains(&path), "{error}");
    }

    /// Without `O_NONBLOCK` the open of a FIFO waits for a writer, and a file
    /// that turned into one between the check and the open hung the upload.
    #[cfg(unix)]
    #[test]
    fn opening_a_fifo_without_a_writer_does_not_wait() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("pipe");
        let c_path = std::ffi::CString::new(fifo.to_string_lossy().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
        let (sender, receiver) = std::sync::mpsc::channel();
        let path = fifo.to_string_lossy().to_string();
        std::thread::spawn(move || {
            let opened = open_source(&path).map(|file| file.metadata().map(|m| m.is_file()));
            let _ = sender.send(opened.map_err(|e| e.to_string()));
        });
        let opened = receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the open of a FIFO with no writer returned at once");
        assert!(!opened.expect("opened").expect("metadata"));
    }

    #[cfg(windows)]
    #[test]
    fn a_pinned_file_and_its_folder_cannot_be_renamed_or_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("work");
        std::fs::create_dir(&folder).unwrap();
        let file = folder.join("report.pdf");
        std::fs::write(&file, b"report").unwrap();
        let pinned = PinnedSource::pin(&file.to_string_lossy(), "local_path", allow).unwrap();
        assert!(std::fs::rename(&file, folder.join("moved.pdf")).is_err());
        assert!(std::fs::remove_file(&file).is_err());
        assert!(std::fs::write(&file, b"rewritten").is_err());
        assert!(std::fs::rename(&folder, dir.path().join("work-moved")).is_err());
        pinned.verify_unchanged().unwrap();
        drop(pinned);
        std::fs::rename(&file, folder.join("moved.pdf")).unwrap();
    }
}
