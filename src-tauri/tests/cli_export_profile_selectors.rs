// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! `export <format> --profiles` selects each profile the way `--profile`
//! does: by id, or by name in any case. It compared every selector with the
//! exact, case-sensitive name, so `--profiles <id>` answered "no exportable
//! profiles" with nothing skipped although the profile was there. A selector
//! that selects nothing is now listed with the skipped profiles, with the
//! reason. On an isolated vault, profiles created by `import rclone --apply`.

use std::path::Path;
use std::process::{Command, Output};

/// `rclone obscure 'export probe secret'`: the import stores it in the vault,
/// so the profiles have a credential to export.
const OBSCURED_PASS: &str = "0oQwNwS7QfVAbzvWYH-HtonAjrKVCfRbJ3qamIntIGyvXOc";

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
        .env("AEROFTP_MASTER_PASSWORD", "export selectors probe vault")
        .env_remove("AEROFTP_MACHINE")
        .env_remove("RUST_LOG")
        .output()
        .expect("run aeroftp-cli")
}

fn stdout_json(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", describe(out)))
}

/// A vault holding two SFTP profiles with a password each, "Lab Box" and
/// "Other Box", and their ids by name.
fn vault_with_two_profiles(config: &Path) -> (String, String) {
    let conf = config.join("rclone.conf");
    std::fs::write(
        &conf,
        format!(
            "[Lab Box]\ntype = sftp\nhost = lab.example.invalid\nuser = smoke\npass = {OBSCURED_PASS}\n\n\
             [Other Box]\ntype = sftp\nhost = other.example.invalid\nuser = smoke\npass = {OBSCURED_PASS}\n"
        ),
    )
    .unwrap();
    let imported = cli(
        config,
        &[
            "import",
            "rclone",
            conf.to_str().unwrap(),
            "--apply",
            "--json",
        ],
    );
    assert!(imported.status.success(), "{}", describe(&imported));

    let listed = cli(config, &["profiles", "--json"]);
    assert!(listed.status.success(), "{}", describe(&listed));
    let listed = stdout_json(&listed);
    let profiles = listed
        .get("profiles")
        .and_then(|p| p.as_array())
        .or_else(|| listed.as_array())
        .unwrap_or_else(|| panic!("no profile list in {listed}"));
    let id_of = |name: &str| {
        profiles
            .iter()
            .find(|p| p["name"] == name)
            .and_then(|p| p["id"].as_str())
            .unwrap_or_else(|| panic!("no profile {name:?} in {listed}"))
            .to_string()
    };
    (id_of("Lab Box"), id_of("Other Box"))
}

/// `export <format> --profiles <selectors> -o <out> --json`.
fn export(config: &Path, format: &str, selectors: &str, out: &Path) -> Output {
    cli(
        config,
        &[
            "export",
            format,
            "--profiles",
            selectors,
            "-o",
            out.to_str().unwrap(),
            "--json",
        ],
    )
}

fn skipped(summary: &serde_json::Value) -> Vec<(String, String)> {
    summary["skipped"]
        .as_array()
        .unwrap_or_else(|| panic!("no skipped list in {summary}"))
        .iter()
        .map(|pair| {
            (
                pair[0].as_str().unwrap().to_string(),
                pair[1].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

#[test]
fn every_export_format_selects_a_profile_by_its_id() {
    let config = tempfile::tempdir().unwrap();
    let (lab, _) = vault_with_two_profiles(config.path());

    // One subcommand per caller of the shared scaffold: rclone, WinSCP,
    // FileZilla, and the generic bridge exporter.
    for (format, file) in [
        ("rclone", "out.conf"),
        ("winscp", "out.ini"),
        ("filezilla", "out.xml"),
        ("ssh", "ssh_config"),
    ] {
        let path = config.path().join(file);
        let out = export(config.path(), format, &lab, &path);
        assert_eq!(out.status.code(), Some(0), "{format}: {}", describe(&out));
        let summary = stdout_json(&out);
        assert_eq!(summary["exported"], 1, "{format}: {summary}");
        assert_eq!(skipped(&summary), [], "{format}: {summary}");
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(
            written.contains("lab.example.invalid") && !written.contains("other.example.invalid"),
            "{format} did not write exactly the profile {lab}:\n{written}"
        );
    }
}

#[test]
fn a_name_in_another_case_selects_its_profile() {
    let config = tempfile::tempdir().unwrap();
    vault_with_two_profiles(config.path());
    let path = config.path().join("out.conf");

    let out = export(config.path(), "rclone", "lab box", &path);
    assert_eq!(out.status.code(), Some(0), "{}", describe(&out));
    let summary = stdout_json(&out);
    assert_eq!(summary["exported"], 1, "{summary}");
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(written.contains("[Lab Box]"), "{written}");
    assert!(!written.contains("[Other Box]"), "{written}");
}

#[test]
fn a_selector_that_selects_nothing_is_reported_and_the_rest_exports() {
    let config = tempfile::tempdir().unwrap();
    let (_, other) = vault_with_two_profiles(config.path());
    let path = config.path().join("out.conf");

    let out = export(
        config.path(),
        "rclone",
        &format!("{other},no such box"),
        &path,
    );
    assert_eq!(out.status.code(), Some(0), "{}", describe(&out));
    let summary = stdout_json(&out);
    assert_eq!(summary["exported"], 1, "{summary}");
    assert_eq!(
        skipped(&summary),
        [(
            "no such box".to_string(),
            "no profile matches 'no such box'".to_string()
        )]
    );
    assert!(std::fs::read_to_string(&path)
        .unwrap()
        .contains("[Other Box]"));
}

#[test]
fn selectors_that_select_nothing_are_listed_not_an_empty_skipped() {
    let config = tempfile::tempdir().unwrap();
    vault_with_two_profiles(config.path());
    let path = config.path().join("out.conf");

    // The observed defect: an unknown id answered `"skipped": []`.
    let out = export(config.path(), "rclone", "srv_0_unknown,9", &path);
    assert_eq!(out.status.code(), Some(4), "{}", describe(&out));
    let summary = stdout_json(&out);
    assert_eq!(summary["error"], "no exportable profiles", "{summary}");
    let reported: Vec<String> = skipped(&summary).into_iter().map(|(s, _)| s).collect();
    assert_eq!(reported, ["srv_0_unknown", "9"], "{summary}");
    assert!(!path.exists(), "a file was written with nothing selected");

    // So does `--machine`, the mode the CLI recommends for agents.
    let out = cli(
        config.path(),
        &[
            "--machine",
            "export",
            "rclone",
            "--profiles",
            "srv_0_unknown",
            "-o",
            path.to_str().unwrap(),
        ],
    );
    assert_eq!(out.status.code(), Some(4), "{}", describe(&out));
    let summary = stdout_json(&out);
    let reported: Vec<String> = skipped(&summary).into_iter().map(|(s, _)| s).collect();
    assert_eq!(reported, ["srv_0_unknown"], "{summary}");

    // Text mode says the same on stderr.
    let out = cli(
        config.path(),
        &[
            "export",
            "rclone",
            "--profiles",
            "srv_0_unknown",
            "-o",
            path.to_str().unwrap(),
        ],
    );
    assert_eq!(out.status.code(), Some(4), "{}", describe(&out));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("srv_0_unknown") && stderr.contains("no profile matches"),
        "{}",
        describe(&out)
    );
}
