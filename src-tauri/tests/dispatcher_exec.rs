#![cfg(unix)]

use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

struct TestDir {
    path: PathBuf,
}

/// Distinguishes two `TestDir`s created in the same process. The clock alone
/// does not: every test here builds the same name from the pid and the
/// nanosecond, and two of them reading the same nanosecond share a directory.
/// Measured on a Linux workstation, three threads sampling simultaneously:
/// 21 collisions in 200000 rounds, with a smallest observed gap of 0 ns.
///
/// A shared directory is not a cosmetic clash, because both tests then copy
/// the dispatcher to the SAME path and `Drop` removes the whole tree. It
/// produces exactly the CI failure this counter closes: `ETXTBSY` on the
/// first exec (the sibling still holds the copy open for writing), then
/// `ENOENT` twenty milliseconds later (the sibling finished and its `Drop`
/// deleted the directory out from under the retry).
static DIR_SEQ: AtomicU64 = AtomicU64::new(0);

impl TestDir {
    fn new() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let seq = DIR_SEQ.fetch_add(1, Ordering::Relaxed);
        let path = env::temp_dir().join(format!(
            "aeroftp-dispatch-test-{}-{seq}-{stamp}",
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
    // ETXTBSY ("Text file busy", errno 26): the kernel refuses to exec a file
    // that is open for writing anywhere on the system. The parallel cargo test
    // runner can transiently hold a writable fd on a just-copied dispatcher
    // binary (a sibling test's process spawn inherits the fd for the brief
    // window between fork and the close-on-exec), so a fresh exec can fail for
    // reasons that are never a real defect. A bounded retry clears it well
    // within a second.
    const ETXTBSY: i32 = 26;
    const MAX_ATTEMPTS: u32 = 50;
    for attempt in 1..=MAX_ATTEMPTS {
        let mut cmd = Command::new(dispatcher);
        cmd.arg0(arg0);
        cmd.args(args);
        match cmd.output() {
            Ok(output) => return output,
            Err(e) if e.raw_os_error() == Some(ETXTBSY) && attempt < MAX_ATTEMPTS => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(e) => panic!("dispatcher exec failed after {attempt} attempt(s): {e}"),
        }
    }
    unreachable!("retry loop returns an Output or panics")
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
fn cli_alias_without_args_shows_help_successfully() {
    let test_dir = TestDir::new();
    let dispatcher = copy_dispatcher(&test_dir.path);
    write_stub(&test_dir.path.join("aeroftp-cli"), "CLI", 0);
    write_stub(&test_dir.path.join("aeroftp.bin"), "GUI", 23);

    let output = run_dispatcher(&dispatcher, "aftp", &[]);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "CLI\nargs:[--help]\nwebkit:\n"
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

/// Asserts the invariant deterministically, which took two attempts and the
/// first one was decoration.
///
/// The first version only checked that 64 paths were distinct, with a comment
/// claiming that would catch a return to a clock-only name. Measured instead of
/// assumed, on the bench that produced this fix: 64 names drawn the old way by
/// eight threads repeat inside the batch in **0.3% of batches** (7 of 2000). A
/// guard that fires three times in a thousand does not guard anything, and
/// saying otherwise in a comment is worse than having no test, because the next
/// reader trusts it.
///
/// The second attempt asserted that the 64 draws were CONSECUTIVE, and that was
/// flaky for the very reason this file exists. `DIR_SEQ` counts for the whole
/// process, the three tests above run in parallel with this one, and a sibling
/// drawing while these 64 are in flight lands INSIDE the range: the values stay
/// distinct, the range gains a hole, and the assertion fails. Predicted from the
/// code in review by a second reader, then measured here: 8 red runs in 200 with
/// `--test-threads=8`. An assertion that depends on scheduling rather than on a
/// property is the same defect as a directory name that depends on the clock.
///
/// So the assertion is only that the 64 draws are DISTINCT, which `fetch_add`
/// guarantees whatever the siblings do. It still fails on every run against a
/// clock-only name, because `rsplit` then reads the pid out of that position and
/// all 64 are identical.
#[test]
fn test_dirs_carry_a_distinct_sequence_number() {
    const THREADS: usize = 8;
    const PER_THREAD: usize = 8;

    let dirs: Vec<TestDir> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..THREADS)
            .map(|_| scope.spawn(|| (0..PER_THREAD).map(|_| TestDir::new()).collect::<Vec<_>>()))
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("thread panicked"))
            .collect()
    });

    // `aeroftp-dispatch-test-<pid>-<seq>-<stamp>`: the sequence is the second
    // field from the end, and reading it back is what makes this deterministic.
    let seqs: Vec<u64> = dirs
        .iter()
        .map(|d| {
            let name = d.path.file_name().unwrap().to_str().unwrap();
            let seq = name.rsplit('-').nth(1).unwrap_or_default();
            seq.parse().unwrap_or_else(|_| {
                panic!("no sequence number in {name}: the directory name is back to clock only")
            })
        })
        .collect();
    let distinct_seqs: std::collections::HashSet<u64> = seqs.iter().copied().collect();
    assert_eq!(
        distinct_seqs.len(),
        seqs.len(),
        "every TestDir must carry its own DIR_SEQ draw, or a sibling's Drop can \
         delete this one's dispatcher mid-exec"
    );

    let distinct: std::collections::HashSet<_> = dirs.iter().map(|d| &d.path).collect();
    assert_eq!(distinct.len(), dirs.len(), "two TestDirs shared a path");
}
