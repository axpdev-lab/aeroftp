#![cfg(unix)]

use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = env::temp_dir().join(format!(
            "aeroftp-dispatch-test-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn copy_dispatcher(test_dir: &Path) -> PathBuf {
    let src = env!("CARGO_BIN_EXE_aeroftp-dispatch");
    let dst = test_dir.join("aeroftp-dispatch");
    fs::copy(src, &dst).unwrap();
    let mut perms = fs::metadata(&dst).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&dst, perms).unwrap();
    dst
}

fn write_stub(path: &Path, name: &str, exit_code: i32) {
    fs::write(
        path,
        format!(
            "#!/bin/sh\nprintf '{name}\\n'\nprintf 'args:'\nfor arg in \"$@\"; do printf '[%s]' \"$arg\"; done\nprintf '\\n'\nprintf 'webkit:%s\\n' \"${{WEBKIT_DISABLE_DMABUF_RENDERER:-}}\"\nexit {exit_code}\n"
        ),
    )
    .unwrap();
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

fn run_dispatcher(dispatcher: &Path, arg0: &str, args: &[&str]) -> Output {
    let mut cmd = Command::new(dispatcher);
    cmd.arg0(arg0);
    cmd.args(args);
    cmd.output().unwrap()
}

#[test]
fn dispatch_execs_cli_stub_and_preserves_arguments() {
    let test_dir = TestDir::new();
    let dispatcher = copy_dispatcher(&test_dir.path);
    write_stub(&test_dir.path.join("aeroftp-cli"), "CLI", 17);
    write_stub(&test_dir.path.join("aeroftp.bin"), "GUI", 0);

    let output = run_dispatcher(&dispatcher, "aeroftp", &["ls", "space value", "\"quoted\""]);

    assert_eq!(output.status.code(), Some(17));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "CLI\nargs:[ls][space value][\"quoted\"]\nwebkit:\n"
    );
}

#[test]
fn dispatch_execs_gui_stub_with_linux_webkit_env() {
    let test_dir = TestDir::new();
    let dispatcher = copy_dispatcher(&test_dir.path);
    write_stub(&test_dir.path.join("aeroftp-cli"), "CLI", 0);
    write_stub(&test_dir.path.join("aeroftp.bin"), "GUI", 23);

    let output = run_dispatcher(&dispatcher, "aeroftp", &["--autostart"]);

    assert_eq!(output.status.code(), Some(23));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "GUI\nargs:[--autostart]\nwebkit:1\n"
    );
}
