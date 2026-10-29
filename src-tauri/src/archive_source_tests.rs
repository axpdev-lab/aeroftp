// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use super::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

const FORMATS: &[&str] = &["zip", "7z", "tar", "tar.gz", "tar.xz", "tar.bz2"];

struct Fixture {
    dir: tempfile::TempDir,
    home: PathBuf,
    output: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().canonicalize().unwrap().join("fixture-home");
        std::fs::create_dir_all(home.join("ordinary/nested")).unwrap();
        std::fs::write(
            home.join("ordinary/nested/hello.txt"),
            b"ordinary fixture bytes",
        )
        .unwrap();
        let output = dir.path().join("existing.archive");
        std::fs::write(&output, b"original archive").unwrap();
        Self { dir, home, output }
    }

    fn policy(&self, path: &Path) -> Result<(), String> {
        // A dummy user's policy. Never create or read the real user's secrets,
        // and never change HOME while other Rust tests may be running.
        let path = ai_core::local_tools::comparable_path(
            path.to_str().ok_or("Archive path is not valid UTF-8")?,
            cfg!(windows),
        );
        let prefix = ai_core::local_tools::comparable_path(
            self.home.join(".ssh").to_str().unwrap(),
            cfg!(windows),
        );
        if ai_core::local_tools::path_matches_prefix(&path, &prefix) {
            Err("fixture sensitive path denied".into())
        } else {
            Ok(())
        }
    }

    fn protected(&self) -> PathBuf {
        let path = self.home.join(".ssh/deeper/id_fixture");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"DUMMY SECRET: never archive this").unwrap();
        path
    }

    fn preserved(&self) {
        assert_eq!(std::fs::read(&self.output).unwrap(), b"original archive");
        assert_eq!(
            std::fs::read_dir(self.dir.path()).unwrap().count(),
            2,
            "no temporary archive may remain"
        );
    }

    async fn compress(&self, root: &Path, format: &str) -> Result<String, String> {
        compress_archive_with_policy(
            vec![root.to_str().unwrap().into()],
            self.output.to_str().unwrap().into(),
            format,
            None,
            Some(0),
            &|path| self.policy(path),
        )
        .await
    }

    async fn extract(&self, format: &str, password: Option<String>) -> PathBuf {
        let dest = self.dir.path().join("decoded");
        let archive = self.output.to_str().unwrap().to_string();
        let out = dest.to_str().unwrap().to_string();
        match format {
            "zip" => extract_archive_core(archive, out, false, password)
                .await
                .unwrap(),
            "7z" => extract_7z_core(archive, out, password, false)
                .await
                .unwrap(),
            _ => extract_tar_as_core(archive, out, false, format.to_string())
                .await
                .unwrap(),
        };
        dest
    }
}

macro_rules! denies_descendant {
    ($name:ident, $format:literal) => {
        #[tokio::test]
        async fn $name() {
            let fixture = Fixture::new();
            fixture.protected();
            let err = fixture.compress(&fixture.home, $format).await.unwrap_err();
            assert!(err.contains("sensitive path denied"), "{err}");
            fixture.preserved();
        }
    };
}
denies_descendant!(requested102_zip_denies_descendant, "zip");
denies_descendant!(requested102_7z_denies_descendant, "7z");
denies_descendant!(requested102_tar_denies_descendant, "tar");
denies_descendant!(requested102_targz_denies_descendant, "tar.gz");
denies_descendant!(requested102_tarxz_denies_descendant, "tar.xz");
denies_descendant!(requested102_tarbz2_denies_descendant, "tar.bz2");

#[tokio::test]
async fn requested102_allowed_siblings_round_trip_in_every_format() {
    for format in FORMATS {
        let fixture = Fixture::new();
        std::fs::create_dir(fixture.home.join(".ssh-backup")).unwrap();
        std::fs::write(
            fixture.home.join(".ssh-backup/allowed.txt"),
            b"allowed prefix sibling",
        )
        .unwrap();
        std::fs::write(
            fixture.home.join("ordinary/nested/ខ្មែរ.txt"),
            b"unicode fixture",
        )
        .unwrap();
        std::fs::create_dir(fixture.home.join("empty")).unwrap();
        let output = fixture.compress(&fixture.home, format).await.unwrap();
        assert_eq!(output, fixture.output.to_str().unwrap(), "{format}");
        let decoded = fixture.extract(format, None).await.join("fixture-home");
        assert_eq!(
            std::fs::read(decoded.join("ordinary/nested/hello.txt")).unwrap(),
            b"ordinary fixture bytes",
            "{format}"
        );
        assert_eq!(
            std::fs::read(decoded.join(".ssh-backup/allowed.txt")).unwrap(),
            b"allowed prefix sibling",
            "{format}"
        );
        assert_eq!(
            std::fs::read(decoded.join("ordinary/nested/ខ្មែរ.txt")).unwrap(),
            b"unicode fixture",
            "{format}"
        );
        if *format == "zip" {
            assert!(decoded.join("empty").is_dir());
        }
    }
}

