// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! `profile-import --on-existing` on an isolated vault: re-importing a backup
//! of a profile the vault still holds skips it by default, overwrites it in
//! place under the same id, or adds a copy under a new id and the next free
//! name, and `--dry-run` reports the plan without writing it.

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
        .env(
            "AEROFTP_MASTER_PASSWORD",
            "existing profile import probe vault",
        )
        .env_remove("RUST_LOG")
        .output()
        .expect("run aeroftp-cli")
}

fn json(out: &Output) -> serde_json::Value {
    assert!(out.status.success(), "{}", describe(out));
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", describe(out)))
}

/// The saved profiles as (id, name), in order.
fn saved(config: &Path) -> Vec<(String, String)> {
    let listed = json(&cli(config, &["profiles", "--json"]));
    listed
        .get("profiles")
        .and_then(|p| p.as_array())
        .or_else(|| listed.as_array())
        .unwrap_or_else(|| panic!("no profile list in {listed}"))
        .iter()
        .map(|p| {
            (
                p["id"].as_str().unwrap_or_default().to_string(),
                p["name"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

const PASSWORD: &str = "existing profile import probe export";

fn import(config: &Path, file: &Path, extra: &[&str]) -> serde_json::Value {
    let mut args = vec![
        "profile-import",
        "-i",
        file.to_str().unwrap(),
        "--password",
        PASSWORD,
        "--json",
    ];
    args.extend_from_slice(extra);
    json(&cli(config, &args))
}

#[test]
fn a_backup_of_a_profile_still_held_is_skipped_overwritten_or_duplicated() {
    let config = tempfile::tempdir().unwrap();
    let config = config.path();
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
    let file = config.join("profiles.aeroftp");
    let exported = cli(
        config,
        &[
            "profile-export",
            "-o",
            file.to_str().unwrap(),
            "--password",
            PASSWORD,
        ],
    );
    assert!(exported.status.success(), "{}", describe(&exported));
    let original = saved(config);
    assert_eq!(original.len(), 1);
    let id = original[0].0.clone();

    let skip = import(config, &file, &[]);
    assert_eq!(skip["on_existing"], "skip", "{skip}");
    assert_eq!(skip["imported"], 0, "{skip}");
    assert_eq!(skip["skipped"], 1, "{skip}");
    assert_eq!(saved(config), original);

    let overwrite = import(config, &file, &["--on-existing", "overwrite"]);
    assert_eq!(overwrite["imported"], 1, "{overwrite}");
    assert_eq!(overwrite["overwritten"], 1, "{overwrite}");
    assert_eq!(overwrite["similar"], 0, "{overwrite}");
    assert_eq!(
        saved(config),
        original,
        "replaced in place, same id and name"
    );

    let preview = import(config, &file, &["--on-existing", "duplicate", "--dry-run"]);
    assert_eq!(preview["dry_run"], true, "{preview}");
    assert_eq!(preview["would_duplicate"], 1, "{preview}");
    assert_eq!(preview["duplicated"][0], "lab", "{preview}");
    assert_eq!(saved(config), original, "a dry run writes nothing");

    let duplicate = import(config, &file, &["--on-existing", "duplicate"]);
    assert_eq!(duplicate["duplicated"], 1, "{duplicate}");
    assert_eq!(duplicate["similar"], 1, "{duplicate}");
    assert_eq!(duplicate["renamed"][0]["to"], "lab (1)", "{duplicate}");
    let after = saved(config);
    assert_eq!(after.len(), 2);
    assert_eq!(after[0], original[0]);
    assert_eq!(after[1].1, "lab (1)");
    assert_ne!(after[1].0, id);
}
