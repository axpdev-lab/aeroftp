// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Git-specialized coding-agent operations.

use serde::Serialize;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use crate::ai_core::local_tools::validate_path;

const MAX_GIT_PATHS: usize = 100;
const DEFAULT_DIFF_BYTES: usize = 64 * 1024;
const MAX_DIFF_BYTES: usize = 256 * 1024;
const DEFAULT_LOG_COUNT: usize = 20;
const MAX_LOG_COUNT: usize = 200;
const MAX_GIT_REF_LEN: usize = 200;
const LOG_FORMAT: &str = "%H%x1f%h%x1f%an%x1f%aI%x1f%s";

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct CodingGitFileStatus {
    pub path: String,
    pub index_status: String,
    pub worktree_status: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct CodingGitStatusResult {
    pub workspace_root: String,
    pub repo_root: String,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub upstream: Option<String>,
    pub ahead: i64,
    pub behind: i64,
    pub clean: bool,
    pub staged: Vec<CodingGitFileStatus>,
    pub unstaged: Vec<CodingGitFileStatus>,
    pub untracked: Vec<CodingGitFileStatus>,
    pub conflicted: Vec<CodingGitFileStatus>,
    pub total: usize,
    pub truncated: bool,
    pub raw: Vec<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct CodingGitDiffStat {
    pub path: String,
    pub additions: u64,
    pub deletions: u64,
    pub binary: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct CodingGitDiffResult {
    pub workspace_root: String,
    pub repo_root: String,
    pub staged: bool,
    pub paths: Vec<String>,
    pub file_count: usize,
    pub total_additions: u64,
    pub total_deletions: u64,
    pub stats: Vec<CodingGitDiffStat>,
    pub diff: String,
    pub truncated: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct CodingGitStageResult {
    pub success: bool,
    pub workspace_root: String,
    pub repo_root: String,
    pub dry_run: bool,
    pub staged: bool,
    pub paths: Vec<String>,
    pub before: CodingGitStatusResult,
    pub after: Option<CodingGitStatusResult>,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct CodingGitCommitResult {
    pub success: bool,
    pub workspace_root: String,
    pub repo_root: String,
    pub dry_run: bool,
    pub committed: bool,
    pub commit_hash: Option<String>,
    pub message: String,
    pub stdout: String,
    pub stderr: String,
    pub before: CodingGitStatusResult,
    pub after: Option<CodingGitStatusResult>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct CodingGitLogEntry {
    pub hash: String,
    pub short_hash: String,
    pub author: String,
    pub date: String,
    pub subject: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct CodingGitLogResult {
    pub workspace_root: String,
    pub repo_root: String,
    pub paths: Vec<String>,
    pub max_count: usize,
    pub commits: Vec<CodingGitLogEntry>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct CodingGitShowResult {
    pub workspace_root: String,
    pub repo_root: String,
    pub commit: String,
    pub hash: String,
    pub short_hash: String,
    pub author: String,
    pub date: String,
    pub subject: String,
    pub body: String,
    pub stats: Vec<CodingGitDiffStat>,
    pub total_additions: u64,
    pub total_deletions: u64,
    pub diff: String,
    pub truncated: bool,
}

#[derive(Clone, Debug)]
pub struct CodingGitLogRequest {
    pub workspace_root: String,
    pub paths: Option<Vec<String>>,
    pub max_count: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct CodingGitShowRequest {
    pub workspace_root: String,
    pub commit: String,
    pub max_bytes: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct CodingGitDiffRequest {
    pub workspace_root: String,
    pub paths: Option<Vec<String>>,
    pub staged: bool,
    pub max_bytes: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct CodingGitStageRequest {
    pub workspace_root: String,
    pub paths: Vec<String>,
    pub dry_run: bool,
}

#[derive(Clone, Debug)]
pub struct CodingGitCommitRequest {
    pub workspace_root: String,
    pub message: String,
    pub dry_run: bool,
}

struct GitWorkspace {
    workspace_root: PathBuf,
    repo_root: PathBuf,
}

pub fn git_status(
    workspace_root: String,
    paths: Option<Vec<String>>,
) -> Result<CodingGitStatusResult, String> {
    let workspace = validate_git_workspace(&workspace_root)?;
    let pathspecs = normalize_optional_paths(&workspace.workspace_root, paths)?;
    git_status_for_workspace(&workspace, &pathspecs)
}

pub fn git_diff(req: CodingGitDiffRequest) -> Result<CodingGitDiffResult, String> {
    let workspace = validate_git_workspace(&req.workspace_root)?;
    let pathspecs = normalize_optional_paths(&workspace.workspace_root, req.paths)?;
    let max_bytes = req
        .max_bytes
        .unwrap_or(DEFAULT_DIFF_BYTES)
        .min(MAX_DIFF_BYTES);

    let mut numstat_args = literal_git_args(["diff", "--numstat", "--no-ext-diff", "--no-color"]);
    if req.staged {
        numstat_args.push("--cached".into());
    }
    append_pathspecs(&mut numstat_args, &pathspecs);
    let numstat = run_git_stdout(&workspace.workspace_root, &numstat_args)?;
    let stats = parse_numstat(&numstat);
    let file_count = stats.len();
    let total_additions = stats.iter().map(|stat| stat.additions).sum();
    let total_deletions = stats.iter().map(|stat| stat.deletions).sum();

    let mut diff_args = literal_git_args(["diff", "--patch", "--no-ext-diff", "--no-color"]);
    if req.staged {
        diff_args.push("--cached".into());
    }
    append_pathspecs(&mut diff_args, &pathspecs);
    let diff_full = run_git_stdout(&workspace.workspace_root, &diff_args)?;
    let (diff, truncated) = truncate_chars(&diff_full, max_bytes);

    Ok(CodingGitDiffResult {
        workspace_root: path_to_string(&workspace.workspace_root),
        repo_root: path_to_string(&workspace.repo_root),
        staged: req.staged,
        paths: pathspecs,
        file_count,
        total_additions,
        total_deletions,
        stats,
        diff,
        truncated,
    })
}

pub fn git_stage(req: CodingGitStageRequest) -> Result<CodingGitStageResult, String> {
    let workspace = validate_git_workspace(&req.workspace_root)?;
    let paths = normalize_required_paths(&workspace.workspace_root, req.paths)?;
    let before = git_status_for_workspace(&workspace, &paths)?;

    if req.dry_run {
        return Ok(CodingGitStageResult {
            success: true,
            workspace_root: path_to_string(&workspace.workspace_root),
            repo_root: path_to_string(&workspace.repo_root),
            dry_run: true,
            staged: false,
            paths,
            before,
            after: None,
            message: "Git stage dry run completed; index was not changed.".to_string(),
        });
    }

    let mut add_args = literal_git_args(["add"]);
    append_pathspecs(&mut add_args, &paths);
    run_git_stdout(&workspace.workspace_root, &add_args)?;
    let after = git_status_for_workspace(&workspace, &paths)?;

    Ok(CodingGitStageResult {
        success: true,
        workspace_root: path_to_string(&workspace.workspace_root),
        repo_root: path_to_string(&workspace.repo_root),
        dry_run: false,
        staged: true,
        paths,
        before,
        after: Some(after),
        message: "Git index updated for the requested path(s).".to_string(),
    })
}

pub fn git_commit(req: CodingGitCommitRequest) -> Result<CodingGitCommitResult, String> {
    let workspace = validate_git_workspace(&req.workspace_root)?;
    let message = validate_commit_message(&req.message)?;
    let before = git_status_for_workspace(&workspace, &[])?;

    if !before.conflicted.is_empty() {
        return Ok(CodingGitCommitResult {
            success: false,
            workspace_root: path_to_string(&workspace.workspace_root),
            repo_root: path_to_string(&workspace.repo_root),
            dry_run: req.dry_run,
            committed: false,
            commit_hash: None,
            message: "Cannot commit while git has conflicted paths.".to_string(),
            stdout: String::new(),
            stderr: String::new(),
            before,
            after: None,
        });
    }

    if before.staged.is_empty() {
        return Ok(CodingGitCommitResult {
            success: false,
            workspace_root: path_to_string(&workspace.workspace_root),
            repo_root: path_to_string(&workspace.repo_root),
            dry_run: req.dry_run,
            committed: false,
            commit_hash: None,
            message: "No staged changes to commit. Use coding_git_stage first.".to_string(),
            stdout: String::new(),
            stderr: String::new(),
            before,
            after: None,
        });
    }

    if req.dry_run {
        return Ok(CodingGitCommitResult {
            success: true,
            workspace_root: path_to_string(&workspace.workspace_root),
            repo_root: path_to_string(&workspace.repo_root),
            dry_run: true,
            committed: false,
            commit_hash: None,
            message: "Git commit dry run completed; commit was not created.".to_string(),
            stdout: String::new(),
            stderr: String::new(),
            before,
            after: None,
        });
    }

    let output = run_git_output(
        &workspace.workspace_root,
        &[
            OsString::from("commit"),
            OsString::from("-m"),
            OsString::from(message.clone()),
        ],
    )?;

    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Ok(CodingGitCommitResult {
            success: false,
            workspace_root: path_to_string(&workspace.workspace_root),
            repo_root: path_to_string(&workspace.repo_root),
            dry_run: false,
            committed: false,
            commit_hash: None,
            message: format!(
                "Git commit failed with exit code {}.",
                exit_code(&output.status)
            ),
            stdout,
            stderr,
            before,
            after: None,
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let commit_hash = run_git_stdout(
        &workspace.workspace_root,
        &str_git_args(["rev-parse", "HEAD"]),
    )
    .ok()
    .map(|hash| hash.trim().to_string())
    .filter(|hash| !hash.is_empty());
    let after = git_status_for_workspace(&workspace, &[])?;

    Ok(CodingGitCommitResult {
        success: true,
        workspace_root: path_to_string(&workspace.workspace_root),
        repo_root: path_to_string(&workspace.repo_root),
        dry_run: false,
        committed: true,
        commit_hash,
        message,
        stdout,
        stderr,
        before,
        after: Some(after),
    })
}

pub fn git_log(req: CodingGitLogRequest) -> Result<CodingGitLogResult, String> {
    let workspace = validate_git_workspace(&req.workspace_root)?;
    let paths = normalize_optional_paths(&workspace.workspace_root, req.paths)?;
    let max_count = req
        .max_count
        .unwrap_or(DEFAULT_LOG_COUNT)
        .clamp(1, MAX_LOG_COUNT);

    let mut args = literal_git_args(["log", "--no-color"]);
    args.push(format!("--max-count={}", max_count + 1).into());
    args.push(format!("--pretty=format:{LOG_FORMAT}").into());
    append_pathspecs(&mut args, &paths);

    let stdout = run_git_stdout(&workspace.workspace_root, &args)?;
    let mut commits = parse_log(&stdout);
    let truncated = commits.len() > max_count;
    commits.truncate(max_count);

    Ok(CodingGitLogResult {
        workspace_root: path_to_string(&workspace.workspace_root),
        repo_root: path_to_string(&workspace.repo_root),
        paths,
        max_count,
        commits,
        truncated,
    })
}

pub fn git_show(req: CodingGitShowRequest) -> Result<CodingGitShowResult, String> {
    let workspace = validate_git_workspace(&req.workspace_root)?;
    let commit = validate_git_ref(&req.commit)?;
    let max_bytes = req
        .max_bytes
        .unwrap_or(DEFAULT_DIFF_BYTES)
        .min(MAX_DIFF_BYTES);

    // Metadata (suppress diff); fields separated by unit separator.
    let meta_raw = run_git_stdout(
        &workspace.workspace_root,
        &str_git_args([
            "show",
            "-s",
            "--no-color",
            "--pretty=format:%H%x1f%h%x1f%an%x1f%aI%x1f%s%x1f%b",
            &commit,
        ]),
    )?;
    let fields: Vec<&str> = meta_raw.split('\u{1f}').collect();
    if fields.len() < 6 {
        return Err(format!("Unexpected git show metadata for '{commit}'"));
    }

    let numstat = run_git_stdout(
        &workspace.workspace_root,
        &str_git_args(["show", "--numstat", "--no-color", "--format=", &commit]),
    )?;
    let stats = parse_numstat(&numstat);
    let total_additions = stats.iter().map(|stat| stat.additions).sum();
    let total_deletions = stats.iter().map(|stat| stat.deletions).sum();

    let patch_raw = run_git_stdout(
        &workspace.workspace_root,
        &str_git_args([
            "show",
            "--patch",
            "--no-color",
            "--no-ext-diff",
            "--format=",
            &commit,
        ]),
    )?;
    let (diff, truncated) = truncate_chars(patch_raw.trim_start_matches('\n'), max_bytes);

    Ok(CodingGitShowResult {
        workspace_root: path_to_string(&workspace.workspace_root),
        repo_root: path_to_string(&workspace.repo_root),
        commit,
        hash: fields[0].trim().to_string(),
        short_hash: fields[1].trim().to_string(),
        author: fields[2].to_string(),
        date: fields[3].trim().to_string(),
        subject: fields[4].to_string(),
        body: fields[5].trim_end().to_string(),
        stats,
        total_additions,
        total_deletions,
        diff,
        truncated,
    })
}

fn parse_log(stdout: &str) -> Vec<CodingGitLogEntry> {
    stdout
        .lines()
        .filter(|line| !line.is_empty())
        .filter_map(|line| {
            let fields: Vec<&str> = line.split('\u{1f}').collect();
            if fields.len() < 5 {
                return None;
            }
            Some(CodingGitLogEntry {
                hash: fields[0].to_string(),
                short_hash: fields[1].to_string(),
                author: fields[2].to_string(),
                date: fields[3].to_string(),
                subject: fields[4].to_string(),
            })
        })
        .collect()
}

/// Validate a git commit-ish reference. Refs may contain '/', '~', '^', etc.,
/// but a leading '-' could inject a flag, so reject it along with control
/// characters, whitespace, and over-length input.
fn validate_git_ref(reference: &str) -> Result<String, String> {
    let trimmed = reference.trim();
    if trimmed.is_empty() {
        return Err("commit reference cannot be empty".to_string());
    }
    if trimmed.len() > MAX_GIT_REF_LEN {
        return Err(format!(
            "commit reference exceeds {MAX_GIT_REF_LEN} characters"
        ));
    }
    if trimmed.starts_with('-') {
        return Err("commit reference cannot start with '-'".to_string());
    }
    if trimmed.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("commit reference cannot contain whitespace or control characters".to_string());
    }
    Ok(trimmed.to_string())
}

fn validate_git_workspace(root: &str) -> Result<GitWorkspace, String> {
    validate_path(root, "workspace_root")?;
    let workspace_root =
        std::fs::canonicalize(root).map_err(|e| format!("Resolve workspace root: {e}"))?;
    if !workspace_root.is_dir() {
        return Err(format!(
            "Workspace root is not a directory: {}",
            workspace_root.display()
        ));
    }

    let repo_root_raw = run_git_stdout(
        &workspace_root,
        &str_git_args(["rev-parse", "--show-toplevel"]),
    )?;
    let repo_root = std::fs::canonicalize(repo_root_raw.trim())
        .map_err(|e| format!("Resolve git repository root: {e}"))?;

    Ok(GitWorkspace {
        workspace_root,
        repo_root,
    })
}

fn git_status_for_workspace(
    workspace: &GitWorkspace,
    paths: &[String],
) -> Result<CodingGitStatusResult, String> {
    let mut args = literal_git_args([
        "status",
        "--porcelain=v1",
        "--branch",
        "--untracked-files=all",
        "--no-renames",
    ]);
    append_pathspecs(&mut args, paths);
    let status = run_git_stdout(&workspace.workspace_root, &args)?;
    let head = run_git_stdout(
        &workspace.workspace_root,
        &str_git_args(["rev-parse", "--short", "HEAD"]),
    )
    .ok()
    .map(|value| value.trim().to_string())
    .filter(|value| !value.is_empty());
    Ok(parse_status(
        &status,
        path_to_string(&workspace.workspace_root),
        path_to_string(&workspace.repo_root),
        head,
    ))
}

fn parse_status(
    status: &str,
    workspace_root: String,
    repo_root: String,
    head: Option<String>,
) -> CodingGitStatusResult {
    let mut branch = None;
    let mut upstream = None;
    let mut ahead = 0;
    let mut behind = 0;
    let mut staged = Vec::new();
    let mut unstaged = Vec::new();
    let mut untracked = Vec::new();
    let mut conflicted = Vec::new();
    let mut raw = Vec::new();
    let mut total = 0;

    for line in status.lines().filter(|line| !line.is_empty()) {
        if let Some(branch_line) = line.strip_prefix("## ") {
            let parsed = parse_branch_line(branch_line);
            branch = parsed.0;
            upstream = parsed.1;
            ahead = parsed.2;
            behind = parsed.3;
            continue;
        }

        raw.push(line.to_string());
        total += 1;
        if line.len() < 3 {
            continue;
        }

        let mut chars = line.chars();
        let index = chars.next().unwrap_or(' ');
        let worktree = chars.next().unwrap_or(' ');
        let path = line[3..].to_string();
        let entry = CodingGitFileStatus {
            path,
            index_status: index.to_string(),
            worktree_status: worktree.to_string(),
        };

        if is_conflicted(index, worktree) {
            conflicted.push(entry);
        } else if index == '?' && worktree == '?' {
            untracked.push(entry);
        } else {
            if index != ' ' {
                staged.push(entry.clone());
            }
            if worktree != ' ' {
                unstaged.push(entry);
            }
        }
    }

    let clean =
        staged.is_empty() && unstaged.is_empty() && untracked.is_empty() && conflicted.is_empty();
    let truncated = raw.len() > 100;
    if truncated {
        raw.truncate(100);
    }

    CodingGitStatusResult {
        workspace_root,
        repo_root,
        branch,
        head,
        upstream,
        ahead,
        behind,
        clean,
        staged,
        unstaged,
        untracked,
        conflicted,
        total,
        truncated,
        raw,
    }
}

fn parse_branch_line(line: &str) -> (Option<String>, Option<String>, i64, i64) {
    let mut ahead = 0;
    let mut behind = 0;
    let mut body = line.trim();

    if let Some((prefix, suffix)) = body.split_once(" [") {
        body = prefix.trim();
        let hints = suffix.trim_end_matches(']');
        for part in hints.split(',').map(str::trim) {
            if let Some(value) = part.strip_prefix("ahead ") {
                ahead = value.parse().unwrap_or(0);
            } else if let Some(value) = part.strip_prefix("behind ") {
                behind = value.parse().unwrap_or(0);
            }
        }
    }

    if let Some(rest) = body.strip_prefix("No commits yet on ") {
        return (Some(rest.to_string()), None, ahead, behind);
    }

    if let Some((local, remote)) = body.split_once("...") {
        (
            Some(local.to_string()),
            Some(remote.to_string()),
            ahead,
            behind,
        )
    } else {
        (Some(body.to_string()), None, ahead, behind)
    }
}

fn is_conflicted(index: char, worktree: char) -> bool {
    index == 'U'
        || worktree == 'U'
        || matches!(
            (index, worktree),
            ('A', 'A') | ('D', 'D') | ('A', 'U') | ('U', 'A') | ('D', 'U') | ('U', 'D')
        )
}

fn parse_numstat(numstat: &str) -> Vec<CodingGitDiffStat> {
    numstat
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '\t');
            let additions_raw = parts.next()?;
            let deletions_raw = parts.next()?;
            let path = parts.next()?.to_string();
            let binary = additions_raw == "-" || deletions_raw == "-";
            Some(CodingGitDiffStat {
                path,
                additions: additions_raw.parse().unwrap_or(0),
                deletions: deletions_raw.parse().unwrap_or(0),
                binary,
            })
        })
        .collect()
}

fn normalize_optional_paths(
    workspace_root: &Path,
    paths: Option<Vec<String>>,
) -> Result<Vec<String>, String> {
    match paths {
        Some(paths) => normalize_required_paths(workspace_root, paths),
        None => Ok(Vec::new()),
    }
}

fn normalize_required_paths(
    workspace_root: &Path,
    paths: Vec<String>,
) -> Result<Vec<String>, String> {
    if paths.is_empty() {
        return Err("'paths' array cannot be empty".to_string());
    }
    if paths.len() > MAX_GIT_PATHS {
        return Err(format!(
            "Too many git paths: {} (max {})",
            paths.len(),
            MAX_GIT_PATHS
        ));
    }

    let mut normalized = Vec::with_capacity(paths.len());
    for path in paths {
        normalized.push(normalize_pathspec(workspace_root, &path)?);
    }
    Ok(normalized)
}

fn normalize_pathspec(workspace_root: &Path, raw: &str) -> Result<String, String> {
    if raw.trim().is_empty() {
        return Err("paths[]: path is empty".to_string());
    }
    if raw.len() > 4096 {
        return Err("paths[]: path exceeds 4096 characters".to_string());
    }
    if raw.contains('\0') {
        return Err("paths[]: path contains null bytes".to_string());
    }

    let path = Path::new(raw);
    let relative = if path.is_absolute() || raw.starts_with('/') {
        validate_path(raw, "paths[]")?;
        let absolute = canonicalize_existing_or_parent(path)?;
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
        return Err("paths[]: workspace root cannot be staged as a path".to_string());
    }
    Ok(display)
}

fn normalize_relative_path(path: &Path) -> Result<PathBuf, String> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err("paths[]: path traversal ('..') not allowed".to_string())
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err("paths[]: absolute path could not be normalized".to_string())
            }
        }
    }
    Ok(out)
}

fn canonicalize_existing_or_parent(path: &Path) -> Result<PathBuf, String> {
    if path.exists() {
        return std::fs::canonicalize(path).map_err(|e| format!("Resolve path: {e}"));
    }

    let parent = path
        .parent()
        .ok_or_else(|| format!("Path has no parent: {}", path.display()))?;
    let file_name = path
        .file_name()
        .ok_or_else(|| format!("Path has no filename: {}", path.display()))?;
    let parent = std::fs::canonicalize(parent).map_err(|e| format!("Resolve path parent: {e}"))?;
    Ok(parent.join(file_name))
}

fn validate_commit_message(message: &str) -> Result<String, String> {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        return Err("'message' cannot be empty".to_string());
    }
    if trimmed.len() > 10_000 {
        return Err("'message' exceeds 10000 characters".to_string());
    }
    if trimmed.contains('\0') {
        return Err("'message' contains null bytes".to_string());
    }
    Ok(trimmed.to_string())
}

fn literal_git_args<const N: usize>(args: [&str; N]) -> Vec<OsString> {
    let mut out = vec![OsString::from("--literal-pathspecs")];
    out.extend(args.into_iter().map(OsString::from));
    out
}

fn str_git_args<const N: usize>(args: [&str; N]) -> Vec<OsString> {
    args.into_iter().map(OsString::from).collect()
}

fn append_pathspecs(args: &mut Vec<OsString>, paths: &[String]) {
    if paths.is_empty() {
        return;
    }
    args.push("--".into());
    args.extend(paths.iter().map(OsString::from));
}

fn run_git_stdout(root: &Path, args: &[OsString]) -> Result<String, String> {
    let output = run_git_output(root, args)?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!(
            "git {} failed with exit code {}: {}",
            format_args_for_error(args),
            exit_code(&output.status),
            stderr.trim()
        ))
    }
}

fn run_git_output(root: &Path, args: &[OsString]) -> Result<std::process::Output, String> {
    Command::new("git")
        .args(args)
        .current_dir(root)
        .env("LC_ALL", "C")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "git is not installed or not found in PATH".to_string()
            } else {
                format!("Failed to run git: {e}")
            }
        })
}

