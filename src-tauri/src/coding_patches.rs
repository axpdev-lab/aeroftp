// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Unified-diff patch engine for AeroAgent coding mode.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

const MAX_PATCH_BYTES: usize = 512 * 1024;
const MAX_PATCH_FILES: usize = 50;
const MAX_PATCH_HUNKS: usize = 500;
const MAX_TARGET_FILE_BYTES: u64 = 5 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct ApplyCodingPatchRequest {
    pub workspace_root: String,
    pub patch: String,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodingPatchFileResult {
    pub path: String,
    pub status: String,
    pub hunks: usize,
    pub additions: usize,
    pub deletions: usize,
    pub old_size_bytes: u64,
    pub new_size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodingPatchDiagnostic {
    pub path: Option<String>,
    pub hunk_index: Option<usize>,
    pub message: String,
    pub expected: Option<String>,
    pub actual: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodingPatchResult {
    pub success: bool,
    pub dry_run: bool,
    pub checkpoint_id: Option<String>,
    pub files: Vec<CodingPatchFileResult>,
    pub diagnostics: Vec<CodingPatchDiagnostic>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
struct FilePatch {
    old_path: Option<String>,
    new_path: Option<String>,
    target_path: String,
    hunks: Vec<Hunk>,
}

#[derive(Debug, Clone)]
struct Hunk {
    old_start: usize,
    lines: Vec<HunkLine>,
}

#[derive(Debug, Clone)]
enum HunkLine {
    Context(String),
    Remove(String),
    Add(String),
}

#[derive(Debug, Clone)]
struct PlannedPatch {
    file: CodingPatchFileResult,
    target: PathBuf,
    new_content: Option<Vec<u8>>,
}

struct PatchPlan {
    plans: Vec<PlannedPatch>,
    diagnostics: Vec<CodingPatchDiagnostic>,
}

struct TextFile {
    lines: Vec<String>,
    final_newline: bool,
    newline: &'static str,
}

pub fn apply_coding_patch(req: ApplyCodingPatchRequest) -> Result<CodingPatchResult, String> {
    let root = validate_workspace_root(&req.workspace_root)?;
    let patches = parse_unified_diff(&req.patch)?;
    let plan = plan_patch(&root, &patches)?;

    if !plan.diagnostics.is_empty() {
        return Ok(CodingPatchResult {
            success: false,
            dry_run: req.dry_run,
            checkpoint_id: None,
            files: plan.plans.into_iter().map(|plan| plan.file).collect(),
            diagnostics: plan.diagnostics,
            warnings: Vec::new(),
        });
    }

    if !req.dry_run {
        write_planned_patch(&plan.plans)?;
    }

    Ok(CodingPatchResult {
        success: true,
        dry_run: req.dry_run,
        checkpoint_id: None,
        files: plan.plans.into_iter().map(|plan| plan.file).collect(),
        diagnostics: Vec::new(),
        warnings: Vec::new(),
    })
}

pub fn preview_coding_patch(req: ApplyCodingPatchRequest) -> Result<CodingPatchResult, String> {
    apply_coding_patch(ApplyCodingPatchRequest {
        dry_run: true,
        ..req
    })
}

fn parse_unified_diff(patch: &str) -> Result<Vec<FilePatch>, String> {
    if patch.trim().is_empty() {
        return Err("Patch is empty".to_string());
    }
    if patch.len() > MAX_PATCH_BYTES {
        return Err(format!(
            "Patch is too large: {:.1} KB (max {:.1} KB)",
            patch.len() as f64 / 1024.0,
            MAX_PATCH_BYTES as f64 / 1024.0
        ));
    }

    let lines: Vec<&str> = patch.lines().collect();
    let mut patches = Vec::new();
    let mut i = 0usize;
    let mut total_hunks = 0usize;

    while i < lines.len() {
        if !lines[i].starts_with("--- ") {
            i += 1;
            continue;
        }

        let old_path = parse_file_marker(lines[i], "--- ")?;
        i += 1;
        if i >= lines.len() || !lines[i].starts_with("+++ ") {
            return Err("Malformed patch: expected +++ file marker".to_string());
        }
        let new_path = parse_file_marker(lines[i], "+++ ")?;
        i += 1;

        let target_path = choose_target_path(old_path.as_deref(), new_path.as_deref())?;
        let mut hunks = Vec::new();

        while i < lines.len() {
            if lines[i].starts_with("--- ") {
                break;
            }
            if !lines[i].starts_with("@@ ") {
                i += 1;
                continue;
            }

            let header = parse_hunk_header(lines[i])?;
            i += 1;
            let mut hunk_lines = Vec::new();
            let mut old_seen = 0usize;
            let mut new_seen = 0usize;
            while i < lines.len() && (old_seen < header.old_count || new_seen < header.new_count) {
                let line = lines[i];
                if line.starts_with("\\ No newline at end of file") {
                    i += 1;
                    continue;
                }
                if line.is_empty() {
                    return Err("Malformed hunk line".to_string());
                }
                let (prefix, content) = line.split_at(1);
                match prefix {
                    " " => {
                        old_seen += 1;
                        new_seen += 1;
                        hunk_lines.push(HunkLine::Context(content.to_string()));
                    }
                    "-" => {
                        old_seen += 1;
                        hunk_lines.push(HunkLine::Remove(content.to_string()));
                    }
                    "+" => {
                        new_seen += 1;
                        hunk_lines.push(HunkLine::Add(content.to_string()));
                    }
                    _ => {
                        return Err(format!(
                            "Malformed hunk line for {}: expected space, +, or -",
                            target_path
                        ));
                    }
                }
                i += 1;
            }

            validate_hunk_counts(&header, &hunk_lines, &target_path)?;
            hunks.push(Hunk {
                old_start: header.old_start,
                lines: hunk_lines,
            });
            total_hunks += 1;
            if total_hunks > MAX_PATCH_HUNKS {
                return Err(format!(
                    "Patch has too many hunks (max {})",
                    MAX_PATCH_HUNKS
                ));
            }
        }

        if hunks.is_empty() {
            return Err(format!("File patch has no hunks: {}", target_path));
        }
        patches.push(FilePatch {
            old_path,
            new_path,
            target_path,
            hunks,
        });
        if patches.len() > MAX_PATCH_FILES {
            return Err(format!(
                "Patch touches too many files (max {})",
                MAX_PATCH_FILES
            ));
        }
    }

    if patches.is_empty() {
        return Err("No file patches found in unified diff".to_string());
    }

    Ok(patches)
}

fn plan_patch(root: &Path, patches: &[FilePatch]) -> Result<PatchPlan, String> {
    let mut seen = HashSet::new();
    let mut plans = Vec::new();
    let mut diagnostics = Vec::new();

    for file_patch in patches {
        validate_file_patch_paths(file_patch)?;
        if !seen.insert(file_patch.target_path.clone()) {
            return Err(format!(
                "Patch contains multiple sections for the same file: {}",
                file_patch.target_path
            ));
        }

        let target = resolve_patch_target(root, &file_patch.target_path)?;
        let status = patch_status(file_patch);
        let old_content = read_target_text(&target, status)?;
        let old_size_bytes = old_content
            .as_ref()
            .map(|text| text.len() as u64)
            .unwrap_or(0);

        let new_text =
            match apply_file_patch(&file_patch.target_path, old_content.as_deref(), file_patch) {
                Ok(new_text) => new_text,
                Err(diag) => {
                    diagnostics.push(diag);
                    continue;
                }
            };
        let new_content = if status == "deleted" {
            None
        } else {
            Some(new_text.into_bytes())
        };

        let new_size_bytes = new_content
            .as_ref()
            .map(|bytes| bytes.len() as u64)
            .unwrap_or(0);
        let (additions, deletions) = count_changes(file_patch);
        plans.push(PlannedPatch {
            file: CodingPatchFileResult {
                path: file_patch.target_path.clone(),
                status: status.to_string(),
                hunks: file_patch.hunks.len(),
                additions,
                deletions,
                old_size_bytes,
                new_size_bytes,
            },
            target,
            new_content,
        });
    }

    Ok(PatchPlan { plans, diagnostics })
}

fn write_planned_patch(plans: &[PlannedPatch]) -> Result<(), String> {
    for plan in plans {
        if plan.file.status == "deleted" {
            if plan.target.exists() {
                let meta = std::fs::symlink_metadata(&plan.target).map_err(|e| {
                    format!(
                        "Failed to inspect delete target {}: {e}",
                        plan.target.display()
                    )
                })?;
                if meta.file_type().is_symlink() || meta.is_dir() {
                    return Err(format!(
                        "Refusing to delete non-regular file: {}",
                        plan.target.display()
                    ));
                }
                std::fs::remove_file(&plan.target)
                    .map_err(|e| format!("Failed to delete {}: {e}", plan.target.display()))?;
            }
            continue;
        }

        let Some(content) = &plan.new_content else {
            return Err(format!("Missing patched content for {}", plan.file.path));
        };
        write_file_atomically(&plan.target, content)?;
    }
    Ok(())
}

fn apply_file_patch(
    path: &str,
    old_content: Option<&str>,
    file_patch: &FilePatch,
) -> Result<String, CodingPatchDiagnostic> {
    let old = old_content.map(split_text).unwrap_or_else(|| TextFile {
        lines: Vec::new(),
        final_newline: true,
        newline: "\n",
    });

    let mut out = Vec::new();
    let mut cursor = 0usize;

    for (hunk_index, hunk) in file_patch.hunks.iter().enumerate() {
        let target = hunk.old_start.saturating_sub(1);
        if target < cursor {
            return Err(diag(
                path,
                hunk_index,
                "Hunk overlaps a previous hunk",
                None,
                None,
            ));
        }
        if target > old.lines.len() {
            return Err(diag(
                path,
                hunk_index,
                "Hunk starts beyond end of file",
                None,
                None,
            ));
        }

        out.extend_from_slice(&old.lines[cursor..target]);
        cursor = target;

        for line in &hunk.lines {
            match line {
                HunkLine::Context(expected) => {
                    ensure_line(path, hunk_index, &old.lines, cursor, expected)?;
                    out.push(old.lines[cursor].clone());
                    cursor += 1;
                }
                HunkLine::Remove(expected) => {
                    ensure_line(path, hunk_index, &old.lines, cursor, expected)?;
                    cursor += 1;
                }
                HunkLine::Add(content) => out.push(content.clone()),
            }
        }
    }

    out.extend_from_slice(&old.lines[cursor..]);
    Ok(join_text(
        &out,
        old.newline,
        old.final_newline || file_patch.old_path.is_none(),
    ))
}

fn ensure_line(
    path: &str,
    hunk_index: usize,
    lines: &[String],
    index: usize,
    expected: &str,
) -> Result<(), CodingPatchDiagnostic> {
    let actual = lines.get(index);
    if actual.map(String::as_str) == Some(expected) {
        return Ok(());
    }
    Err(diag(
        path,
        hunk_index,
        "Patch context did not match",
        Some(expected.to_string()),
        actual.cloned(),
    ))
}

fn split_text(content: &str) -> TextFile {
    let newline = if content.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let final_newline = content.ends_with('\n');
    let normalized = content.replace("\r\n", "\n");
    let mut lines: Vec<String> = normalized.split('\n').map(str::to_string).collect();
    if final_newline {
        lines.pop();
    }
    TextFile {
        lines,
        final_newline,
        newline,
    }
}

fn join_text(lines: &[String], newline: &str, final_newline: bool) -> String {
    let mut out = lines.join(newline);
    if final_newline && (!lines.is_empty() || !out.is_empty()) {
        out.push_str(newline);
    }
    out
}

fn read_target_text(target: &Path, status: &str) -> Result<Option<String>, String> {
    match status {
        "created" => {
            if target.exists() {
                return Err(format!(
                    "Patch creates a file that already exists: {}",
                    target.display()
                ));
            }
            Ok(None)
        }
        "modified" | "deleted" => {
            let meta = std::fs::symlink_metadata(target)
                .map_err(|e| format!("Failed to inspect {}: {e}", target.display()))?;
            if meta.file_type().is_symlink() {
                return Err(format!("Refusing to patch symlink: {}", target.display()));
            }
            if !meta.is_file() {
                return Err(format!(
                    "Patch target is not a regular file: {}",
                    target.display()
                ));
            }
            if meta.len() > MAX_TARGET_FILE_BYTES {
                return Err(format!(
                    "Target file too large for patch: {} ({:.1} MB, max {:.1} MB)",
                    target.display(),
                    meta.len() as f64 / 1_048_576.0,
                    MAX_TARGET_FILE_BYTES as f64 / 1_048_576.0
                ));
            }
            std::fs::read_to_string(target)
                .map(Some)
                .map_err(|e| format!("Failed to read {} as UTF-8 text: {e}", target.display()))
        }
        _ => Err(format!("Unknown patch status: {status}")),
    }
}

fn write_file_atomically(target: &Path, content: &[u8]) -> Result<(), String> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create parent {}: {e}", parent.display()))?;
    }
    let parent = target
        .parent()
        .ok_or_else(|| format!("Patch target has no parent: {}", target.display()))?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)
        .map_err(|e| format!("Failed to create temp file in {}: {e}", parent.display()))?;
    temp.write_all(content)
        .map_err(|e| format!("Failed to write temp patch file: {e}"))?;
    temp.persist(target)
        .map_err(|e| format!("Failed to replace {}: {e}", target.display()))?;
    Ok(())
}

fn validate_workspace_root(root: &str) -> Result<PathBuf, String> {
    validate_workspace_root_string(root)?;
    let path = Path::new(root);
    if !path.exists() {
        return Err(format!("Workspace root does not exist: {root}"));
    }
    if !path.is_dir() {
        return Err(format!("Workspace root is not a directory: {root}"));
    }
    std::fs::canonicalize(path).map_err(|e| format!("Resolve workspace root: {e}"))
}

fn validate_workspace_root_string(path: &str) -> Result<(), String> {
    if path.trim().is_empty() {
        return Err("workspace_root: path is empty".to_string());
    }
    if path.len() > 4096 {
        return Err("workspace_root: path exceeds 4096 characters".to_string());
    }
    if path.contains('\0') {
        return Err("workspace_root: path contains null bytes".to_string());
    }
    for component in Path::new(path).components() {
        if matches!(component, Component::ParentDir) {
            return Err("workspace_root: path traversal ('..') not allowed".to_string());
        }
    }
    Ok(())
}

fn resolve_patch_target(root: &Path, relative_path: &str) -> Result<PathBuf, String> {
    validate_patch_path(relative_path, "patch path")?;
    let target = root.join(relative_path);
    ensure_existing_ancestor_inside(root, &target)?;
    if target.exists() {
        let canonical = std::fs::canonicalize(&target)
            .map_err(|e| format!("Resolve patch target {}: {e}", target.display()))?;
        if !canonical.starts_with(root) {
            return Err(format!(
                "Patch target resolves outside workspace root: {relative_path}"
            ));
        }
        return Ok(canonical);
    }
    if !target.starts_with(root) {
        return Err(format!(
            "Patch target is outside workspace root: {relative_path}"
        ));
    }
    Ok(target)
}

fn ensure_existing_ancestor_inside(root: &Path, path: &Path) -> Result<(), String> {
    let mut current = path.to_path_buf();
    while !current.exists() {
        if !current.pop() {
            return Err(format!(
                "No existing ancestor for patch path {}",
                path.display()
            ));
        }
    }
    let canonical = std::fs::canonicalize(&current)
        .map_err(|e| format!("Resolve patch path ancestor {}: {e}", current.display()))?;
    if !canonical.starts_with(root) {
        return Err(format!(
            "Patch path ancestor resolves outside workspace root: {}",
            current.display()
        ));
    }
    Ok(())
}

fn validate_patch_path(path: &str, label: &str) -> Result<(), String> {
    if path.trim().is_empty() {
        return Err(format!("{label}: path is empty"));
    }
    if path.len() > 4096 {
        return Err(format!("{label}: path exceeds 4096 characters"));
    }
    if path.contains('\0') {
        return Err(format!("{label}: path contains null bytes"));
    }
    let p = Path::new(path);
    if p.is_absolute() {
        return Err(format!("{label}: absolute paths are not allowed"));
    }
    for component in p.components() {
        if matches!(component, Component::ParentDir) {
            return Err(format!("{label}: path traversal ('..') not allowed"));
        }
    }
    Ok(())
}

fn parse_file_marker(line: &str, prefix: &str) -> Result<Option<String>, String> {
    let raw = line
        .strip_prefix(prefix)
        .ok_or_else(|| format!("Malformed file marker: {line}"))?;
    let raw = raw.split('\t').next().unwrap_or(raw).trim();
    if raw == "/dev/null" {
        return Ok(None);
    }
    let path = raw
        .strip_prefix("a/")
        .or_else(|| raw.strip_prefix("b/"))
        .unwrap_or(raw)
        .to_string();
    validate_patch_path(&path, "patch path")?;
    Ok(Some(path))
}

fn choose_target_path(old_path: Option<&str>, new_path: Option<&str>) -> Result<String, String> {
    match (old_path, new_path) {
        (Some(old), Some(new)) if old == new => Ok(new.to_string()),
        (None, Some(new)) => Ok(new.to_string()),
        (Some(old), None) => Ok(old.to_string()),
        (Some(old), Some(new)) => Err(format!(
            "Patch renames are not supported yet: {} -> {}",
            old, new
        )),
        (None, None) => Err("Patch cannot use /dev/null for both paths".to_string()),
    }
}

#[derive(Debug)]
struct ParsedHunkHeader {
    old_start: usize,
    old_count: usize,
    new_count: usize,
}

fn parse_hunk_header(line: &str) -> Result<ParsedHunkHeader, String> {
    let body = line
        .strip_prefix("@@ ")
        .and_then(|s| s.split(" @@").next())
        .ok_or_else(|| format!("Malformed hunk header: {line}"))?;
    let mut parts = body.split_whitespace();
    let old = parts
        .next()
        .ok_or_else(|| format!("Malformed hunk header: {line}"))?;
    let new = parts
        .next()
        .ok_or_else(|| format!("Malformed hunk header: {line}"))?;
    let (old_start, old_count) = parse_hunk_range(old, '-')?;
    let (_new_start, new_count) = parse_hunk_range(new, '+')?;
    Ok(ParsedHunkHeader {
        old_start,
        old_count,
        new_count,
    })
}

fn parse_hunk_range(input: &str, prefix: char) -> Result<(usize, usize), String> {
    let raw = input
        .strip_prefix(prefix)
        .ok_or_else(|| format!("Malformed hunk range: {input}"))?;
    let mut parts = raw.splitn(2, ',');
    let start = parts
        .next()
        .and_then(|value| value.parse::<usize>().ok())
        .ok_or_else(|| format!("Malformed hunk range: {input}"))?;
    let count = parts
        .next()
        .map(|value| value.parse::<usize>())
        .transpose()
        .map_err(|_| format!("Malformed hunk range: {input}"))?
        .unwrap_or(1);
    Ok((start, count))
}

fn validate_hunk_counts(
    header: &ParsedHunkHeader,
    lines: &[HunkLine],
    path: &str,
) -> Result<(), String> {
    let old_count = lines
        .iter()
        .filter(|line| matches!(line, HunkLine::Context(_) | HunkLine::Remove(_)))
        .count();
    let new_count = lines
        .iter()
        .filter(|line| matches!(line, HunkLine::Context(_) | HunkLine::Add(_)))
        .count();
    if old_count != header.old_count || new_count != header.new_count {
        return Err(format!(
            "Hunk count mismatch in {}: header -{}, +{} but body -{}, +{}",
            path, header.old_count, header.new_count, old_count, new_count
        ));
    }
    Ok(())
}

fn validate_file_patch_paths(file_patch: &FilePatch) -> Result<(), String> {
    if let Some(path) = &file_patch.old_path {
        validate_patch_path(path, "old patch path")?;
    }
    if let Some(path) = &file_patch.new_path {
        validate_patch_path(path, "new patch path")?;
    }
    validate_patch_path(&file_patch.target_path, "patch path")
}

fn patch_status(file_patch: &FilePatch) -> &'static str {
    match (&file_patch.old_path, &file_patch.new_path) {
        (None, Some(_)) => "created",
        (Some(_), None) => "deleted",
        _ => "modified",
    }
}

