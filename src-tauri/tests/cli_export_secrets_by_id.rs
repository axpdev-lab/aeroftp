// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! `export <format>` writes each profile with its own secret, and never two
//! profiles under one entry name.
//!
//! The scaffold handed every exporter its secrets keyed by profile name, so
//! two profiles of one name both went out with the second one's. The map is
//! keyed by id now, and each of the four places that build the exporter's
//! input (rclone, WinSCP, FileZilla, and the bridge exporters) has to carry
//! the id for any secret to be found. A vault this build writes cannot hold
//! two profiles of one name, so the profiles here have two names that one
//! format maps onto one entry: `Lab Box` and `Lab_Box` are both the lftp
//! bookmark `Lab_Box`. On an isolated vault, profiles created by
//! `import rclone --apply`.

#[path = "support/isolated_cli.rs"]
mod isolated_cli;

use std::path::Path;
use std::process::Output;

/// `rclone obscure 'cli export secret a'` and `'cli export secret b'`
/// (rclone v1.75.2): the import stores them in the vault revealed.
const OBSCURED_A: &str = "4SFA-bRYvIkqHlD0OsThgkDH5WEHmUp_fyJHA5GBrfLfLAM";
const OBSCURED_B: &str = "lii89K2-VW8O5TezkruRn5noya9zKKsUDKCpab12i6B00-E";

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
        .env(
            "AEROFTP_MASTER_PASSWORD",
            "export secrets by id probe vault",
        )
        .env_remove("RUST_LOG")
        .output()
        .expect("run aeroftp-cli")
}

/// `(host, secret)` of what an importer read back, sorted.
fn hosts_and_secrets<'a>(
    servers: impl Iterator<Item = (&'a String, &'a Option<String>)>,
) -> Vec<(String, String)> {
    let mut out: Vec<_> = servers
        .map(|(host, secret)| (host.clone(), secret.clone().unwrap_or_default()))
        .collect();
    out.sort();
    out
}

#[test]
fn every_export_writes_each_profile_with_its_own_secret() {
    use ftp_client_gui_lib::{filezilla_import, lftp_import, rclone_import, winscp_import};

    let config = tempfile::tempdir().unwrap();
    let conf = config.path().join("rclone.conf");
    std::fs::write(
        &conf,
        format!(
            "[Lab Box]\ntype = sftp\nhost = a.example.invalid\nuser = smoke\npass = {OBSCURED_A}\n\n\
             [Lab_Box]\ntype = sftp\nhost = b.example.invalid\nuser = smoke\npass = {OBSCURED_B}\n"
        ),
    )
    .unwrap();
    let imported = cli(
        config.path(),
        &[
            "import",
            "rclone",
            conf.to_str().unwrap(),
            "--apply",
            "--json",
        ],
    );
    assert!(imported.status.success(), "{}", describe(&imported));

    let expected = vec![
        (
            "a.example.invalid".to_string(),
            "cli export secret a".to_string(),
        ),
        (
            "b.example.invalid".to_string(),
            "cli export secret b".to_string(),
        ),
    ];
    type ReadBack = fn(&Path) -> Vec<(String, String)>;
    let formats: [(&str, &str, ReadBack); 4] = [
        ("rclone", "out.conf", |p| {
            let back = rclone_import::import_rclone(p).unwrap().servers;
            hosts_and_secrets(back.iter().map(|s| (&s.host, &s.credential)))
        }),
        ("winscp", "out.ini", |p| {
            let back = winscp_import::import_winscp(p).unwrap().servers;
            hosts_and_secrets(back.iter().map(|s| (&s.host, &s.credential)))
        }),
        ("filezilla", "out.xml", |p| {
            let back = filezilla_import::import_filezilla(p).unwrap().servers;
            hosts_and_secrets(back.iter().map(|s| (&s.host, &s.credential)))
        }),
        ("lftp", "out.rc", |p| {
            let back = lftp_import::import_lftp(p).unwrap().servers;
            hosts_and_secrets(back.iter().map(|s| (&s.host, &s.credential)))
        }),
    ];
    for (format, file, read_back) in formats {
        let path = config.path().join(file);
        let out = cli(
            config.path(),
            &["export", format, "-o", path.to_str().unwrap(), "--json"],
        );
        assert_eq!(out.status.code(), Some(0), "{format}: {}", describe(&out));
        let summary: serde_json::Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{format}: {e}: {}", describe(&out)));
        assert_eq!(summary["exported"], 2, "{format}: {summary}");
        assert_eq!(read_back(&path), expected, "{format}");
    }

    // The two lftp bookmarks: one name would have been opened as the first.
    let rc = std::fs::read_to_string(config.path().join("out.rc")).unwrap();
    let names: Vec<&str> = rc
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split_whitespace().next())
        .collect();
    assert_eq!(names, ["Lab_Box", "Lab_Box-2"], "{rc}");
}
