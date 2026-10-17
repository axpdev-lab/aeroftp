// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! An imported profile whose name a saved profile already has gets the first
//! free " (1)", " (2)", ...: two profiles of one name cannot be told apart in
//! the lists, and `--profile NAME` takes the first. Both CLI import writers
//! (`import rclone --apply` and `profile-import`) on an isolated vault.

use std::path::Path;
use std::process::{Command, Output};

fn describe(out: &Output) -> String {
    format!(
        "exit {:?}\nstdout: {}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// The CLI on the vault in `config`, with nothing of this machine's.
fn cli(config: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aeroftp-cli"))
        .args(args)
        .env("XDG_CONFIG_HOME", config)
        .env("XDG_DATA_HOME", config)
        .env("HOME", config)
        .env("APPDATA", config)
        .env("LOCALAPPDATA", config)
        .env("AEROFTP_MASTER_PASSWORD", "duplicate names probe vault")
        .env_remove("RUST_LOG")
        .output()
        .expect("run aeroftp-cli")
}

fn json(out: &Output) -> serde_json::Value {
    assert!(out.status.success(), "{}", describe(out));
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", describe(out)))
}

/// The names of the saved profiles, in order.
fn saved_names(config: &Path) -> Vec<String> {
    let listed = json(&cli(config, &["profiles", "--json"]));
    let profiles = listed
        .get("profiles")
        .and_then(|p| p.as_array())
        .or_else(|| listed.as_array())
        .unwrap_or_else(|| panic!("no profile list in {listed}"));
    profiles
        .iter()
        .map(|p| p["name"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[test]
fn an_rclone_import_of_a_taken_name_gets_the_next_free_number() {
    let config = tempfile::tempdir().unwrap();
    let conf = config.path().join("rclone.conf");
    // "lab" and "LAB" collide: the CLI selects a profile case-insensitively.
    // The import takes the remotes sorted by name, so "LAB" comes first.
    std::fs::write(
        &conf,
        "[lab]\ntype = sftp\nhost = sftp.example.invalid\nuser = smoke\n\n\
         [LAB]\ntype = sftp\nhost = other.example.invalid\nuser = smoke\n",
    )
    .unwrap();
    let conf = conf.to_str().unwrap();

    let first = json(&cli(
        config.path(),
        &["import", "rclone", conf, "--apply", "--json"],
    ));
    assert_eq!(first["appliedSummary"]["profilesAppended"], 2, "{first}");
    assert_eq!(saved_names(config.path()), ["LAB", "lab (1)"]);

    let again = json(&cli(
        config.path(),
        &["import", "rclone", conf, "--apply", "--json"],
    ));
    let renamed: Vec<(String, String)> = again["appliedSummary"]["profilesRenamed"]
        .as_array()
        .unwrap_or_else(|| panic!("no renames reported in {again}"))
        .iter()
        .map(|r| {
            (
                r["from"].as_str().unwrap().into(),
                r["to"].as_str().unwrap().into(),
            )
        })
        .collect();
    assert_eq!(
        renamed,
        [
            ("LAB".to_string(), "LAB (2)".to_string()),
            ("lab".to_string(), "lab (3)".to_string()),
        ]
    );
    assert_eq!(
        saved_names(config.path()),
        ["LAB", "lab (1)", "LAB (2)", "lab (3)"]
    );
}

#[test]
fn a_profile_file_import_of_a_taken_name_gets_the_next_free_number() {
    // Two vaults that each made a profile named "lab": the same name, two ids.
    let here = tempfile::tempdir().unwrap();
    let there = tempfile::tempdir().unwrap();
    for config in [here.path(), there.path()] {
        let conf = config.join("rclone.conf");
        std::fs::write(
            &conf,
            "[lab]\ntype = sftp\nhost = sftp.example.invalid\nuser = smoke\n",
        )
        .unwrap();
        json(&cli(
            config,
            &[
                "import",
                "rclone",
                conf.to_str().unwrap(),
                "--apply",
                "--json",
            ],
        ));
    }
    let file = there.path().join("profiles.aeroftp");
    let password = "duplicate names probe export";
    let exported = cli(
        there.path(),
        &[
            "profile-export",
            "-o",
            file.to_str().unwrap(),
            "--password",
            password,
        ],
    );
    assert!(exported.status.success(), "{}", describe(&exported));

    let imported = json(&cli(
        here.path(),
        &[
            "profile-import",
            "-i",
            file.to_str().unwrap(),
            "--password",
            password,
            "--json",
        ],
    ));
    assert_eq!(imported["imported"], 1, "{imported}");
    assert_eq!(imported["renamed"][0]["from"], "lab", "{imported}");
    assert_eq!(imported["renamed"][0]["to"], "lab (1)", "{imported}");
    assert_eq!(saved_names(here.path()), ["lab", "lab (1)"]);
}
