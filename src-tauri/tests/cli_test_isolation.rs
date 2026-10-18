// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Test the executable fixture against the real CLI. A copied/linked binary
//! with a private portable.marker must put its vault inside the fixture,
//! including on Windows where dirs ignores APPDATA environment redirection.

#[path = "support/isolated_cli.rs"]
mod isolated_cli;

use std::path::Path;

#[test]
fn two_cli_fixtures_write_independent_private_vaults() {
    let source = Path::new(env!("CARGO_BIN_EXE_aeroftp-cli"));
    let root = tempfile::tempdir().unwrap();
    let leaf = if cfg!(debug_assertions) {
        "aeroftp-dev"
    } else {
        "aeroftp"
    };
    let mut keys = Vec::new();
    for name in ["first CLI", "second CLI"] {
        let fixture = root.path().join(name);
        std::fs::create_dir(&fixture).unwrap();
        let conf = fixture.join("rclone.conf");
        std::fs::write(
            &conf,
            "[Isolated fixture]\ntype = sftp\nhost = isolation.example.invalid\nuser = fixture\n",
        )
        .unwrap();
        let imported = isolated_cli::command(source, &fixture)
            .args(["import", "rclone"])
            .arg(&conf)
            .args(["--apply", "--json"])
            .output()
            .unwrap();
        assert!(
            imported.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&imported.stdout),
            String::from_utf8_lossy(&imported.stderr)
        );
        let state = fixture.join("cli/data").join(leaf);
        assert!(state.join("vault.db").is_file(), "{state:?}");
        keys.push(std::fs::read(state.join("vault.key")).unwrap());
        assert!(!fixture.join(leaf).exists());
        // Repeated commands use the same portable vault, not a fresh copy.
        let listed = isolated_cli::command(source, &fixture)
            .args(["profiles", "--json"])
            .output()
            .unwrap();
        assert!(
            listed.status.success(),
            "{}",
            String::from_utf8_lossy(&listed.stderr)
        );
        let profiles: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
        assert_eq!(profiles.as_array().unwrap().len(), 1, "{profiles}");
    }
    assert_ne!(keys[0], keys[1], "fixtures shared a vault key");
}
