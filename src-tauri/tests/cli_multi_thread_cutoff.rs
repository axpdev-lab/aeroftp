//! R21 (#898): an invalid `--multi-thread-cutoff` is a hard usage error
//! (exit 5) that names the flag and the value, instead of silently falling
//! back to 250M. The validation lives where the cutoff is consumed: download
//! commands fail, commands that never download are unaffected even when
//! `AEROFTP_MULTI_THREAD_CUTOFF` is invalid.

use std::process::Command;

const DEAD_URL: &str = "ftp://127.0.0.1:9/object";

#[test]
fn invalid_multi_thread_cutoff_env_is_ignored_by_non_download_commands() {
    // G4: a command that never downloads must not even look at the cutoff.
    let output = Command::new(env!("CARGO_BIN_EXE_aeroftp-cli"))
        .args(["agent-info", "--json"])
        .env("AEROFTP_MULTI_THREAD_CUTOFF", "xyz")
        .output()
        .expect("spawn aeroftp-cli");
    assert_eq!(output.status.code(), Some(0), "{output:?}");
}

#[test]
fn invalid_multi_thread_cutoff_flag_is_a_usage_error() {
    let output = Command::new(env!("CARGO_BIN_EXE_aeroftp-cli"))
        .args(["--multi-thread-cutoff", "abc", "get", DEAD_URL, "./out"])
        .env_remove("AEROFTP_MULTI_THREAD_CUTOFF")
        .output()
        .expect("spawn aeroftp-cli");
    assert_eq!(output.status.code(), Some(5), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--multi-thread-cutoff") && stderr.contains("'abc'"),
        "stderr: {stderr}"
    );
    assert!(
        !stderr.contains("AEROFTP_MULTI_THREAD_CUTOFF"),
        "stderr: {stderr}"
    );
}

#[test]
fn invalid_multi_thread_cutoff_env_names_the_variable() {
    let output = Command::new(env!("CARGO_BIN_EXE_aeroftp-cli"))
        .args(["get", DEAD_URL, "./out"])
        .env("AEROFTP_MULTI_THREAD_CUTOFF", "xyz")
        .output()
        .expect("spawn aeroftp-cli");
    assert_eq!(output.status.code(), Some(5), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("AEROFTP_MULTI_THREAD_CUTOFF") && stderr.contains("'xyz'"),
        "stderr: {stderr}"
    );
}

#[test]
fn flag_wins_over_env_in_the_error_source() {
    // G5: flag and env both set (to different invalid values): the error
    // reports the flag value and must NOT blame the environment.
    let output = Command::new(env!("CARGO_BIN_EXE_aeroftp-cli"))
        .args(["--multi-thread-cutoff", "abc", "get", DEAD_URL, "./out"])
        .env("AEROFTP_MULTI_THREAD_CUTOFF", "xyz")
        .output()
        .expect("spawn aeroftp-cli");
    assert_eq!(output.status.code(), Some(5), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("'abc'"), "stderr: {stderr}");
    assert!(
        !stderr.contains("AEROFTP_MULTI_THREAD_CUTOFF"),
        "stderr: {stderr}"
    );
}
