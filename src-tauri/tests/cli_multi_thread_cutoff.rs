//! R21 (#898): an invalid `--multi-thread-cutoff` is a hard usage error
//! (exit 5) that names the flag and the value, instead of silently falling
//! back to 250M. The same rule covers a value injected through
//! `AEROFTP_MULTI_THREAD_CUTOFF`, and the error names the variable.

use std::process::Command;

#[test]
fn invalid_multi_thread_cutoff_flag_is_a_usage_error() {
    let output = Command::new(env!("CARGO_BIN_EXE_aeroftp-cli"))
        .args(["--multi-thread-cutoff", "abc", "agent-info", "--json"])
        .env_remove("AEROFTP_MULTI_THREAD_CUTOFF")
        .output()
        .expect("spawn aeroftp-cli");
    assert_eq!(output.status.code(), Some(5), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--multi-thread-cutoff") && stderr.contains("'abc'"),
        "stderr: {stderr}"
    );
}

#[test]
fn invalid_multi_thread_cutoff_env_names_the_variable() {
    let output = Command::new(env!("CARGO_BIN_EXE_aeroftp-cli"))
        .args(["agent-info", "--json"])
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
