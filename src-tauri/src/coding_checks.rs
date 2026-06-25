// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Verification runner for AeroAgent coding mode.
//!
//! Runs a small, curated, allowlisted catalog of project check commands
//! (build/test/lint/typecheck) inside a declared workspace root and returns a
//! structured pass/fail result with bounded stdout/stderr.
//!
//! This is intentionally distinct from `shell_execute`: there is no arbitrary
//! command, no shell, and the program plus its base arguments come from a fixed
//! catalog. The only caller-controlled value is an optional test-name `filter`
//! appended to filter-capable presets, and it is validated to reject argument
//! injection. Arbitrary command execution remains the job of `shell_execute`.

use serde::Serialize;
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::process::Command as TokioCommand;

use crate::ai_core::local_tools::validate_path;

/// Per-stream output cap. Compiler/test failures summarize at the end, so the
/// tail is kept when output exceeds the cap.
const MAX_CHECK_OUTPUT_BYTES: usize = 32 * 1024;
const MAX_FILTER_LEN: usize = 200;
const DEFAULT_TIMEOUT_SECS: u64 = 600;
const MIN_TIMEOUT_SECS: u64 = 5;
const MAX_TIMEOUT_SECS: u64 = 1800;

/// Minimal, safe environment forwarded to the child process.
const ENV_ALLOWLIST: &[&str] = &[
    "PATH", "HOME", "LANG", "LC_ALL", "TERM", "TMPDIR", "USER", "SHELL",
];

struct CheckPreset {
    key: &'static str,
    label: &'static str,
    program: &'static str,
    args: &'static [&'static str],
    supports_filter: bool,
}

/// Curated, allowlisted check catalog. Each entry is a fixed program plus base
/// arguments; nothing here is caller-controlled except an optional filter on
/// filter-capable entries.
const CHECK_CATALOG: &[CheckPreset] = &[
    // Type-check the Rust workspace without producing binaries.
    CheckPreset {
        key: "cargo-check",
        label: "Cargo Check",
        program: "cargo",
        args: &["check"],
        supports_filter: false,
    },
    // Build the Rust workspace.
    CheckPreset {
        key: "cargo-build",
        label: "Cargo Build",
        program: "cargo",
        args: &["build"],
        supports_filter: false,
    },
    // Run Rust tests, optionally filtered by a test-name substring.
    CheckPreset {
        key: "cargo-test",
        label: "Cargo Test",
        program: "cargo",
        args: &["test"],
        supports_filter: true,
    },
    // Lint the Rust workspace with clippy.
    CheckPreset {
        key: "cargo-clippy",
        label: "Cargo Clippy",
        program: "cargo",
        args: &["clippy"],
        supports_filter: false,
    },
    // Check Rust formatting without modifying files.
    CheckPreset {
        key: "cargo-fmt-check",
        label: "Cargo Format Check",
        program: "cargo",
        args: &["fmt", "--check"],
        supports_filter: false,
    },
    // Type-check the TypeScript project without emitting output.
    CheckPreset {
        key: "tsc",
        label: "TypeScript Check",
        program: "npx",
        args: &["--no-install", "tsc", "--noEmit", "--pretty", "false"],
        supports_filter: false,
    },
    // Run the Vitest suite once, optionally filtered by file/name.
    CheckPreset {
        key: "vitest",
        label: "Vitest",
        program: "npx",
        args: &["--no-install", "vitest", "run"],
        supports_filter: true,
    },
    // Lint the project with ESLint.
    CheckPreset {
        key: "eslint",
        label: "ESLint",
        program: "npx",
        args: &["--no-install", "eslint", "."],
        supports_filter: false,
    },
    // Run the project's npm build script.
    CheckPreset {
        key: "npm-build",
        label: "npm Build",
        program: "npm",
        args: &["run", "build"],
        supports_filter: false,
    },
];

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct CodingRunCheckResult {
    pub success: bool,
    pub workspace_root: String,
    pub check: String,
    pub label: String,
    pub program: String,
    pub args: Vec<String>,
    pub filter: Option<String>,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub timeout_secs: u64,
    pub duration_ms: u64,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

#[derive(Clone, Debug)]
pub struct CodingRunCheckRequest {
    pub workspace_root: String,
    pub check: String,
    pub filter: Option<String>,
    pub timeout_secs: Option<u64>,
}

/// Maximum checks accepted in a single verify pass.
const MAX_VERIFY_CHECKS: usize = 10;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct CodingVerifyResult {
    pub workspace_root: String,
    pub overall_success: bool,
    pub stopped_early: bool,
    pub checks: Vec<CodingRunCheckResult>,
}

#[derive(Clone, Debug)]
pub struct CodingVerifyRequest {
    pub workspace_root: String,
    pub checks: Vec<String>,
    pub continue_on_failure: bool,
    pub timeout_secs: Option<u64>,
}

fn find_preset(key: &str) -> Option<&'static CheckPreset> {
    CHECK_CATALOG.iter().find(|preset| preset.key == key)
}

