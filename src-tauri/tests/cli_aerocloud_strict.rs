// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! The AeroCloud mutating CLI commands must never overwrite a config file
//! they cannot read: an unreadable `cloud_config.json` / `cloud_pairs.json`
//! exits 11 with the strict-load error and the bytes stay identical.

#[path = "support/isolated_cli.rs"]
mod isolated_cli;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn cli(root: &Path) -> Command {
    isolated_cli::command(Path::new(env!("CARGO_BIN_EXE_aeroftp-cli")), root)
}

fn data_dir(root: &Path) -> PathBuf {
    // The CLI picks its data leaf by build profile (`aeroftp-dev` in debug).
    root.join("cli")
        .join("data")
        .join(if cfg!(debug_assertions) {
            "aeroftp-dev"
        } else {
            "aeroftp"
        })
}

fn run(root: &Path, args: &[&str]) -> Output {
    cli(root).args(args).output().expect("run aeroftp-cli")
}

fn assert_exit_11_and_bytes_kept(file: &Path, bytes: &[u8], output: &Output) {
    assert_eq!(
        output.status.code(),
        Some(11),
        "expected exit 11, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read(file).unwrap(),
        bytes,
        "file must stay byte-identical"
    );
}

fn write_unparseable(root: &Path, name: &str) -> (PathBuf, Vec<u8>) {
    let dir = data_dir(root);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    let bytes = b"{ not json".to_vec();
    std::fs::write(&path, &bytes).unwrap();
    (path, bytes)
}

#[test]
fn aerocloud_enable_and_disable_refuse_an_unparseable_config() {
    let root = isolated_cli::root();
    let root_path = root.path().to_path_buf();
    let (path, bytes) = write_unparseable(&root_path, "cloud_config.json");
    for args in [
        vec!["aerocloud", "enable"],
        vec!["aerocloud", "disable"],
        vec!["aerocloud", "set", "--local", "/tmp/whatever"],
    ] {
        let out = run(&root_path, &args);
        assert_exit_11_and_bytes_kept(&path, &bytes, &out);
    }
}

#[test]
fn aerocloud_pair_mutations_refuse_unparseable_pairs() {
    let root = isolated_cli::root();
    let root_path = root.path().to_path_buf();
    let (path, bytes) = write_unparseable(&root_path, "cloud_pairs.json");
    for args in [
        vec![
            "aerocloud",
            "pair",
            "add",
            "--local",
            "/tmp/l",
            "--remote",
            "/r",
            "--profile",
            "p",
        ],
        vec!["aerocloud", "pair", "remove", "some-id"],
        vec!["aerocloud", "pair", "enable", "some-id"],
        vec!["aerocloud", "pair", "disable", "some-id"],
    ] {
        let out = run(&root_path, &args);
        assert_exit_11_and_bytes_kept(&path, &bytes, &out);
    }
}

/// The CodeRabbit scenario end to end: a mutating command that meets an
/// unreadable `cloud_config.json` must leave the credential vault AND the
/// mounts sidecar byte-identical — no migration, no partial write.
#[test]
fn aerocloud_mutations_leave_vault_and_sidecar_untouched() {
    let root = isolated_cli::root();
    let root_path = root.path().to_path_buf();

    // Let the CLI create its vault, then seed a mounts sidecar next to the
    // config files.
    let probe = run(&root_path, &["profiles", "--json"]);
    assert!(
        probe.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&probe.stderr)
    );

    fn find_under(dir: &Path, name: &str, out: &mut Vec<PathBuf>) {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    find_under(&path, name, out);
                } else if path.file_name().is_some_and(|n| n == name) {
                    out.push(path);
                }
            }
        }
    }
    let mut vaults = Vec::new();
    find_under(&root_path, "vault.db", &mut vaults);
    assert_eq!(vaults.len(), 1, "expected exactly one vault.db");
    let vault = vaults.remove(0);

    let cfg_dir = data_dir(&root_path);
    let sidecar = cfg_dir.join("mounts.json");
    let sidecar_bytes = b"{\"storage_mode\":\"sidecar\",\"mounts\":[]}".to_vec();
    std::fs::write(&sidecar, &sidecar_bytes).unwrap();

    // Unreadable config AND pairs, then every mutating command must
    // refuse before touching either store.
    let (cfg, cfg_bytes) = write_unparseable(&root_path, "cloud_config.json");
    let (pairs, pairs_bytes) = write_unparseable(&root_path, "cloud_pairs.json");
    let vault_bytes = std::fs::read(&vault).unwrap();
    for args in [
        vec!["aerocloud", "enable"],
        vec!["aerocloud", "disable"],
        vec!["aerocloud", "set", "--local", "/tmp/x"],
        vec![
            "aerocloud",
            "pair",
            "add",
            "--local",
            "/tmp/l",
            "--remote",
            "/r",
            "--profile",
            "p",
        ],
        vec!["aerocloud", "pair", "remove", "some-id"],
        vec!["aerocloud", "pair", "enable", "some-id"],
        vec!["aerocloud", "pair", "disable", "some-id"],
    ] {
        let out = run(&root_path, &args);
        assert_eq!(
            out.status.code(),
            Some(11),
            "expected exit 11 for {args:?}, stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        for (file, bytes) in [
            (&cfg, &cfg_bytes),
            (&pairs, &pairs_bytes),
            (&vault, &vault_bytes),
            (&sidecar, &sidecar_bytes),
        ] {
            assert_eq!(
                std::fs::read(file).unwrap(),
                *bytes,
                "{file:?} changed by {args:?}"
            );
        }
    }
}

#[test]
fn aerocloud_enable_still_works_when_the_config_is_absent() {
    let root = isolated_cli::root();
    let root_path = root.path().to_path_buf();
    let out = run(&root_path, &["aerocloud", "enable"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let saved = std::fs::read_to_string(data_dir(&root_path).join("cloud_config.json")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&saved).unwrap();
    assert_eq!(parsed["enabled"], true);
}
