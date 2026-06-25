// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Structured compiler/typechecker diagnostics for AeroAgent coding mode.
//!
//! Runs a fixed, allowlisted diagnostic command (`cargo check
//! --message-format=json`, `tsc --noEmit`, or `eslint -f json`) inside a
//! declared workspace root and parses the machine-readable output into a flat
//! list of structured `{file, line, column, severity, code, message}` entries.
//!
//! This is a read-only inspection tool: it never modifies the workspace and the
//! program plus its arguments come from a fixed catalog (the only
//! caller-controlled value is the workspace root and an optional timeout).
//! Arbitrary command execution remains the job of `shell_execute`.

use std::path::Path;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use regex::Regex;
use serde::Serialize;
use serde_json::Value;

use crate::ai_core::local_tools::validate_path;
use crate::coding_checks::spawn_capture;

/// Cap on the number of diagnostics returned; the rest are dropped and the
/// `truncated` flag is set so the agent knows there were more.
const MAX_DIAGNOSTICS: usize = 500;
const DEFAULT_TIMEOUT_SECS: u64 = 600;
const MIN_TIMEOUT_SECS: u64 = 5;
const MAX_TIMEOUT_SECS: u64 = 1800;

/// `file(line,col): severity TS####: message`, with the location prefix
/// optional (tsc emits a few project-level errors without a location).
static TSC_LINE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:(.+?)\((\d+),(\d+)\): )?(error|warning) (TS\d+): (.*)$")
        .expect("static tsc diagnostic regex is valid")
});

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct CodingDiagnostic {
    pub file: Option<String>,
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub severity: String,
    pub code: Option<String>,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct CodingDiagnosticsResult {
    pub workspace_root: String,
    pub source: String,
    pub program: String,
    pub args: Vec<String>,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub timeout_secs: u64,
    pub duration_ms: u64,
    /// True when the run completed and produced no error-level diagnostics.
    pub success: bool,
    pub error_count: usize,
    pub warning_count: usize,
    pub diagnostics: Vec<CodingDiagnostic>,
    pub truncated: bool,
}

#[derive(Clone, Debug)]
pub struct CodingDiagnosticsRequest {
    pub workspace_root: String,
    pub source: String,
    pub timeout_secs: Option<u64>,
}

struct DiagnosticsPreset {
    source: &'static str,
    program: &'static str,
    args: &'static [&'static str],
}

/// Allowlisted diagnostic commands. Each entry is a fixed program plus base
/// arguments; nothing here is caller-controlled.
const DIAGNOSTICS_CATALOG: &[DiagnosticsPreset] = &[
    DiagnosticsPreset {
        source: "cargo",
        program: "cargo",
        args: &["check", "--message-format=json"],
    },
    DiagnosticsPreset {
        source: "tsc",
        program: "npx",
        args: &["--no-install", "tsc", "--noEmit", "--pretty", "false"],
    },
    DiagnosticsPreset {
        source: "eslint",
        program: "npx",
        args: &["--no-install", "eslint", "-f", "json", "."],
    },
];

fn find_preset(source: &str) -> Option<&'static DiagnosticsPreset> {
    DIAGNOSTICS_CATALOG
        .iter()
        .find(|preset| preset.source == source)
}

/// Ordered list of curated diagnostics source keys. Single source of truth for
/// the tool schema enum and the validation allowlist, so adding a source to
/// `DIAGNOSTICS_CATALOG` propagates everywhere without duplicate lists.
pub(crate) fn diagnostics_sources() -> Vec<&'static str> {
    DIAGNOSTICS_CATALOG
        .iter()
        .map(|preset| preset.source)
        .collect()
}

