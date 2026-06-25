// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Ripgrep-backed structured workspace search for AeroAgent coding mode.
//!
//! Runs a fixed `rg --json` invocation inside a declared workspace root and
//! parses the machine-readable output into a flat list of structured
//! `{file, line, column, line_text, submatches}` matches. This is the read-only
//! "find this symbol/string in the codebase" tool tuned for coding work: it
//! returns file/line/column locations the agent can feed straight back into
//! `coding_git_show`, `coding_apply_patch`, or `local_edit`.
//!
//! Only the search pattern, an optional path subset, and optional glob filters
//! are caller-controlled; every other ripgrep flag is fixed. The pattern is
//! always passed via `-e` and paths/globs after a `--` separator so a leading
//! `-` can never be interpreted as a flag. Arbitrary command execution remains
//! the job of `shell_execute`.

use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;

use crate::ai_core::local_tools::validate_path;
use crate::coding_checks::spawn_capture;

/// Cap on the number of matches returned; the rest are dropped and the
/// `truncated` flag is set so the agent knows there were more.
const DEFAULT_MAX_RESULTS: usize = 200;
const MAX_MAX_RESULTS: usize = 1000;
/// Per-line cap on the number of submatches kept (highlight spans).
const MAX_SUBMATCHES_PER_LINE: usize = 16;
/// Hard cap on the length of any returned line / submatch text (chars).
const MAX_LINE_TEXT: usize = 500;
const MAX_SUBMATCH_TEXT: usize = 200;
/// Cap on caller-supplied glob filters.
const MAX_GLOBS: usize = 20;
const MAX_GLOB_LEN: usize = 200;
const MAX_PATTERN_LEN: usize = 1000;
const MAX_PATH_LEN: usize = 4096;

const DEFAULT_TIMEOUT_SECS: u64 = 60;
const MIN_TIMEOUT_SECS: u64 = 5;
const MAX_TIMEOUT_SECS: u64 = 600;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct CodingSearchSubmatch {
    /// Byte offset of the match start within the line (0-based).
    pub start: u32,
    /// Byte offset of the match end within the line (exclusive).
    pub end: u32,
    pub text: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct CodingSearchMatch {
    /// Workspace-relative path of the matching file.
    pub file: String,
    /// 1-based line number.
    pub line: u32,
    /// 1-based column of the first match on the line (byte offset + 1).
    pub column: u32,
    pub line_text: String,
    pub submatches: Vec<CodingSearchSubmatch>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct CodingSearchResult {
    pub workspace_root: String,
    pub pattern: String,
    pub path: Option<String>,
    pub globs: Vec<String>,
    pub case_insensitive: bool,
    pub fixed_strings: bool,
    pub program: String,
    pub args: Vec<String>,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub timeout_secs: u64,
    pub duration_ms: u64,
    pub total_matches: usize,
    pub file_count: usize,
    pub matches: Vec<CodingSearchMatch>,
    pub truncated: bool,
}

#[derive(Clone, Debug)]
pub struct CodingSearchRequest {
    pub workspace_root: String,
    pub pattern: String,
    pub path: Option<String>,
    pub globs: Option<Vec<String>>,
    pub case_insensitive: bool,
    pub fixed_strings: bool,
    pub max_results: Option<usize>,
    pub timeout_secs: Option<u64>,
}

/// Validate the search pattern. The pattern itself can contain almost anything
/// (it is passed via `-e`, never positionally), but reject empty/blank input,
/// null bytes, and pathological length.
fn validate_pattern(pattern: &str) -> Result<String, String> {
    if pattern.trim().is_empty() {
        return Err("'pattern' cannot be empty".to_string());
    }
    if pattern.len() > MAX_PATTERN_LEN {
        return Err(format!("'pattern' exceeds {MAX_PATTERN_LEN} characters"));
    }
    if pattern.contains('\0') {
        return Err("'pattern' contains null bytes".to_string());
    }
    Ok(pattern.to_string())
}

/// Validate caller-supplied glob filters. Globs may legitimately start with `!`
/// (ripgrep negation) and contain `*`/`?`, but reject null bytes, blank entries,
/// over-length values, and too many globs.
fn validate_globs(globs: Option<Vec<String>>) -> Result<Vec<String>, String> {
    let Some(globs) = globs else {
        return Ok(Vec::new());
    };
    if globs.len() > MAX_GLOBS {
        return Err(format!("Too many globs: {} (max {MAX_GLOBS})", globs.len()));
    }
    let mut out = Vec::with_capacity(globs.len());
    for glob in globs {
        if glob.trim().is_empty() {
            return Err("globs[]: glob is empty".to_string());
        }
        if glob.len() > MAX_GLOB_LEN {
            return Err(format!("globs[]: glob exceeds {MAX_GLOB_LEN} characters"));
        }
        if glob.contains('\0') {
            return Err("globs[]: glob contains null bytes".to_string());
        }
        out.push(glob);
    }
    Ok(out)
}

/// Resolve an optional search path subset to a clean workspace-relative string,
/// rejecting traversal outside the workspace root. `None` means "search the
/// whole workspace".
fn normalize_search_path(
    workspace_root: &Path,
    raw: Option<String>,
) -> Result<Option<String>, String> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    if raw.trim().is_empty() {
        return Ok(None);
    }
    if raw.len() > MAX_PATH_LEN {
        return Err(format!("'path' exceeds {MAX_PATH_LEN} characters"));
    }
    if raw.contains('\0') {
        return Err("'path' contains null bytes".to_string());
    }

    let path = Path::new(&raw);
    let relative = if path.is_absolute() || raw.starts_with('/') {
        validate_path(&raw, "path")?;
        let absolute =
            std::fs::canonicalize(path).map_err(|e| format!("Resolve search path: {e}"))?;
        absolute
            .strip_prefix(workspace_root)
            .map_err(|_| format!("Path is outside workspace root: {raw}"))?
            .to_path_buf()
    } else {
        normalize_relative_path(path)?
    };

    let display = relative
        .to_string_lossy()
        .replace('\\', "/")
        .trim_matches('/')
        .to_string();
    if display.is_empty() || display == "." {
        return Ok(None);
    }

    if !workspace_root.join(&display).exists() {
        return Err(format!("Search path does not exist: {display}"));
    }
    Ok(Some(display))
}

fn normalize_relative_path(path: &Path) -> Result<PathBuf, String> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err("'path': path traversal ('..') not allowed".to_string())
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err("'path': absolute path could not be normalized".to_string())
            }
        }
    }
    Ok(out)
}