#[tokio::test]
async fn requested102_mixed_roots_refuse_the_whole_operation() {
    for format in FORMATS {
        let fixture = Fixture::new();
        let denied = fixture.protected();
        let roots = vec![
            fixture.home.join("ordinary").to_str().unwrap().into(),
            denied.to_str().unwrap().into(),
        ];
        let err = compress_archive_with_policy(
            roots,
            fixture.output.to_str().unwrap().into(),
            format,
            None,
            Some(0),
            &|path| fixture.policy(path),
        )
        .await
        .unwrap_err();
        assert!(err.contains("sensitive path denied"), "{format}: {err}");
        fixture.preserved();
    }
}

#[tokio::test]
async fn requested102_missing_source_never_becomes_a_successful_partial_archive() {
    for format in FORMATS {
        let fixture = Fixture::new();
        let roots = vec![
            fixture.home.join("ordinary").to_str().unwrap().into(),
            fixture.home.join("missing").to_str().unwrap().into(),
        ];
        let err = compress_archive_with_policy(
            roots,
            fixture.output.to_str().unwrap().into(),
            format,
            None,
            Some(0),
            &|path| fixture.policy(path),
        )
        .await
        .unwrap_err();
        assert!(err.contains("cannot be resolved"), "{format}: {err}");
        fixture.preserved();
    }
}

#[tokio::test]
async fn requested102_unrestricted_compression_still_backs_up_protected_names() {
    for format in FORMATS {
        let fixture = Fixture::new();
        fixture.protected();
        let paths = vec![fixture.home.to_str().unwrap().into()];
        let output = fixture.output.to_str().unwrap().to_string();
        match *format {
            "zip" => compress_files_core(paths, output, None, Some(0))
                .await
                .unwrap(),
            "7z" => compress_7z_core(paths, output, None, Some(0), None, None)
                .await
                .unwrap(),
            _ => compress_tar_core(paths, output, format.to_string(), Some(0))
                .await
                .unwrap(),
        };
        let decoded = fixture.extract(format, None).await;
        assert_eq!(
            std::fs::read(decoded.join("fixture-home/.ssh/deeper/id_fixture")).unwrap(),
            b"DUMMY SECRET: never archive this",
            "{format}"
        );
    }
}

#[tokio::test]
async fn requested102_encrypted_archives_keep_allowed_payloads() {
    for format in ["zip", "7z"] {
        let fixture = Fixture::new();
        let password = Some("dummy fixture password".to_string());
        compress_archive_with_policy(
            vec![fixture.home.to_str().unwrap().into()],
            fixture.output.to_str().unwrap().into(),
            format,
            password.clone(),
            Some(0),
            &|path| fixture.policy(path),
        )
        .await
        .unwrap();
        let decoded = fixture.extract(format, password).await;
        assert_eq!(
            std::fs::read(decoded.join("fixture-home/ordinary/nested/hello.txt")).unwrap(),
            b"ordinary fixture bytes"
        );
    }
}

#[tokio::test]
async fn requested102_output_policy_is_rechecked_before_publication() {
    for format in FORMATS {
        let fixture = Fixture::new();
        let writing = AtomicBool::new(false);
        let policy = |path: &Path| {
            fixture.policy(path)?;
            if path == fixture.output && writing.load(Ordering::SeqCst) {
                return Err("fixture output policy changed".into());
            }
            if path.to_str().unwrap().contains(".aerotmp") {
                writing.store(true, Ordering::SeqCst);
            }
            Ok(())
        };
        let err = compress_archive_with_policy(
            vec![fixture.home.to_str().unwrap().into()],
            fixture.output.to_str().unwrap().into(),
            format,
            None,
            Some(0),
            &policy,
        )
        .await
        .unwrap_err();
        assert!(err.contains("output policy changed"), "{format}: {err}");
        fixture.preserved();
    }
}

#[tokio::test]
async fn requested102_empty_zip_keeps_directory_entries() {
    let fixture = Fixture::new();
    let root = fixture.home.join("empty-root");
    std::fs::create_dir_all(root.join("nested")).unwrap();
    fixture.compress(&root, "zip").await.unwrap();
    let decoded = fixture.extract("zip", None).await;
    assert!(decoded.join("empty-root/nested").is_dir());
}