/// Validate the optional test-name filter. The filter is appended as a separate
/// argv element (no shell), but a leading dash could still inject a flag into
/// the underlying tool, so reject it along with control/whitespace characters.
fn validate_filter(filter: &str) -> Result<(), String> {
    let trimmed = filter.trim();
    if trimmed.is_empty() {
        return Err("filter cannot be empty when provided".to_string());
    }
    if trimmed.len() > MAX_FILTER_LEN {
        return Err(format!("filter exceeds {MAX_FILTER_LEN} characters"));
    }
    if trimmed.starts_with('-') {
        return Err("filter cannot start with '-' (would inject a flag)".to_string());
    }
    if trimmed.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("filter cannot contain whitespace or control characters".to_string());
    }
    Ok(())
}

/// Build the full argv (base args plus an optional validated filter).
fn build_argv(preset: &CheckPreset, filter: Option<&str>) -> Result<Vec<String>, String> {
    let mut argv: Vec<String> = preset.args.iter().map(|a| a.to_string()).collect();
    if let Some(filter) = filter {
        if !preset.supports_filter {
            return Err(format!("Check '{}' does not accept a filter", preset.key));
        }
        validate_filter(filter)?;
        argv.push(filter.trim().to_string());
    }
    Ok(argv)
}

/// Keep the last `max` bytes of `bytes`, decoding lossily. Returns the string
/// plus whether truncation occurred.
fn bound_tail(bytes: &[u8], max: usize) -> (String, bool) {
    if bytes.len() <= max {
        return (String::from_utf8_lossy(bytes).to_string(), false);
    }
    let tail = &bytes[bytes.len() - max..];
    let mut out = String::from("[...truncated, showing last bytes]\n");
    out.push_str(&String::from_utf8_lossy(tail));
    (out, true)
}

/// Spawn `program` with `args` in `workspace`, capturing separated
/// stdout/stderr with a timeout. Factored out so tests can exercise the real
/// spawn/capture path with a ubiquitous program, and reused by
/// `coding_diagnostics` which needs the raw (uncapped) output to parse.
pub(crate) async fn spawn_capture(
    workspace: &std::path::Path,
    program: &str,
    args: &[String],
    timeout: Duration,
) -> Result<(Option<i32>, Vec<u8>, Vec<u8>, bool), String> {
    let mut cmd = TokioCommand::new(program);
    cmd.args(args)
        .current_dir(workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env_clear();
    for key in ENV_ALLOWLIST {
        if let Ok(val) = std::env::var(key) {
            cmd.env(key, val);
        }
    }

    let child = cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn '{program}': {e}"))?;

    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => Ok((output.status.code(), output.stdout, output.stderr, false)),
        Ok(Err(e)) => Err(format!("Process error running '{program}': {e}")),
        Err(_) => Ok((None, Vec::new(), Vec::new(), true)),
    }
}

/// Run a curated check in the given workspace root.
pub async fn run_check(req: CodingRunCheckRequest) -> Result<CodingRunCheckResult, String> {
    validate_path(&req.workspace_root, "workspace_root")?;
    let workspace = std::fs::canonicalize(&req.workspace_root)
        .map_err(|e| format!("Resolve workspace root: {e}"))?;
    if !workspace.is_dir() {
        return Err(format!(
            "Workspace root is not a directory: {}",
            req.workspace_root
        ));
    }

    let preset = find_preset(&req.check).ok_or_else(|| {
        let known: Vec<&str> = CHECK_CATALOG.iter().map(|p| p.key).collect();
        format!(
            "Unknown check '{}'. Known checks: {}",
            req.check,
            known.join(", ")
        )
    })?;

    let argv = build_argv(preset, req.filter.as_deref())?;

    let timeout_secs = req
        .timeout_secs
        .unwrap_or(DEFAULT_TIMEOUT_SECS)
        .clamp(MIN_TIMEOUT_SECS, MAX_TIMEOUT_SECS);

    let started = Instant::now();
    let (exit_code, stdout_raw, stderr_raw, timed_out) = spawn_capture(
        &workspace,
        preset.program,
        &argv,
        Duration::from_secs(timeout_secs),
    )
    .await?;
    let duration_ms = started.elapsed().as_millis() as u64;

    let (stdout, stdout_truncated) = bound_tail(&stdout_raw, MAX_CHECK_OUTPUT_BYTES);
    let (stderr, stderr_truncated) = if timed_out {
        (format!("Check timed out after {timeout_secs}s"), false)
    } else {
        bound_tail(&stderr_raw, MAX_CHECK_OUTPUT_BYTES)
    };

    let success = !timed_out && exit_code == Some(0);

    Ok(CodingRunCheckResult {
        success,
        workspace_root: workspace.to_string_lossy().to_string(),
        check: preset.key.to_string(),
        label: preset.label.to_string(),
        program: preset.program.to_string(),
        args: argv,
        filter: req.filter.map(|f| f.trim().to_string()),
        exit_code,
        timed_out,
        timeout_secs,
        duration_ms,
        stdout,
        stderr,
        stdout_truncated,
        stderr_truncated,
    })
}

