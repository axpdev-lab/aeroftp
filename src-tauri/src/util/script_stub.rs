//! A command stand-in for tests, run through a checked-in executable so no
//! test writes a file and then executes it (see `tests/fixtures/script_stub.sh`).

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// Make `path` a command that runs the shell script `body`: a symlink to the
/// checked-in stub, with the script beside it as `<path>.body`. Panics when
/// the stub lost its executable bit, so a broken checkout fails here and not
/// as a confusing command error.
pub(crate) fn install(path: impl AsRef<Path>, body: impl AsRef<str>) -> PathBuf {
    let path = path.as_ref();
    let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/script_stub.sh");
    let mode = std::fs::metadata(&target)
        .unwrap_or_else(|e| panic!("stub fixture missing at {}: {e}", target.display()))
        .permissions()
        .mode();
    assert!(
        mode & 0o111 != 0,
        "stub fixture {} lost its executable bit (mode {mode:o})",
        target.display()
    );
    let mut script = path.as_os_str().to_owned();
    script.push(".body");
    std::fs::write(&script, body.as_ref()).unwrap();
    std::os::unix::fs::symlink(&target, path).unwrap();
    path.to_path_buf()
}
