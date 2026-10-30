// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! `aeroftp-cli size` on a local folder, end to end: the command as a script
//! runs it, and the totals it prints.

#[path = "support/isolated_cli.rs"]
mod isolated_cli;

use std::path::Path;

/// `size <folder> <path>` measures the folder named inside the one given.
/// The folder given holds more than the one named, so a dispatch that sized
/// the wrong one, or left the local walk, prints other totals.
#[test]
fn size_prints_the_totals_of_the_local_folder_named_inside_the_one_given() {
    let config = isolated_cli::root();
    let data = tempfile::tempdir().expect("data dir");
    std::fs::create_dir_all(data.path().join("docs/sub")).expect("mkdir");
    std::fs::write(data.path().join("docs/top.bin"), [0u8; 1000]).expect("write");
    std::fs::write(data.path().join("docs/sub/a.bin"), [0u8; 7]).expect("write");
    std::fs::write(data.path().join("docs/sub/b.bin"), [0u8; 5]).expect("write");

    let out = isolated_cli::command(Path::new(env!("CARGO_BIN_EXE_aeroftp-cli")), config.path())
        .args(["--format", "json", "size"])
        .arg(data.path())
        .arg("/docs/sub")
        .output()
        .expect("run aeroftp-cli");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("one JSON document");
    assert_eq!(json["bytes"], 12, "{json}");
    assert_eq!(json["count"], 2, "{json}");
    assert_eq!(json["scan_method"], "parallel local walk", "{json}");
    // Named as the provider walk names it, from the folder given: a script
    // passes it on to the next command with the same folder.
    assert_eq!(json["path"], "/docs/sub", "{json}");
}

/// The path is printed as given, as the provider walk prints it: whatever its
/// characters, and relative when given relative.
#[test]
fn size_names_the_folder_as_the_provider_walk_does() {
    let config = isolated_cli::root();
    let data = tempfile::tempdir().expect("data dir");
    std::fs::create_dir_all(data.path().join("docs/é x#%")).expect("mkdir");
    std::fs::write(data.path().join("docs/é x#%/c.bin"), [0u8; 3]).expect("write");
    // A name of spaces is a folder, not an omitted path: it was printed as
    // `/`, and the provider walk measured the whole root.
    std::fs::create_dir_all(data.path().join(" ")).expect("mkdir");
    std::fs::write(data.path().join(" /d.bin"), [0u8; 5]).expect("write");

    // `--max-depth` leaves the measure to the provider walk.
    let size = |given: &str, depth: Option<&str>| -> serde_json::Value {
        let mut command =
            isolated_cli::command(Path::new(env!("CARGO_BIN_EXE_aeroftp-cli")), config.path());
        command.args(["--format", "json"]);
        if let Some(depth) = depth {
            command.args(["--max-depth", depth]);
        }
        let out = command
            .arg("size")
            .arg(data.path())
            .arg(given)
            .output()
            .expect("run aeroftp-cli");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert_eq!(
            out.status.code(),
            Some(0),
            "stdout: {stdout}\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_str(&stdout).expect("one JSON document")
    };
    for (given, printed, bytes) in [
        ("/docs/é x#%", "/docs/é x#%", 3),
        ("docs/é x#%", "docs/é x#%", 3),
        (" ", " ", 5),
        ("/ ", "/ ", 5),
    ] {
        let local = size(given, None);
        assert_eq!(local["scan_method"], "parallel local walk", "{local}");
        assert_eq!(local["bytes"], bytes, "{local}");
        assert_eq!(local["path"], printed, "{local}");
        let provider = size(given, Some("10"));
        assert_ne!(provider["scan_method"], "parallel local walk", "{provider}");
        assert_eq!(provider["bytes"], bytes, "{provider}");
        assert_eq!(provider["path"], local["path"], "{provider}");
    }
}

/// `--profile` with a URL is refused, as by every command, before anything is
/// measured: a local folder's URL is no exception.
#[test]
fn size_refuses_a_profile_given_with_a_local_folder_url() {
    let config = isolated_cli::root();
    let data = tempfile::tempdir().expect("data dir");
    std::fs::create_dir_all(data.path().join("docs")).expect("mkdir");
    let url = url::Url::from_directory_path(data.path()).expect("a file URL");

    let out = isolated_cli::command(Path::new(env!("CARGO_BIN_EXE_aeroftp-cli")), config.path())
        .args(["--format", "json", "--profile", "Some Server", "size"])
        .arg(url.as_str())
        .arg("/docs")
        .output()
        .expect("run aeroftp-cli");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(5),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        format!("{stdout}{stderr}").contains("Cannot specify both --profile and URL"),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(!stdout.contains("\"bytes\""), "measured anyway: {stdout}");
}