/// Run an ordered list of curated checks. Stops at the first failing check
/// unless `continue_on_failure` is set. Validates the whole request up front so
/// no check runs if any check key is unknown.
pub async fn run_verify(req: CodingVerifyRequest) -> Result<CodingVerifyResult, String> {
    if req.checks.is_empty() {
        return Err("verify requires at least one check".to_string());
    }
    if req.checks.len() > MAX_VERIFY_CHECKS {
        return Err(format!("verify accepts at most {MAX_VERIFY_CHECKS} checks"));
    }
    for check in &req.checks {
        if find_preset(check).is_none() {
            let known: Vec<&str> = CHECK_CATALOG.iter().map(|p| p.key).collect();
            return Err(format!(
                "Unknown check '{}'. Known checks: {}",
                check,
                known.join(", ")
            ));
        }
    }

    let mut checks = Vec::with_capacity(req.checks.len());
    let mut overall_success = true;
    let mut stopped_early = false;

    for check in &req.checks {
        let result = run_check(CodingRunCheckRequest {
            workspace_root: req.workspace_root.clone(),
            check: check.clone(),
            filter: None,
            timeout_secs: req.timeout_secs,
        })
        .await?;
        let passed = result.success;
        checks.push(result);
        if !passed {
            overall_success = false;
            if !req.continue_on_failure {
                stopped_early = true;
                break;
            }
        }
    }

    Ok(CodingVerifyResult {
        workspace_root: req.workspace_root,
        overall_success,
        stopped_early,
        checks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_expected_presets() {
        assert!(find_preset("cargo-test").is_some());
        assert!(find_preset("tsc").is_some());
        assert!(find_preset("vitest").is_some());
        assert!(find_preset("does-not-exist").is_none());
        assert_eq!(CHECK_CATALOG.len(), 9);
    }

    #[test]
    fn build_argv_appends_filter_for_filter_capable_check() {
        let preset = find_preset("cargo-test").unwrap();
        let argv = build_argv(preset, Some("my_test")).unwrap();
        assert_eq!(argv, vec!["test".to_string(), "my_test".to_string()]);
    }

    #[test]
    fn build_argv_rejects_filter_for_unsupported_check() {
        let preset = find_preset("cargo-clippy").unwrap();
        let err = build_argv(preset, Some("anything")).unwrap_err();
        assert!(err.contains("does not accept a filter"));
    }

    #[test]
    fn validate_filter_rejects_leading_dash_and_whitespace() {
        assert!(validate_filter("--release").is_err());
        assert!(validate_filter("a b").is_err());
        assert!(validate_filter("").is_err());
        assert!(validate_filter(&"x".repeat(MAX_FILTER_LEN + 1)).is_err());
        assert!(validate_filter("module::test_name").is_ok());
        assert!(validate_filter("src/foo.test.ts").is_ok());
    }

    #[test]
    fn bound_tail_keeps_tail_and_flags_truncation() {
        let (s, truncated) = bound_tail(b"short", 32);
        assert!(!truncated);
        assert_eq!(s, "short");

        let big = vec![b'x'; 100];
        let (s, truncated) = bound_tail(&big, 10);
        assert!(truncated);
        assert!(s.contains("truncated"));
        assert!(s.ends_with("xxxxxxxxxx"));
    }

    #[tokio::test]
    async fn run_verify_rejects_empty_and_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_string_lossy().to_string();
        assert!(run_verify(CodingVerifyRequest {
            workspace_root: root.clone(),
            checks: vec![],
            continue_on_failure: false,
            timeout_secs: None,
        })
        .await
        .is_err());
        let err = run_verify(CodingVerifyRequest {
            workspace_root: root,
            checks: vec!["nope".to_string()],
            continue_on_failure: false,
            timeout_secs: None,
        })
        .await
        .unwrap_err();
        assert!(err.contains("Unknown check"));
    }

    #[tokio::test]
    async fn run_check_rejects_unknown_check() {
        let dir = tempfile::tempdir().unwrap();
        let req = CodingRunCheckRequest {
            workspace_root: dir.path().to_string_lossy().to_string(),
            check: "nope".to_string(),
            filter: None,
            timeout_secs: None,
        };
        let err = run_check(req).await.unwrap_err();
        assert!(err.contains("Unknown check"));
    }

    #[tokio::test]
    async fn spawn_capture_runs_real_program() {
        // `git --version` exists in the dev/CI environment and exits 0.
        let dir = tempfile::tempdir().unwrap();
        let (code, stdout, _stderr, timed_out) = spawn_capture(
            dir.path(),
            "git",
            &["--version".to_string()],
            Duration::from_secs(30),
        )
        .await
        .unwrap();
        assert!(!timed_out);
        assert_eq!(code, Some(0));
        assert!(String::from_utf8_lossy(&stdout).contains("git"));
    }

    #[tokio::test]
    async fn spawn_capture_times_out() {
        let dir = tempfile::tempdir().unwrap();
        let result = spawn_capture(
            dir.path(),
            "sleep",
            &["5".to_string()],
            Duration::from_millis(200),
        )
        .await
        .unwrap();
        assert!(result.3, "expected timed_out=true");
    }
}