fn format_args_for_error(args: &[OsString]) -> String {
    args.iter()
        .map(|arg| arg.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ")
}

fn exit_code(status: &std::process::ExitStatus) -> String {
    status
        .code()
        .map(|code| code.to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn truncate_chars(value: &str, max_bytes: usize) -> (String, bool) {
    if value.len() <= max_bytes {
        return (value.to_string(), false);
    }

    let mut end = 0;
    for (idx, _) in value.char_indices() {
        if idx > max_bytes {
            break;
        }
        end = idx;
    }
    if end == 0 {
        end = max_bytes.min(value.len());
    }
    (value[..end].to_string(), true)
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn run(repo: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn init_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        run(dir.path(), &["init", "-q"]);
        run(dir.path(), &["config", "user.name", "Aero Test"]);
        run(dir.path(), &["config", "user.email", "aero@example.test"]);
        fs::write(dir.path().join("README.md"), "hello\n").expect("write");
        run(dir.path(), &["add", "README.md"]);
        run(dir.path(), &["commit", "-m", "initial"]);
        dir
    }

    #[test]
    fn parse_status_groups_porcelain_entries() {
        let status = "\
## main...origin/main [ahead 1, behind 2]
 M src/lib.rs
A  src/new.rs
?? notes.txt
UU conflict.txt
";
        let parsed = parse_status(
            status,
            "/repo".to_string(),
            "/repo".to_string(),
            Some("abc".to_string()),
        );

        assert_eq!(parsed.branch.as_deref(), Some("main"));
        assert_eq!(parsed.upstream.as_deref(), Some("origin/main"));
        assert_eq!(parsed.ahead, 1);
        assert_eq!(parsed.behind, 2);
        assert_eq!(parsed.staged[0].path, "src/new.rs");
        assert_eq!(parsed.unstaged[0].path, "src/lib.rs");
        assert_eq!(parsed.untracked[0].path, "notes.txt");
        assert_eq!(parsed.conflicted[0].path, "conflict.txt");
        assert!(!parsed.clean);
    }

    #[test]
    fn git_stage_dry_run_does_not_mutate_index() {
        let dir = init_repo();
        fs::write(dir.path().join("README.md"), "hello\nchanged\n").expect("write");

        let result = git_stage(CodingGitStageRequest {
            workspace_root: path_to_string(dir.path()),
            paths: vec!["README.md".to_string()],
            dry_run: true,
        })
        .expect("stage dry run");

        assert!(result.success);
        assert!(!result.staged);
        assert_eq!(result.before.unstaged.len(), 1);

        let status = git_status(path_to_string(dir.path()), None).expect("status");
        assert_eq!(status.staged.len(), 0);
        assert_eq!(status.unstaged.len(), 1);
    }

    #[test]
    fn git_stage_and_commit_create_commit() {
        let dir = init_repo();
        fs::write(dir.path().join("feature.txt"), "feature\n").expect("write");

        let stage = git_stage(CodingGitStageRequest {
            workspace_root: path_to_string(dir.path()),
            paths: vec!["feature.txt".to_string()],
            dry_run: false,
        })
        .expect("stage");
        assert!(stage.staged);
        assert!(!stage.after.expect("after").staged.is_empty());

        let commit = git_commit(CodingGitCommitRequest {
            workspace_root: path_to_string(dir.path()),
            message: "add feature".to_string(),
            dry_run: false,
        })
        .expect("commit");

        assert!(commit.success);
        assert!(commit.committed);
        assert!(commit.commit_hash.is_some());
        assert!(commit.after.expect("after").clean);
    }

    #[test]
    fn git_commit_refuses_empty_index_without_erroring() {
        let dir = init_repo();

        let commit = git_commit(CodingGitCommitRequest {
            workspace_root: path_to_string(dir.path()),
            message: "empty".to_string(),
            dry_run: false,
        })
        .expect("commit result");

        assert!(!commit.success);
        assert!(!commit.committed);
        assert!(commit.message.contains("No staged changes"));
    }

    #[test]
    fn normalize_pathspec_rejects_traversal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = normalize_pathspec(dir.path(), "../outside.txt").expect_err("traversal");
        assert!(err.contains("path traversal"));
    }

    #[test]
    fn parse_log_splits_unit_separated_fields() {
        let stdout = "abc123\u{1f}abc\u{1f}Jane\u{1f}2026-06-25T10:00:00+00:00\u{1f}first\n\
                      def456\u{1f}def\u{1f}John\u{1f}2026-06-24T10:00:00+00:00\u{1f}second\n";
        let entries = parse_log(stdout);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].hash, "abc123");
        assert_eq!(entries[0].subject, "first");
        assert_eq!(entries[1].author, "John");
    }

    #[test]
    fn validate_git_ref_rejects_injection() {
        assert!(validate_git_ref("--all").is_err());
        assert!(validate_git_ref("").is_err());
        assert!(validate_git_ref("a b").is_err());
        assert!(validate_git_ref(&"x".repeat(MAX_GIT_REF_LEN + 1)).is_err());
        assert_eq!(validate_git_ref("HEAD~1").unwrap(), "HEAD~1");
        assert_eq!(validate_git_ref("feature/x").unwrap(), "feature/x");
    }

    #[test]
    fn git_log_lists_commits_newest_first() {
        let dir = init_repo();
        fs::write(dir.path().join("a.txt"), "a\n").expect("write");
        run(dir.path(), &["add", "a.txt"]);
        run(dir.path(), &["commit", "-m", "second"]);

        let result = git_log(CodingGitLogRequest {
            workspace_root: path_to_string(dir.path()),
            paths: None,
            max_count: Some(10),
        })
        .expect("log");

        assert_eq!(result.commits.len(), 2);
        assert_eq!(result.commits[0].subject, "second");
        assert_eq!(result.commits[1].subject, "initial");
        assert!(!result.truncated);
    }

    #[test]
    fn git_log_flags_truncation() {
        let dir = init_repo();
        fs::write(dir.path().join("a.txt"), "a\n").expect("write");
        run(dir.path(), &["add", "a.txt"]);
        run(dir.path(), &["commit", "-m", "second"]);

        let result = git_log(CodingGitLogRequest {
            workspace_root: path_to_string(dir.path()),
            paths: None,
            max_count: Some(1),
        })
        .expect("log");

        assert_eq!(result.commits.len(), 1);
        assert!(result.truncated);
    }

    #[test]
    fn git_show_returns_metadata_and_diff() {
        let dir = init_repo();
        fs::write(dir.path().join("a.txt"), "alpha\n").expect("write");
        run(dir.path(), &["add", "a.txt"]);
        run(dir.path(), &["commit", "-m", "add alpha"]);

        let result = git_show(CodingGitShowRequest {
            workspace_root: path_to_string(dir.path()),
            commit: "HEAD".to_string(),
            max_bytes: None,
        })
        .expect("show");

        assert_eq!(result.subject, "add alpha");
        assert_eq!(result.author, "Aero Test");
        assert!(result.diff.contains("alpha"));
        assert_eq!(result.stats.len(), 1);
        assert_eq!(result.stats[0].path, "a.txt");
        assert_eq!(result.total_additions, 1);
    }
}
