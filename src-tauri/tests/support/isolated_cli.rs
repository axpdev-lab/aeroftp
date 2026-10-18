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

/// Cargo places integration scratch on the target volume. Lib tests have no
/// CARGO_TARGET_TMPDIR, so keep their fixture beside the test executable.
/// System temp may be a small tmpfs or a different Windows drive: putting a
/// fixture there would copy the GiB-sized binary instead of hard-linking it.
pub fn root() -> tempfile::TempDir {
    let executable = std::env::current_exe().expect("test executable");
    let parent = option_env!("CARGO_TARGET_TMPDIR")
        .map(Path::new)
        .unwrap_or_else(|| executable.parent().expect("test executable directory"));
    std::fs::create_dir_all(parent).expect("test scratch directory");
    tempfile::Builder::new()
        .prefix("aeroftp-cli-")
        .tempdir_in(parent)
        .expect("fixture on the executable volume")
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
