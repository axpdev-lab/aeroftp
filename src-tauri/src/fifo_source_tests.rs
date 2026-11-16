// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! A FIFO given as a file to copy or hash, an archive to read or a compress
//! input held the operation inside `open` until a writer appeared, before
//! any type check or Stop could refuse it. Each reader here answers at once
//! now.

#![cfg(unix)]

use std::path::PathBuf;

type Case = (
    &'static str,
    Box<dyn FnOnce(PathBuf) -> String + Send>,
    &'static str,
);

/// Run each case on a FIFO no one writes to; a case that waits more than 5
/// seconds is let go by writers (so the test ends) and reported.
fn refused_at_once(cases: Vec<Case>) -> Vec<String> {
    refused_within(cases, std::time::Duration::from_secs(5))
}

fn refused_within(cases: Vec<Case>, patience: std::time::Duration) -> Vec<String> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::OpenOptionsExt;
    let mut wrong = Vec::new();
    for (case, run, expected) in cases {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("source");
        let path = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: a NUL-terminated path that lives for the call.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        let (sent, received) = std::sync::mpsc::channel();
        let given = fifo.clone();
        let worker = std::thread::spawn(move || {
            let _ = sent.send(run(given));
        });
        match received.recv_timeout(patience) {
            Ok(answer) if answer.contains(expected) => {}
            Ok(answer) => wrong.push(format!("{case}: {answer}")),
            Err(_) => {
                // Waiting in `open`, or done just now. A nonblocking writer
                // lets a waiting open go, and fails (ENXIO) once no reader is
                // left, so this cleanup never waits itself; it gives up after
                // 10 seconds.
                let late = (0..100).find_map(|_| {
                    let _ = std::fs::OpenOptions::new()
                        .write(true)
                        .custom_flags(libc::O_NONBLOCK)
                        .open(&fifo);
                    received
                        .recv_timeout(std::time::Duration::from_millis(100))
                        .ok()
                });
                match late {
                    Some(late) => {
                        wrong.push(format!("{case}: did not answer in time, then {late}"))
                    }
                    None => {
                        // Not joined: it would hold the test with it.
                        wrong.push(format!("{case}: never answered"));
                        continue;
                    }
                }
            }
        }
        worker.join().unwrap();
    }
    wrong
}

/// The cleanup after a timeout never waits itself: a case that answers just
/// after it without ever opening the FIFO leaves no reader, and a blocking
/// writer's open would wait for one forever.
#[test]
fn a_case_answering_after_the_timeout_is_reported_not_waited_on() {
    let (sent, received) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let wrong = refused_within(
            vec![(
                "late",
                Box::new(|_| {
                    std::thread::sleep(std::time::Duration::from_millis(300));
                    "late answer".to_string()
                }),
                "never",
            )],
            std::time::Duration::from_millis(100),
        );
        let _ = sent.send(wrong);
    });
    let wrong = received
        .recv_timeout(std::time::Duration::from_secs(20))
        .expect("the cleanup waited on its own open");
    assert_eq!(wrong, ["late: did not answer in time, then late answer"]);
}

/// A FIFO with no writer held the local copy inside `open`, before the check
/// that refuses it and before any Stop could be seen.
#[test]
fn a_fifo_to_copy_is_refused_at_once() {
    let wrong = refused_at_once(vec![(
        "copy_file_cancellable",
        Box::new(|path| {
            let to = path.with_file_name("copy");
            let copied = super::copy_file_cancellable(
                &path,
                &to,
                &std::sync::atomic::AtomicBool::new(false),
                super::LocalPublish::Replace,
            );
            if to.exists() {
                return format!("{copied:?}, and a copy was left at the destination");
            }
            format!("{copied:?}")
        }),
        REFUSED,
    )]);
    assert!(wrong.is_empty(), "{wrong:#?}");
}

fn text(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
}

const REFUSED: &str = "not a regular file";

