// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use std::path::Path;

const CHILD_ROOT: &str = "AEROFTP_RAR5_TEST_ROOT";
const HARDLINK_RAR: &[u8] =
    include_bytes!("../tests/fixtures/rar/test_read_format_rar5_hardlink.rar");
const UNICODE_RAR: &[u8] =
    include_bytes!("../tests/fixtures/rar/test_read_format_rar5_unicode.rar");

// A subprocess gives UnRAR a hostile CWD without changing the working directory
// (or C locale) of other tests running in parallel in the parent process.
#[tokio::test]
async fn rar5_hardlinks_use_the_extraction_root_not_the_process_cwd() {
    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        exercise_hardlinks(Path::new(&root)).await;
        return;
    }

    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().join("cwd");
    std::fs::create_dir(&cwd).unwrap();
    for name in ["file.txt", "👋🌎.txt"] {
        std::fs::write(cwd.join(name), b"unrelated CWD file").unwrap();
    }
    let child = crate::hidden_command(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "rar5_hardlink_tests::rar5_hardlinks_use_the_extraction_root_not_the_process_cwd",
            "--nocapture",
        ])
        .current_dir(&cwd)
        .env(CHILD_ROOT, root.path())
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    assert!(
        child.status.success(),
        "RAR5 extraction subprocess failed ({}):\n{}\n{}",
        child.status,
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr)
    );
    for name in ["file.txt", "👋🌎.txt"] {
        assert_eq!(
            std::fs::read(cwd.join(name)).unwrap(),
            b"unrelated CWD file"
        );
    }
}

async fn exercise_hardlinks(root: &Path) {
    let file_copy = reference_fixture(5, b"file.txt");
    for (case, bytes, original, link) in [
        ("ascii", HARDLINK_RAR, "file.txt", "hardlink.txt"),
        ("unicode", UNICODE_RAR, "👋🌎.txt", "Ⓗⓐⓡⓓ Ⓛⓘⓝⓚ.txt"),
        ("copy", file_copy.as_slice(), "file.txt", "hardlink.txt"),
    ] {
        let archive = root.join(format!("{case}.rar"));
        std::fs::write(&archive, bytes).unwrap();
        // Keep the destination Unicode too: the Linux workaround must preserve
        // both the archive's names and every component of the extraction root.
        let output = root.join(format!("{case}-dest-é"));
        std::fs::create_dir(&output).unwrap();
        std::fs::write(output.join(original), b"previous destination file").unwrap();
        crate::extract_rar_core(
            archive.to_string_lossy().into_owned(),
            output.to_string_lossy().into_owned(),
            None,
            false,
        )
        .await
        .unwrap_or_else(|error| panic!("{case}: {error}"));
        let contents = std::fs::read(output.join(original)).unwrap();
        assert_eq!(
            std::fs::read(output.join(link)).unwrap(),
            contents,
            "{case}"
        );
        assert_ne!(contents, b"unrelated CWD file");
        assert_ne!(contents, b"previous destination file");
        let browsed = root.join(format!("{case}-browsed-é.txt"));
        std::fs::write(&browsed, b"previous browser output").unwrap();
        crate::archive_browse::extract_rar_entry_impl(
            archive.to_string_lossy().into_owned(),
            link.to_string(),
            browsed.to_string_lossy().into_owned(),
            None,
            None,
        )
        .await
        .unwrap_or_else(|error| panic!("{case} browser: {error}"));
        assert_eq!(std::fs::read(&browsed).unwrap(), contents);
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let original = std::fs::metadata(output.join(original)).unwrap();
            let link = std::fs::metadata(output.join(link)).unwrap();
            assert_eq!(
                (original.dev(), original.ino()) == (link.dev(), link.ino()),
                case != "copy"
            );
        }
        assert!(!output.join("𝒮𝓎𝓂𝒷𝑜𝓁𝒾𝒸 𝐿𝒾𝓃𝓀.txt").exists());
        assert!(std::fs::read_dir(&output).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".aeroftp-extract-")));
    }

    // Keep all RAR5 header sizes fixed while replacing the eight-byte target.
    // Recompute only the affected header's CRC, so rejection exercises target
    // validation rather than UnRAR's corrupt-header path.
    for (case, target) in [("parent", b"../x.txt"), ("absolute", b"/tmp/foo")] {
        let bytes = reference_fixture(4, target);
        let archive = root.join(format!("{case}.rar"));
        std::fs::write(&archive, bytes).unwrap();
        let output = root.join(format!("{case}-out"));
        std::fs::create_dir(&output).unwrap();
        std::fs::write(output.join("hardlink.txt"), b"keep existing output").unwrap();
        let result = crate::extract_rar_core(
            archive.to_string_lossy().into_owned(),
            output.to_string_lossy().into_owned(),
            None,
            false,
        )
        .await;
        assert!(result.unwrap_err().contains("Unsafe RAR reference target"));
        assert_eq!(
            std::fs::read(output.join("hardlink.txt")).unwrap(),
            b"keep existing output"
        );
    }

    // Remove the fixture's first member (header + stored payload), leaving a
    // valid hardlink record whose target exists only outside this extraction.
    // Each RAR5 header has its own CRC, so the retained records are unchanged.
    let mut missing_target = HARDLINK_RAR.to_vec();
    missing_target.drain(23..63);
    let archive = root.join("missing-target.rar");
    std::fs::write(&archive, missing_target).unwrap();
    let output = root.join("missing-target-out");
    std::fs::create_dir(&output).unwrap();
    std::fs::write(output.join("file.txt"), b"unrelated destination source").unwrap();
    std::fs::write(output.join("hardlink.txt"), b"keep existing output").unwrap();
    for browse in [false, true] {
        let result = if browse {
            crate::archive_browse::extract_rar_entry_impl(
                archive.to_string_lossy().into_owned(),
                "hardlink.txt".to_string(),
                output.join("hardlink.txt").to_string_lossy().into_owned(),
                None,
                None,
            )
            .await
        } else {
            crate::extract_rar_core(
                archive.to_string_lossy().into_owned(),
                output.to_string_lossy().into_owned(),
                None,
                false,
            )
            .await
        };
        assert!(result.is_err(), "missing target accepted (browse={browse})");
        assert_eq!(
            std::fs::read(output.join("hardlink.txt")).unwrap(),
            b"keep existing output"
        );
        assert_eq!(
            std::fs::read(output.join("file.txt")).unwrap(),
            b"unrelated destination source"
        );
    }
}

fn reference_fixture(kind: u8, target: &[u8; 8]) -> Vec<u8> {
    let mut bytes = HARDLINK_RAR.to_vec();
    assert_eq!(&bytes[106..114], b"file.txt");
    assert_eq!(bytes[103], 4);
    bytes[103] = kind;
    bytes[106..114].copy_from_slice(target);
    let mut crc = flate2::Crc::new();
    crc.update(&bytes[67..114]);
    bytes[63..67].copy_from_slice(&crc.sum().to_le_bytes());
    bytes
}
