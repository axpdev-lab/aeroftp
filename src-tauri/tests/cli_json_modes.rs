// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! `--machine` and `--format json` reach every command, not only the literal
//! `--json`. The import, export and keystore families (and profile-export /
//! profile-import) declared a `--json` of their own and read only that one:
//! under `--machine`, the mode the CLI recommends for agents, `keystore
//! status` exited 0 printing nothing, an export that selected nothing exited
//! 4 printing nothing, and the rest printed their text summary. On an isolated
//! vault, each command is run in both modes and must leave exactly one JSON
//! document on stdout.

#[path = "support/isolated_cli.rs"]
mod isolated_cli;

use std::path::Path;
use std::process::Output;

/// `rclone obscure 'json modes probe secret'`: the import stores it, so the
/// profile has a credential to export.
const OBSCURED_PASS: &str = "DAC7SvKvKUdOs0_hzKtkahYM1lQCJ2SOFqWYVFvynEwA63oo5RkW";

const MODES: [&[&str]; 2] = [&["--machine"], &["--format", "json"]];

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
    isolated_cli::command(Path::new(env!("CARGO_BIN_EXE_aeroftp-cli")), config)
        .args(args)
        .env("AEROFTP_MASTER_PASSWORD", "json modes probe vault")
        .env("AEROFTP_PROFILE_PASSWORD", "json modes probe profile file")
        .env(
            "AEROFTP_KEYSTORE_PASSWORD",
            "json modes probe keystore file",
        )
        .env_remove("AEROFTP_MACHINE")
        .env_remove("RUST_LOG")
        .output()
        .expect("run aeroftp-cli")
}

/// `args` under `mode`, which must leave exactly one JSON document on stdout
/// and exit with `code`.
fn run_json(config: &Path, mode: &[&str], args: &[&str], code: i32) -> serde_json::Value {
    let argv: Vec<&str> = mode.iter().chain(args).copied().collect();
    let out = cli(config, &argv);
    assert_eq!(
        out.status.code(),
        Some(code),
        "{argv:?}\n{}",
        describe(&out)
    );
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "{argv:?}: stdout is not one JSON document ({e})\n{}",
            describe(&out)
        )
    })
}

/// A vault holding one SFTP profile with a password, and the rclone.conf it
/// was imported from.
fn vault_with_one_profile(config: &Path) -> String {
    let conf = config.join("rclone.conf");
    std::fs::write(
        &conf,
        format!(
            "[Json Box]\ntype = sftp\nhost = json.example.invalid\nuser = smoke\npass = {OBSCURED_PASS}\n"
        ),
    )
    .unwrap();
    let conf = conf.to_str().unwrap().to_string();
    let imported = cli(config, &["import", "rclone", &conf, "--apply", "--json"]);
    assert!(imported.status.success(), "{}", describe(&imported));
    conf
}

#[test]
fn the_import_and_export_families_answer_in_json_under_machine_and_format_json() {
    let config = tempfile::tempdir().unwrap();
    let dir = config.path();
    let conf = vault_with_one_profile(dir);
    let out_conf = dir.join("out.conf");
    let out_conf = out_conf.to_str().unwrap();

    for mode in MODES {
        let preview = run_json(dir, mode, &["import", "rclone", &conf], 0);
        assert!(preview.is_object(), "{mode:?}: {preview}");

        let missing = run_json(
            dir,
            mode,
            &["import", "winscp", "/nonexistent/WinSCP.ini"],
            1,
        );
        assert!(missing.get("error").is_some(), "{mode:?}: {missing}");

        let exported = run_json(dir, mode, &["export", "rclone", "-o", out_conf], 0);
        assert_eq!(exported["exported"], 1, "{mode:?}: {exported}");

        // Exited 4 with nothing on stdout or stderr.
        let none = run_json(
            dir,
            mode,
            &[
                "export",
                "rclone",
                "--profiles",
                "no such box",
                "-o",
                out_conf,
            ],
            4,
        );
        assert_eq!(none["error"], "no exportable profiles", "{mode:?}: {none}");

        let bundle = dir.join("profiles.aeroftp");
        let bundle = bundle.to_str().unwrap();
        let saved = run_json(dir, mode, &["profile-export", "-o", bundle], 0);
        assert!(saved.is_object(), "{mode:?}: {saved}");
        let dry = run_json(dir, mode, &["profile-import", "-i", bundle, "--dry-run"], 0);
        assert!(dry.is_object(), "{mode:?}: {dry}");
    }
}

#[test]
fn the_keystore_family_answers_in_json_under_machine_and_format_json() {
    let config = tempfile::tempdir().unwrap();
    let dir = config.path();
    vault_with_one_profile(dir);
    let backup = dir.join("backup.aeroftp-keystore");
    let backup = backup.to_str().unwrap();

    for mode in MODES {
        // Exited 0 with nothing on stdout or stderr.
        let status = run_json(dir, mode, &["keystore", "status"], 0);
        assert!(status.get("mode").is_some(), "{mode:?}: {status}");

        let _ = std::fs::remove_file(backup);
        let exported = run_json(
            dir,
            mode,
            &["keystore", "export", "-o", backup, "--mode", "vault-only"],
            0,
        );
        assert!(exported.is_object(), "{mode:?}: {exported}");

        let info = run_json(dir, mode, &["keystore", "info", "-i", backup], 0);
        assert!(info.is_object(), "{mode:?}: {info}");
    }
}

#[test]
fn text_mode_stays_text() {
    let config = tempfile::tempdir().unwrap();
    let dir = config.path();
    vault_with_one_profile(dir);

    let out = cli(dir, &["keystore", "status"]);
    assert_eq!(out.status.code(), Some(0), "{}", describe(&out));
    assert!(
        serde_json::from_slice::<serde_json::Value>(&out.stdout).is_err(),
        "text mode printed JSON: {}",
        describe(&out)
    );
    assert!(
        !out.stdout.is_empty() || !out.stderr.is_empty(),
        "text mode printed nothing"
    );
}