fn count_changes(file_patch: &FilePatch) -> (usize, usize) {
    let mut additions = 0usize;
    let mut deletions = 0usize;
    for hunk in &file_patch.hunks {
        for line in &hunk.lines {
            match line {
                HunkLine::Add(_) => additions += 1,
                HunkLine::Remove(_) => deletions += 1,
                HunkLine::Context(_) => {}
            }
        }
    }
    (additions, deletions)
}

fn diag(
    path: &str,
    hunk_index: usize,
    message: &str,
    expected: Option<String>,
    actual: Option<String>,
) -> CodingPatchDiagnostic {
    CodingPatchDiagnostic {
        path: Some(path.to_string()),
        hunk_index: Some(hunk_index + 1),
        message: message.to_string(),
        expected,
        actual,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(root: &Path, patch: &str, dry_run: bool) -> ApplyCodingPatchRequest {
        ApplyCodingPatchRequest {
            workspace_root: root.to_string_lossy().to_string(),
            patch: patch.to_string(),
            dry_run,
        }
    }

    #[test]
    fn dry_run_validates_multi_file_patch_without_mutating() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\n").expect("write a");
        std::fs::write(dir.path().join("b.txt"), "red\nblue\n").expect("write b");
        let patch = "\
--- a/a.txt
+++ b/a.txt
@@ -1,2 +1,2 @@
 one
-two
+three
--- a/b.txt
+++ b/b.txt
@@ -1,2 +1,2 @@
 red
-blue
+green
";

        let result = apply_coding_patch(request(dir.path(), patch, true)).expect("dry run");

        assert!(result.success);
        assert!(result.dry_run);
        assert_eq!(result.files.len(), 2);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).expect("read a"),
            "one\ntwo\n"
        );
    }

    #[test]
    fn applies_modify_create_and_delete_patch() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\n").expect("write a");
        std::fs::write(dir.path().join("gone.txt"), "bye\n").expect("write gone");
        let patch = "\
--- a/a.txt
+++ b/a.txt
@@ -1,2 +1,2 @@
 one