/// Parse one cargo `compiler-message` JSON line into a diagnostic. Returns
/// `None` for non-diagnostic messages, summary lines without spans, and levels
/// other than error/warning.
fn parse_cargo_line(line: &str) -> Option<CodingDiagnostic> {
    let value: Value = serde_json::from_str(line.trim()).ok()?;
    if value.get("reason").and_then(|r| r.as_str()) != Some("compiler-message") {
        return None;
    }
    let message = value.get("message")?;

    let level = message.get("level").and_then(|l| l.as_str())?;
    let severity = match level {
        "error" => "error",
        "warning" => "warning",
        _ => return None,
    };

    // Drop summary lines such as "aborting due to N previous errors" which
    // carry no span and only restate the count.
    let spans = message.get("spans").and_then(|s| s.as_array());
    let primary = spans.and_then(|spans| {
        spans
            .iter()
            .find(|span| span.get("is_primary").and_then(|p| p.as_bool()) == Some(true))
            .or_else(|| spans.first())
    })?;

    let file = primary
        .get("file_name")
        .and_then(|f| f.as_str())
        .map(str::to_string);
    let line = primary
        .get("line_start")
        .and_then(|l| l.as_u64())
        .map(|l| l as u32);
    let column = primary
        .get("column_start")
        .and_then(|c| c.as_u64())
        .map(|c| c as u32);

    let code = message
        .get("code")
        .and_then(|c| c.get("code"))
        .and_then(|c| c.as_str())
        .map(str::to_string);

    let text = message
        .get("message")
        .and_then(|m| m.as_str())
        .unwrap_or_default()
        .to_string();

    Some(CodingDiagnostic {
        file,
        line,
        column,
        severity: severity.to_string(),
        code,
        message: text,
    })
}

fn parse_cargo_diagnostics(stdout: &str) -> Vec<CodingDiagnostic> {
    stdout.lines().filter_map(parse_cargo_line).collect()
}

fn parse_tsc_line(line: &str) -> Option<CodingDiagnostic> {
    let caps = TSC_LINE_RE.captures(line.trim_end())?;
    let file = caps.get(1).map(|m| m.as_str().to_string());
    let line_no = caps.get(2).and_then(|m| m.as_str().parse::<u32>().ok());
    let column = caps.get(3).and_then(|m| m.as_str().parse::<u32>().ok());
    let severity = caps.get(4)?.as_str().to_string();
    let code = caps.get(5).map(|m| m.as_str().to_string());
    let message = caps
        .get(6)
        .map(|m| m.as_str().to_string())
        .unwrap_or_default();
    Some(CodingDiagnostic {
        file,
        line: line_no,
        column,
        severity,
        code,
        message,
    })
}

fn parse_tsc_diagnostics(output: &str) -> Vec<CodingDiagnostic> {
    output.lines().filter_map(parse_tsc_line).collect()
}

/// Make an eslint absolute `filePath` workspace-relative (forward slashes);
/// fall back to the original string when it is not under the workspace.
fn relativize_path(path: &str, workspace: &Path) -> String {
    Path::new(path)
        .strip_prefix(workspace)
        .map(|rel| rel.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| path.to_string())
}

/// Parse `eslint -f json` output (an array of per-file results) into structured
/// diagnostics. eslint severity 2 = error, 1 = warning; `ruleId` maps to `code`.
/// Non-JSON output (e.g. a fatal config error) yields an empty list, matching
/// the tsc fallback behavior.
fn parse_eslint_diagnostics(stdout: &str, workspace: &Path) -> Vec<CodingDiagnostic> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
        return Vec::new();
    };
    let Some(files) = value.as_array() else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for file in files {
        let file_rel = file
            .get("filePath")
            .and_then(|p| p.as_str())
            .map(|p| relativize_path(p, workspace));
        let Some(messages) = file.get("messages").and_then(|m| m.as_array()) else {
            continue;
        };
        for message in messages {
            let severity = match message.get("severity").and_then(|s| s.as_u64()) {
                Some(2) => "error",
                Some(1) => "warning",
                _ => continue,
            };
            let line = message
                .get("line")
                .and_then(|l| l.as_u64())
                .map(|l| l as u32);
            let column = message
                .get("column")
                .and_then(|c| c.as_u64())
                .map(|c| c as u32);
            let code = message
                .get("ruleId")
                .and_then(|r| r.as_str())
                .map(str::to_string);
            let text = message
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or_default()
                .to_string();
            out.push(CodingDiagnostic {
                file: file_rel.clone(),
                line,
                column,
                severity: severity.to_string(),
                code,
                message: text,
            });
        }
    }
    out
}

