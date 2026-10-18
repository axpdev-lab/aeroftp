// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// A child executable and portable.marker live inside each test's TempDir.
// Redirecting APPDATA alone does not isolate dirs::config_dir() on Windows:
// dirs asks the Known Folder API there. Never put a marker beside the shared
// Cargo binary, which would redirect unrelated tests into the same vault.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

static PREPARE: Mutex<()> = Mutex::new(());

/// A fixture root on the volume of the test binaries, so `executable` can
/// hard-link the CLI instead of copying it. The system temp directory is often
/// another filesystem (a tmpfs `/tmp`, or C: temp beside a D: workspace on the
/// Windows runners), where every fixture would copy the debug CLI, over a GiB,
/// and parallel tests run out of space. Integration tests get Cargo's
/// `CARGO_TARGET_TMPDIR`; a lib test, which has none, uses its own directory.
pub fn root() -> std::io::Result<tempfile::TempDir> {
    let base = match option_env!("CARGO_TARGET_TMPDIR") {
        Some(dir) => PathBuf::from(dir),
        None => std::env::current_exe()?
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(std::env::temp_dir),
    };
    std::fs::create_dir_all(&base)?;
    tempfile::Builder::new()
        .prefix("aeroftp-cli-fixture-")
        .tempdir_in(base)
}

pub fn executable(source: &Path, root: &Path) -> PathBuf {
    let _prepare = PREPARE.lock().unwrap_or_else(|e| e.into_inner());
    let dir = root.join("cli");
    std::fs::create_dir_all(&dir).expect("isolated executable directory");
    let target = dir.join(source.file_name().expect("executable name"));
    // The debug CLI can be over a GiB. A hard link gives this launch its own
    // pathname without copying its bytes. Cross-volume TempDirs use a copy.
    // Both routes leave the shared executable unchanged.
    if !target.exists() && std::fs::hard_link(source, &target).is_err() {
        std::fs::copy(source, &target).expect("copy isolated executable");
    }
    std::fs::write(dir.join("portable.marker"), b"").expect("private portable marker");
    target
}

pub fn command(source: &Path, root: &Path) -> Command {
    let mut command = Command::new(executable(source, root));
    command.current_dir(root);
    // Keep incidental home/XDG readers private too. Portable mode, not these
    // environment variables, is what isolates AeroFTP state on Windows.
    for name in [
        "HOME",
        "USERPROFILE",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_CACHE_HOME",
        "APPDATA",
        "LOCALAPPDATA",
    ] {
        command.env(name, root);
    }
    command
        .env("AEROFTP_MASTER_PASSWORD", "isolated CLI test vault")
        .env_remove("AEROFTP_MACHINE")
        .env_remove("RUST_LOG");
    command
}
