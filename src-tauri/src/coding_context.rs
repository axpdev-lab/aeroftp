// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Explicit user-attached context for AeroAgent coding mode.

use serde::Serialize;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

const MAX_MENTIONS: usize = 10;
const MAX_FILE_BYTES: u64 = 24 * 1024;
const MAX_FOLDER_ENTRIES: usize = 100;

#[derive(Debug, Serialize)]
pub struct MentionFolderEntry {
    pub path: String,
    pub kind: String,
    pub size: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct MentionAttachment {
    pub kind: String,
    pub path: String,
    pub content: Option<String>,
    pub entries: Option<Vec<MentionFolderEntry>>,
    pub truncated: bool,
    pub size: u64,
    pub error: Option<String>,
}

fn validate_project_path(path: &str) -> Result<(), String> {
    if path.len() > 4096 {
        return Err("Path exceeds 4096 character limit".to_string());
    }
    if path.contains('\0') {
        return Err("Path contains null bytes".to_string());
    }
    for component in Path::new(path).components() {
        if matches!(component, Component::ParentDir) {
            return Err("Path traversal ('..') not allowed".to_string());
        }
    }
    Ok(())
}

fn validate_mention_path(path: &str) -> Result<(), String> {
    if path.is_empty() {
        return Err("Mention path is empty".to_string());
    }
    if path.len() > 4096 {
        return Err("Mention path exceeds 4096 character limit".to_string());
    }
    if path.contains('\0') {
        return Err("Mention path contains null bytes".to_string());
    }
    for component in Path::new(path).components() {
        if matches!(component, Component::ParentDir) {
            return Err("Mention path traversal ('..') not allowed".to_string());
        }
    }
    Ok(())
}

fn resolve_mention(root: &Path, mention: &str) -> PathBuf {
    let trimmed = mention.trim_start_matches('@').trim();
    let path = Path::new(trimmed);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

fn relative_display(root: &Path, canonical: &Path) -> String {
    canonical
        .strip_prefix(root)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| canonical.to_string_lossy().to_string())
}

fn error_attachment(path: &str, error: String) -> MentionAttachment {
    MentionAttachment {
        kind: "error".to_string(),
        path: path.to_string(),
        content: None,
        entries: None,
        truncated: false,
        size: 0,
        error: Some(error),
    }
}

fn read_file_attachment(root: &Path, path: &Path, display_path: String) -> MentionAttachment {
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(e) => return error_attachment(&display_path, e.to_string()),
    };
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) => return error_attachment(&display_path, e.to_string()),
    };
    let mut buf = Vec::new();
    if let Err(e) = file.by_ref().take(MAX_FILE_BYTES).read_to_end(&mut buf) {
        return error_attachment(&display_path, e.to_string());
    }
    let truncated = meta.len() > MAX_FILE_BYTES;
    let content = if buf.contains(&0) {
        "[binary file omitted]".to_string()
    } else {
        String::from_utf8_lossy(&buf).to_string()
    };

    MentionAttachment {
        kind: "file".to_string(),
        path: relative_display(root, path),
        content: Some(content),
        entries: None,
        truncated,
        size: meta.len(),
        error: None,
    }
}

fn read_folder_attachment(root: &Path, path: &Path, display_path: String) -> MentionAttachment {
    let entries_iter = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(e) => return error_attachment(&display_path, e.to_string()),
    };

    let mut entries = Vec::new();
    let mut total = 0usize;
    for entry in entries_iter.flatten() {
        total += 1;
        if entries.len() >= MAX_FOLDER_ENTRIES {
            continue;
        }
        let entry_path = entry.path();
        let meta = match entry.metadata() {
            Ok(meta) => meta,
            Err(_) => continue,
        };
        let kind = if meta.is_dir() { "dir" } else { "file" }.to_string();
        entries.push(MentionFolderEntry {
            path: relative_display(root, &entry_path),
            kind,
            size: if meta.is_file() {
                Some(meta.len())
            } else {
                None
            },
        });
    }

    MentionAttachment {
        kind: "folder".to_string(),
        path: relative_display(root, path),
        content: None,
        entries: Some(entries),
        truncated: total > MAX_FOLDER_ENTRIES,
        size: 0,
        error: None,
    }
}

#[tauri::command]
pub async fn resolve_context_mentions(
    project_path: String,
    mentions: Vec<String>,
) -> Result<Vec<MentionAttachment>, String> {
    validate_project_path(&project_path)?;
    let root = Path::new(&project_path);
    if !root.exists() {
        return Err(format!("Path does not exist: {}", project_path));
    }
    if !root.is_dir() {
        return Err(format!("Path is not a directory: {}", project_path));
    }

    let root_canonical = std::fs::canonicalize(root)
        .map_err(|e| format!("Failed to resolve project path {}: {}", project_path, e))?;

    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for mention in mentions.into_iter().take(MAX_MENTIONS) {
        let clean = mention.trim().trim_start_matches('@').to_string();
        if !seen.insert(clean.clone()) {
            continue;
        }
        if let Err(e) = validate_mention_path(&clean) {
            out.push(error_attachment(&clean, e));
            continue;
        }
        let resolved = resolve_mention(&root_canonical, &clean);
        let canonical = match std::fs::canonicalize(&resolved) {
            Ok(path) => path,
            Err(e) => {
                out.push(error_attachment(&clean, e.to_string()));
                continue;
            }
        };
        if !canonical.starts_with(&root_canonical) {
            out.push(error_attachment(
                &clean,
                "Mention resolves outside project root".to_string(),
            ));
            continue;
        }
        let display = relative_display(&root_canonical, &canonical);
        if canonical.is_file() {
            out.push(read_file_attachment(&root_canonical, &canonical, display));
        } else if canonical.is_dir() {
            out.push(read_folder_attachment(&root_canonical, &canonical, display));
        } else {
            out.push(error_attachment(
                &display,
                "Unsupported path type".to_string(),
            ));
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn resolves_file_mentions_inside_project() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("README.md"), "hello").expect("write");
        let result = resolve_context_mentions(
            dir.path().to_string_lossy().to_string(),
            vec!["README.md".to_string()],
        )
        .await
        .expect("mentions");

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].kind, "file");
        assert_eq!(result[0].path, "README.md");
        assert_eq!(result[0].content.as_deref(), Some("hello"));
    }

    #[tokio::test]
    async fn rejects_traversal_mentions() {
        let dir = tempfile::tempdir().expect("tempdir");
        let result = resolve_context_mentions(
            dir.path().to_string_lossy().to_string(),
            vec!["../secret.txt".to_string()],
        )
        .await
        .expect("mentions");

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].kind, "error");
        assert!(result[0]
            .error
            .as_deref()
            .unwrap_or("")
            .contains("traversal"));
    }

    #[tokio::test]
    async fn lists_folder_mentions_with_cap() {
        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("src");
        std::fs::create_dir(&src).expect("mkdir");
        std::fs::write(src.join("main.rs"), "fn main() {}").expect("write");

        let result = resolve_context_mentions(
            dir.path().to_string_lossy().to_string(),
            vec!["src".to_string()],
        )
        .await
        .expect("mentions");

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].kind, "folder");
        let entries = result[0].entries.as_ref().expect("entries");
        assert_eq!(entries[0].path, "src/main.rs");
    }
}