/// Run a curated diagnostic command and parse its output into structured
/// diagnostics.
pub async fn run_diagnostics(
    req: CodingDiagnosticsRequest,
) -> Result<CodingDiagnosticsResult, String> {
    validate_path(&req.workspace_root, "workspace_root")?;
    let workspace = std::fs::canonicalize(&req.workspace_root)
        .map_err(|e| format!("Resolve workspace root: {e}"))?;
    if !workspace.is_dir() {
        return Err(format!(
            "Workspace root is not a directory: {}",
            req.workspace_root
        ));
    }

    let preset = find_preset(&req.source).ok_or_else(|| {
        let known: Vec<&str> = DIAGNOSTICS_CATALOG.iter().map(|p| p.source).collect();
        format!(
            "Unknown diagnostics source '{}'. Known sources: {}",
            req.source,
            known.join(", ")
        )
    })?;

    let argv: Vec<String> = preset.args.iter().map(|a| a.to_string()).collect();

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

    let mut diagnostics = if timed_out {
        Vec::new()
    } else {
        let stdout = String::from_utf8_lossy(&stdout_raw);
        match preset.source {
            "cargo" => parse_cargo_diagnostics(&stdout),
            "tsc" => {
                // tsc writes to stdout, but be tolerant of stderr too.
                let stderr = String::from_utf8_lossy(&stderr_raw);
                let mut out = parse_tsc_diagnostics(&stdout);
                out.extend(parse_tsc_diagnostics(&stderr));
                out
            }
            "eslint" => parse_eslint_diagnostics(&stdout, &workspace),
            _ => Vec::new(),
        }
    };

    let total = diagnostics.len();
    let truncated = total > MAX_DIAGNOSTICS;
    if truncated {
        diagnostics.truncate(MAX_DIAGNOSTICS);
    }

    let error_count = diagnostics.iter().filter(|d| d.severity == "error").count();
    let warning_count = diagnostics
        .iter()
        .filter(|d| d.severity == "warning")
        .count();
    let success = !timed_out && error_count == 0;

    Ok(CodingDiagnosticsResult {
        workspace_root: workspace.to_string_lossy().to_string(),
        source: preset.source.to_string(),
        program: preset.program.to_string(),
        args: argv,
        exit_code,
        timed_out,
        timeout_secs,
        duration_ms,
        success,
        error_count,
        warning_count,
        diagnostics,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_cargo_tsc_and_eslint() {
        assert!(find_preset("cargo").is_some());
        assert!(find_preset("tsc").is_some());
        assert!(find_preset("eslint").is_some());
        assert!(find_preset("nope").is_none());
        // The accessor used by the schema/validation must list every source.
        assert_eq!(diagnostics_sources(), vec!["cargo", "tsc", "eslint"]);
    }

    #[test]
    fn parse_eslint_extracts_errors_and_warnings() {
        let workspace = Path::new("/repo");
        let stdout = r#"[
            {"filePath":"/repo/src/a.ts","messages":[
                {"ruleId":"no-unused-vars","severity":2,"line":3,"column":5,"message":"'x' is defined but never used."},
                {"ruleId":"eqeqeq","severity":1,"line":7,"column":9,"message":"Expected '===' and instead saw '=='."}
            ]},
            {"filePath":"/repo/src/b.ts","messages":[
                {"ruleId":null,"severity":2,"line":1,"column":1,"message":"Parsing error: Unexpected token"}
            ]}
        ]"#;
        let diags = parse_eslint_diagnostics(stdout, workspace);
        assert_eq!(diags.len(), 3);
        assert_eq!(diags[0].file.as_deref(), Some("src/a.ts"));
        assert_eq!(diags[0].severity, "error");
        assert_eq!(diags[0].code.as_deref(), Some("no-unused-vars"));
        assert_eq!(diags[0].line, Some(3));
        assert_eq!(diags[0].column, Some(5));
        assert_eq!(diags[1].severity, "warning");
        // ruleId null -> no code, but still reported.
        assert_eq!(diags[2].file.as_deref(), Some("src/b.ts"));
        assert!(diags[2].code.is_none());
    }

    #[test]
    fn parse_eslint_handles_empty_and_non_json() {
        let workspace = Path::new("/repo");
        // Clean run: empty array, no diagnostics.
        assert!(parse_eslint_diagnostics("[]", workspace).is_empty());
        assert!(parse_eslint_diagnostics("   ", workspace).is_empty());
        // Fatal config error (non-JSON) degrades to empty, like tsc.
        assert!(parse_eslint_diagnostics("Oops something went wrong", workspace).is_empty());
        // severity 0 (off) is skipped.
        let off = r#"[{"filePath":"/repo/a.ts","messages":[{"ruleId":"x","severity":0,"line":1,"column":1,"message":"off"}]}]"#;
        assert!(parse_eslint_diagnostics(off, workspace).is_empty());
    }

    #[test]
    fn relativize_path_strips_workspace_or_keeps_absolute() {
        let workspace = Path::new("/repo");
        assert_eq!(relativize_path("/repo/src/a.ts", workspace), "src/a.ts");
        // Outside the workspace: kept as-is.
        assert_eq!(relativize_path("/other/x.ts", workspace), "/other/x.ts");
    }

    #[test]
    fn parse_cargo_line_extracts_primary_span() {
        let line = r#"{"reason":"compiler-message","message":{"level":"error","message":"unresolved import `foo`","code":{"code":"E0432"},"spans":[{"file_name":"src/lib.rs","line_start":12,"column_start":5,"is_primary":true},{"file_name":"src/other.rs","line_start":1,"column_start":1,"is_primary":false}]}}"#;
        let diag = parse_cargo_line(line).unwrap();
        assert_eq!(diag.severity, "error");
        assert_eq!(diag.code.as_deref(), Some("E0432"));
        assert_eq!(diag.file.as_deref(), Some("src/lib.rs"));
        assert_eq!(diag.line, Some(12));
        assert_eq!(diag.column, Some(5));
        assert_eq!(diag.message, "unresolved import `foo`");
    }

    #[test]
    fn parse_cargo_line_skips_non_diagnostic_and_summary() {
        // Not a compiler message.
        assert!(parse_cargo_line(r#"{"reason":"build-finished","success":false}"#).is_none());
        // Error level but no spans (summary line).
        let summary = r#"{"reason":"compiler-message","message":{"level":"error","message":"aborting due to previous error","code":null,"spans":[]}}"#;
        assert!(parse_cargo_line(summary).is_none());
        // Note level is dropped.
        let note = r#"{"reason":"compiler-message","message":{"level":"note","message":"note text","code":null,"spans":[{"file_name":"a.rs","line_start":1,"column_start":1,"is_primary":true}]}}"#;
        assert!(parse_cargo_line(note).is_none());
        // Garbage line.
        assert!(parse_cargo_line("not json").is_none());
    }

    #[test]
    fn parse_cargo_diagnostics_counts_levels() {
        let stdout = [
            r#"{"reason":"compiler-message","message":{"level":"warning","message":"unused variable","code":{"code":"unused_variables"},"spans":[{"file_name":"src/a.rs","line_start":3,"column_start":9,"is_primary":true}]}}"#,
            r#"{"reason":"compiler-message","message":{"level":"error","message":"mismatched types","code":{"code":"E0308"},"spans":[{"file_name":"src/b.rs","line_start":7,"column_start":1,"is_primary":true}]}}"#,
            r#"{"reason":"build-finished","success":false}"#,
        ]
        .join("\n");
        let diags = parse_cargo_diagnostics(&stdout);
        assert_eq!(diags.len(), 2);
        assert_eq!(diags[0].severity, "warning");
        assert_eq!(diags[1].code.as_deref(), Some("E0308"));
    }

    #[test]
    fn parse_tsc_line_with_and_without_location() {
        let with =
            parse_tsc_line("src/foo.ts(12,5): error TS2304: Cannot find name 'bar'.").unwrap();
        assert_eq!(with.file.as_deref(), Some("src/foo.ts"));
        assert_eq!(with.line, Some(12));
        assert_eq!(with.column, Some(5));
        assert_eq!(with.severity, "error");
        assert_eq!(with.code.as_deref(), Some("TS2304"));
        assert_eq!(with.message, "Cannot find name 'bar'.");

        let without =
            parse_tsc_line("error TS18003: No inputs were found in config file.").unwrap();
        assert!(without.file.is_none());
        assert!(without.line.is_none());
        assert_eq!(without.code.as_deref(), Some("TS18003"));

        // Continuation/unrelated lines do not match.
        assert!(parse_tsc_line("  Found 3 errors.").is_none());
        assert!(parse_tsc_line("").is_none());
    }

    #[test]
    fn parse_tsc_diagnostics_collects_all() {
        let output = [
            "src/a.ts(1,1): error TS1005: ';' expected.",
            "src/b.ts(2,2): warning TS6133: 'x' is declared but never used.",
            "Found 2 errors in 2 files.",
        ]
        .join("\n");
        let diags = parse_tsc_diagnostics(&output);
        assert_eq!(diags.len(), 2);
        assert_eq!(diags[0].severity, "error");
        assert_eq!(diags[1].severity, "warning");
    }

    #[tokio::test]
    async fn run_diagnostics_rejects_unknown_source() {
        let dir = tempfile::tempdir().unwrap();
        let err = run_diagnostics(CodingDiagnosticsRequest {
            workspace_root: dir.path().to_string_lossy().to_string(),
            source: "nope".to_string(),
            timeout_secs: None,
        })
        .await
        .unwrap_err();
        assert!(err.contains("Unknown diagnostics source"));
    }
}
