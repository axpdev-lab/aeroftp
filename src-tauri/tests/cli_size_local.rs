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
}