fn truncate_text(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    value.chars().take(max_chars).collect()
}

/// Parse one ripgrep `--json` "match" line into a structured match. Returns
/// `None` for any other message type (begin/end/context/summary) or malformed
/// input.
fn parse_match_line(line: &str) -> Option<CodingSearchMatch> {
    let value: Value = serde_json::from_str(line.trim()).ok()?;
    if value.get("type").and_then(Value::as_str) != Some("match") {
        return None;
    }
    let data = value.get("data")?;

    let path = data.get("path")?.get("text")?.as_str()?;
    let file = path.strip_prefix("./").unwrap_or(path).to_string();
    if file.is_empty() {
        return None;
    }

    let line_number = data.get("line_number").and_then(Value::as_u64)? as u32;

    let line_text_raw = data
        .get("lines")
        .and_then(|lines| lines.get("text"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let line_text = truncate_text(line_text_raw.trim_end_matches(['\n', '\r']), MAX_LINE_TEXT);

    let mut submatches = Vec::new();
    if let Some(items) = data.get("submatches").and_then(Value::as_array) {
        for item in items.iter().take(MAX_SUBMATCHES_PER_LINE) {
            let start = item.get("start").and_then(Value::as_u64);
            let end = item.get("end").and_then(Value::as_u64);
            let (Some(start), Some(end)) = (start, end) else {
                continue;
            };
            let text = item
                .get("match")
                .and_then(|m| m.get("text"))
                .and_then(Value::as_str)
                .unwrap_or("");
            submatches.push(CodingSearchSubmatch {
                start: start as u32,
                end: end as u32,
                text: truncate_text(text, MAX_SUBMATCH_TEXT),
            });
        }
    }

    let column = submatches.first().map(|s| s.start + 1).unwrap_or(1);

    Some(CodingSearchMatch {
        file,
        line: line_number,
        column,
        line_text,
        submatches,
    })
}

fn parse_matches(stdout: &str) -> Vec<CodingSearchMatch> {
    stdout.lines().filter_map(parse_match_line).collect()
}

fn count_distinct_files(matches: &[CodingSearchMatch]) -> usize {
    let mut files: Vec<&str> = matches.iter().map(|m| m.file.as_str()).collect();
    files.sort_unstable();
    files.dedup();
    files.len()
}

fn build_args(
    pattern: &str,
    globs: &[String],
    path: &Option<String>,
    case_insensitive: bool,
    fixed_strings: bool,
) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "--json".to_string(),
        "--color=never".to_string(),
        "--no-config".to_string(),
        "--max-columns=4096".to_string(),
    ];
    if case_insensitive {
        args.push("-i".to_string());
    }
    if fixed_strings {
        args.push("-F".to_string());
    }
    for glob in globs {
        args.push("-g".to_string());
        args.push(glob.clone());
    }
    args.push("-e".to_string());
    args.push(pattern.to_string());
    // Everything after `--` is a positional search path, never a flag.
    args.push("--".to_string());
    args.push(path.clone().unwrap_or_else(|| ".".to_string()));
    args
}