#[cfg(unix)]
#[tokio::test]
async fn requested102_nested_aliases_cannot_expose_fixture_secrets() {
    for format in FORMATS {
        for directory in [false, true] {
            let fixture = Fixture::new();
            let secret = fixture.protected();
            let target = if directory {
                secret.parent().unwrap()
            } else {
                &secret
            };
            std::os::unix::fs::symlink(target, fixture.home.join("ordinary/alias")).unwrap();
            let err = fixture
                .compress(&fixture.home.join("ordinary"), format)
                .await
                .unwrap_err();
            assert!(err.contains("sensitive path denied"), "{format}: {err}");
            fixture.preserved();
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn requested102_safe_nested_links_are_omitted_and_root_links_refused() {
    for format in FORMATS {
        let fixture = Fixture::new();
        let target = fixture.home.join("ordinary/nested/hello.txt");
        std::os::unix::fs::symlink(&target, fixture.home.join("ordinary/safe-link")).unwrap();
        fixture.compress(&fixture.home, format).await.unwrap();
        let decoded = fixture.extract(format, None).await;
        assert!(
            !decoded.join("fixture-home/ordinary/safe-link").exists(),
            "{format}"
        );
        let err = fixture
            .compress(&fixture.home.join("ordinary/safe-link"), format)
            .await
            .unwrap_err();
        assert!(err.contains("regular file or directory"), "{err}");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn requested102_late_source_alias_is_refused_before_small_or_large_reads() {
    for format in FORMATS {
        for large in [false, true] {
            let fixture = Fixture::new();
            let target = fixture.home.join("ordinary/nested/hello.txt");
            if large {
                std::fs::File::create(&target)
                    .unwrap()
                    .set_len(ZIP_STORE_TRIAL_CAP + 1)
                    .unwrap();
            }
            let secret = fixture.protected();
            let swapped = AtomicBool::new(false);
            let policy = |path: &Path| {
                fixture.policy(path)?;
                // The manifest has already been admitted when the temporary
                // output is checked. Change one source before the encoder opens it.
                if path.to_str().unwrap().contains(".aerotmp")
                    && !swapped.swap(true, Ordering::SeqCst)
                {
                    std::fs::remove_file(&target).unwrap();
                    std::os::unix::fs::symlink(&secret, &target).unwrap();
                }
                Ok(())
            };
            let err = compress_archive_with_policy(
                vec![fixture.home.join("ordinary").to_str().unwrap().into()],
                fixture.output.to_str().unwrap().into(),
                format,
                None,
                Some(5),
                &policy,
            )
            .await
            .unwrap_err();
            assert!(
                err.contains("sensitive path denied"),
                "{format}, large={large}: {err}"
            );
            fixture.preserved();
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn requested102_solid_7z_rechecks_each_reader() {
    let fixture = Fixture::new();
    let target = fixture.home.join("ordinary/nested/hello.txt");
    let secret = fixture.protected();
    let swapped = AtomicBool::new(false);
    let policy = |path: &Path| {
        fixture.policy(path)?;
        if path.to_str().unwrap().contains(".aerotmp") && !swapped.swap(true, Ordering::SeqCst) {
            std::fs::remove_file(&target).unwrap();
            std::os::unix::fs::symlink(&secret, &target).unwrap();
        }
        Ok(())
    };
    let err = compress_7z_impl(
        vec![fixture.home.join("ordinary").to_str().unwrap().into()],
        fixture.output.to_str().unwrap().into(),
        None,
        Some(0),
        None,
        Some(SevenZAdvanced {
            solid: Some(true),
            ..Default::default()
        }),
        ArchiveAccess {
            app: None,
            policy: Some(&policy),
        },
    )
    .await
    .unwrap_err();
    assert!(err.contains("sensitive path denied"), "{err}");
    fixture.preserved();
}

#[cfg(unix)]
#[tokio::test]
async fn requested102_walk_errors_are_not_silently_discarded() {
    use std::os::unix::fs::PermissionsExt;
    for format in FORMATS {
        let fixture = Fixture::new();
        let locked = fixture.home.join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::write(locked.join("fixture.txt"), b"unreadable fixture").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o0)).unwrap();
        let result = fixture.compress(&fixture.home, format).await;
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
        let err = result.unwrap_err();
        assert!(err.contains("walk failed"), "{format}: {err}");
        fixture.preserved();
    }
}

#[cfg(unix)]
#[test]
fn requested102_denied_directory_is_checked_before_descent() {
    let fixture = Fixture::new();
    fixture.protected();
    let child_checked = AtomicBool::new(false);
    let denied = fixture.home.join(".ssh");
    let policy = |path: &Path| {
        if path.starts_with(&denied) && path != denied {
            child_checked.store(true, Ordering::SeqCst);
        }
        fixture.policy(path)
    };
    let access = ArchiveAccess {
        app: None,
        policy: Some(&policy),
    };
    let err = access
        .collect(&[fixture.home.to_str().unwrap().into()])
        .err()
        .unwrap();
    assert!(err.contains("sensitive path denied"), "{err}");
    assert!(!child_checked.load(Ordering::SeqCst));
}

#[cfg(unix)]
#[tokio::test]
async fn requested102_non_utf8_names_fail_without_publishing_a_lossy_archive() {
    use std::os::unix::ffi::OsStringExt;
    for format in FORMATS {
        let fixture = Fixture::new();
        let name = std::ffi::OsString::from_vec(b"invalid-\xff.txt".to_vec());
        std::fs::write(fixture.home.join(name), b"fixture").unwrap();
        let err = fixture.compress(&fixture.home, format).await.unwrap_err();
        assert!(err.contains("valid UTF-8"), "{format}: {err}");
        fixture.preserved();
    }
}
