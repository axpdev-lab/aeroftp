//! A command stand-in for integration tests, run through the checked-in
//! `tests/fixtures/script_stub.sh` so no test writes a file and then executes
//! it (ETXTBSY under the parallel suite; see that file). The crate's unit tests
//! use the same fixture through `util::script_stub`.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// Make `path` a command that runs the shell script `body`: a symlink to the
/// checked-in stub, with the script beside it as `<path>.body`.
pub fn fake_tool(path: impl AsRef<Path>, body: impl AsRef<str>) -> PathBuf {
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