/// Run a ripgrep search inside a coding workspace and parse the matches.
pub async fn run_search(req: CodingSearchRequest) -> Result<CodingSearchResult, String> {
    validate_path(&req.workspace_root, "workspace_root")?;
    let workspace = std::fs::canonicalize(&req.workspace_root)
        .map_err(|e| format!("Resolve workspace root: {e}"))?;
    if !workspace.is_dir() {
        return Err(format!(
            "Workspace root is not a directory: {}",
            req.workspace_root
        ));
    }

    let pattern = validate_pattern(&req.pattern)?;
    let globs = validate_globs(req.globs)?;
    let path = normalize_search_path(&workspace, req.path)?;

    let max_results = req
        .max_results
        .unwrap_or(DEFAULT_MAX_RESULTS)
        .clamp(1, MAX_MAX_RESULTS);
    let timeout_secs = req
        .timeout_secs
        .unwrap_or(DEFAULT_TIMEOUT_SECS)
        .clamp(MIN_TIMEOUT_SECS, MAX_TIMEOUT_SECS);

    let args = build_args(
        &pattern,
        &globs,
        &path,
        req.case_insensitive,
        req.fixed_strings,
    );

    let started = Instant::now();
    let (exit_code, stdout_raw, stderr_raw, timed_out) =
        spawn_capture(&workspace, "rg", &args, Duration::from_secs(timeout_secs))
            .await
            .map_err(|e| {
                if e.contains("Failed to spawn") {
                    "ripgrep (rg) is not installed or not found in PATH".to_string()
                } else {
                    e
                }
            })?;
    let duration_ms = started.elapsed().as_millis() as u64;

    // ripgrep exit codes: 0 = matches found, 1 = no matches, >=2 = error.
    if let Some(code) = exit_code {
        if code >= 2 {
            let stderr = String::from_utf8_lossy(&stderr_raw);
            return Err(format!("ripgrep failed (exit {code}): {}", stderr.trim()));
        }
    }

    let mut matches = if timed_out {
        Vec::new()
    } else {
        let stdout = String::from_utf8_lossy(&stdout_raw);
        parse_matches(&stdout)
    };

    let total = matches.len();
    let truncated = total > max_results;
    if truncated {
        matches.truncate(max_results);
    }
    let file_count = count_distinct_files(&matches);

    Ok(CodingSearchResult {
        workspace_root: workspace.to_string_lossy().to_string(),
        pattern,
        path,
        globs,
        case_insensitive: req.case_insensitive,
        fixed_strings: req.fixed_strings,
        program: "rg".to_string(),
        args,
        exit_code,
        timed_out,
        timeout_secs,
        duration_ms,
        total_matches: matches.len(),
        file_count,
        matches,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_pattern_rejects_blank_and_null() {
        assert!(validate_pattern("   ").is_err());
        assert!(validate_pattern("foo\0bar").is_err());
        assert_eq!(validate_pattern("fn main").unwrap(), "fn main");
        // A leading '-' is fine because the pattern is passed via -e.
        assert_eq!(validate_pattern("--flag").unwrap(), "--flag");
    }

    #[test]
    fn validate_globs_enforces_caps_and_allows_negation() {
        assert!(validate_globs(None).unwrap().is_empty());
        assert_eq!(
            validate_globs(Some(vec!["*.rs".to_string(), "!target/*".to_string()])).unwrap(),
            vec!["*.rs".to_string(), "!target/*".to_string()]
        );
        assert!(validate_globs(Some(vec!["bad\0".to_string()])).is_err());
        let too_many: Vec<String> = (0..MAX_GLOBS + 1).map(|i| format!("*.{i}")).collect();
        assert!(validate_globs(Some(too_many)).is_err());
    }

    #[test]
    fn normalize_search_path_rejects_traversal_and_outside() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir(root.join("src")).unwrap();

        assert_eq!(normalize_search_path(&root, None).unwrap(), None);
        assert_eq!(
            normalize_search_path(&root, Some("src".to_string())).unwrap(),
            Some("src".to_string())
        );
        // "." collapses to whole-workspace search.
        assert_eq!(
            normalize_search_path(&root, Some(".".to_string())).unwrap(),
            None
        );
        assert!(normalize_search_path(&root, Some("../etc".to_string())).is_err());
        assert!(normalize_search_path(&root, Some("missing".to_string())).is_err());
    }

    #[test]
    fn parse_match_line_extracts_location_and_submatches() {
        let line = r#"{"type":"match","data":{"path":{"text":"./src/lib.rs"},"lines":{"text":"    let value = compute();\n"},"line_number":42,"absolute_offset":900,"submatches":[{"match":{"text":"value"},"start":8,"end":13}]}}"#;
        let m = parse_match_line(line).unwrap();
        assert_eq!(m.file, "src/lib.rs");
        assert_eq!(m.line, 42);
        assert_eq!(m.column, 9);
        assert_eq!(m.line_text, "    let value = compute();");
        assert_eq!(m.submatches.len(), 1);
        assert_eq!(m.submatches[0].text, "value");
        assert_eq!(m.submatches[0].start, 8);
    }

    #[test]
    fn parse_match_line_skips_non_match_types() {
        assert!(parse_match_line(r#"{"type":"begin","data":{"path":{"text":"a.rs"}}}"#).is_none());
        assert!(parse_match_line(r#"{"type":"end","data":{}}"#).is_none());
        assert!(parse_match_line(r#"{"type":"summary","data":{}}"#).is_none());
        assert!(parse_match_line("not json").is_none());
    }

    #[test]
    fn parse_match_line_handles_missing_line_text() {
        // When --max-columns is exceeded, rg may omit lines.text; we default it.
        let line = r#"{"type":"match","data":{"path":{"text":"big.min.js"},"line_number":1,"submatches":[{"match":{"text":"x"},"start":0,"end":1}]}}"#;
        let m = parse_match_line(line).unwrap();
        assert_eq!(m.file, "big.min.js");
        assert_eq!(m.line_text, "");
        assert_eq!(m.column, 1);
    }

    #[test]
    fn count_distinct_files_dedups() {
        let matches = vec![
            CodingSearchMatch {
                file: "a.rs".to_string(),
                line: 1,
                column: 1,
                line_text: String::new(),
                submatches: Vec::new(),
            },
            CodingSearchMatch {
                file: "a.rs".to_string(),
                line: 2,
                column: 1,
                line_text: String::new(),
                submatches: Vec::new(),
            },
            CodingSearchMatch {
                file: "b.rs".to_string(),
                line: 1,
                column: 1,
                line_text: String::new(),
                submatches: Vec::new(),
            },
        ];
        assert_eq!(count_distinct_files(&matches), 2);
    }

    #[test]
    fn build_args_orders_flags_pattern_and_path() {
        let args = build_args(
            "needle",
            &["*.rs".to_string()],
            &Some("src".to_string()),
            true,
            true,
        );
        assert!(args.contains(&"--json".to_string()));
        assert!(args.contains(&"-i".to_string()));
        assert!(args.contains(&"-F".to_string()));
        // Pattern is passed via -e, path after --.
        let e_pos = args.iter().position(|a| a == "-e").unwrap();
        assert_eq!(args[e_pos + 1], "needle");
        let sep = args.iter().position(|a| a == "--").unwrap();
        assert_eq!(args[sep + 1], "src");
        assert!(e_pos < sep);
    }

    #[tokio::test]
    async fn run_search_rejects_empty_pattern() {
        let dir = tempfile::tempdir().unwrap();
        let err = run_search(CodingSearchRequest {
            workspace_root: dir.path().to_string_lossy().to_string(),
            pattern: "  ".to_string(),
            path: None,
            globs: None,
            case_insensitive: false,
            fixed_strings: false,
            max_results: None,
            timeout_secs: None,
        })
        .await
        .unwrap_err();
        assert!(err.contains("pattern"));
    }

    #[tokio::test]
    async fn run_search_finds_matches_when_rg_available() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("sample.rs"),
            "fn alpha() {}\nfn beta() {}\nfn alpha_two() {}\n",
        )
        .unwrap();

        let result = run_search(CodingSearchRequest {
            workspace_root: dir.path().to_string_lossy().to_string(),
            pattern: "alpha".to_string(),
            path: None,
            globs: None,
            case_insensitive: false,
            fixed_strings: false,
            max_results: None,
            timeout_secs: Some(30),
        })
        .await;

        let result = match result {
            Ok(result) => result,
            // Skip when ripgrep is not installed in the test environment.
            Err(e) if e.contains("not installed") => return,
            Err(e) => panic!("unexpected error: {e}"),
        };

        assert_eq!(result.file_count, 1);
        assert_eq!(result.total_matches, 2);
        assert!(result.matches.iter().all(|m| m.file == "sample.rs"));
        assert!(result.matches.iter().any(|m| m.line == 1));
    }
}