-two
+three
--- /dev/null
+++ b/new.txt
@@ -0,0 +1,1 @@
+fresh
--- a/gone.txt
+++ /dev/null
@@ -1,1 +0,0 @@
-bye
";

        let result = apply_coding_patch(request(dir.path(), patch, false)).expect("apply");

        assert_eq!(result.files.len(), 3);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).expect("read a"),
            "one\nthree\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("new.txt")).expect("read new"),
            "fresh\n"
        );
        assert!(!dir.path().join("gone.txt").exists());
    }

    #[test]
    fn reports_conflict_when_context_does_not_match() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\n").expect("write a");
        let patch = "\
--- a/a.txt
+++ b/a.txt
@@ -1,2 +1,2 @@
 nope
-two
+three
";

        let result = apply_coding_patch(request(dir.path(), patch, false)).expect("conflict");

        assert!(!result.success);
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(result.diagnostics[0].message, "Patch context did not match");
        assert_eq!(result.diagnostics[0].path.as_deref(), Some("a.txt"));
        assert_eq!(result.diagnostics[0].hunk_index, Some(1));
        assert_eq!(result.diagnostics[0].expected.as_deref(), Some("nope"));
        assert_eq!(result.diagnostics[0].actual.as_deref(), Some("one"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).expect("read a"),
            "one\ntwo\n"
        );
    }

    #[test]
    fn rejects_traversal_paths() {
        let dir = tempfile::tempdir().expect("tempdir");
        let patch = "\
--- a/../secret.txt
+++ b/../secret.txt
@@ -1,1 +1,1 @@
-old
+new
";

        let err = apply_coding_patch(request(dir.path(), patch, true)).expect_err("traversal");

        assert!(err.contains("traversal"));
    }
}