#[test]
fn a_fifo_to_hash_is_refused_at_once() {
    let wrong = refused_at_once(vec![
        (
            "calculate_checksum",
            Box::new(|path| {
                format!(
                    "{:?}",
                    tauri::async_runtime::block_on(super::calculate_checksum(
                        text(&path),
                        "sha256".to_string()
                    ))
                )
            }),
            REFUSED,
        ),
        (
            "compute_sha256 (local sync tree)",
            Box::new(|path| {
                format!(
                    "{:?}",
                    tauri::async_runtime::block_on(super::compute_sha256(&path))
                )
            }),
            "None",
        ),
    ]);
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn a_fifo_picked_as_an_archive_is_refused_at_once() {
    fn out(path: &std::path::Path) -> String {
        text(&path.with_file_name("out"))
    }
    let wrong = refused_at_once(vec![
        (
            "extract_archive",
            Box::new(|path| {
                format!(
                    "{:?}",
                    tauri::async_runtime::block_on(super::extract_archive(
                        text(&path),
                        out(&path),
                        false,
                        None
                    ))
                )
            }),
            REFUSED,
        ),
        (
            "extract_7z",
            Box::new(|path| {
                format!(
                    "{:?}",
                    tauri::async_runtime::block_on(super::extract_7z(
                        text(&path),
                        out(&path),
                        None,
                        false
                    ))
                )
            }),
            REFUSED,
        ),
        (
            "extract_single_impl",
            Box::new(|path| {
                format!(
                    "{:?}",
                    tauri::async_runtime::block_on(super::extract_single_impl(
                        text(&path),
                        out(&path),
                        false,
                        Some("gz".to_string())
                    ))
                )
            }),
            REFUSED,
        ),
        (
            "is_zip_encrypted",
            Box::new(|path| {
                format!(
                    "{:?}",
                    tauri::async_runtime::block_on(super::is_zip_encrypted(text(&path)))
                )
            }),
            REFUSED,
        ),
        (
            "is_7z_encrypted",
            Box::new(|path| {
                format!(
                    "{:?}",
                    tauri::async_runtime::block_on(super::is_7z_encrypted(text(&path)))
                )
            }),
            REFUSED,
        ),
        (
            "detect_zip_meta",
            Box::new(|path| format!("{:?}", super::detect_zip_meta(&text(&path)).map(|_| ()))),
            REFUSED,
        ),
        (
            "detect_7z_meta",
            Box::new(|path| format!("{:?}", super::detect_7z_meta(&text(&path)).map(|_| ()))),
            REFUSED,
        ),
        (
            "detect_7z_compression",
            Box::new(|path| format!("{:?}", super::detect_7z_compression(&text(&path)))),
            "None",
        ),
        (
            "detect_rar_cipher",
            Box::new(|path| format!("{:?}", super::detect_rar_cipher(&text(&path)))),
            REFUSED,
        ),
        (
            "rar_signature_cipher",
            Box::new(|path| format!("{:?}", super::rar_signature_cipher(&text(&path)))),
            REFUSED,
        ),
        (
            "tar_reader_for",
            Box::new(|path| {
                format!(
                    "{:?}",
                    super::tar_reader_for(&text(&path), Some("tar")).map(|_| ())
                )
            }),
            REFUSED,
        ),
    ]);
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn a_fifo_given_as_a_compress_input_is_refused_at_once() {
    fn progress() -> crate::archive_progress::ArchiveProgress {
        crate::archive_progress::ArchiveProgress::for_optional_app(
            None,
            crate::archive_progress::phase::COMPRESSING,
            0,
        )
    }
    let wrong = refused_at_once(vec![
        (
            "add_zip_file_entry",
            Box::new(|path| {
                let archive = tempfile::tempfile().unwrap();
                let mut zip = zip::ZipWriter::new(archive);
                format!(
                    "{:?}",
                    super::add_zip_file_entry(
                        &mut zip,
                        "source".to_string(),
                        &path,
                        6,
                        &None,
                        &mut progress(),
                        None
                    )
                )
            }),
            REFUSED,
        ),
        (
            "zip_large_should_store",
            Box::new(|path| format!("{:?}", super::zip_large_should_store(&path, 6))),
            REFUSED,
        ),
        (
            "tar_append_file",
            Box::new(|path| {
                let mut archive = tar::Builder::new(Vec::new());
                format!(
                    "{:?}",
                    super::tar_append_file(&mut archive, &path, "source", &mut progress(), None)
                )
            }),
            REFUSED,
        ),
        (
            "read_region_append (compression estimate)",
            Box::new(|path| {
                let mut sample = Vec::new();
                super::read_region_append(&path, 0, 16, &mut sample);
                format!("read {} bytes", sample.len())
            }),
            "read 0 bytes",
        ),
    ]);
    assert!(wrong.is_empty(), "{wrong:#?}");
}
