//! Virtual path helpers for MTP object trees.
//!
//! Scheme (see APPENDIX-MTP/05):
//! ```text
//! /                          -> list storages
//! /{storage}                 -> storage root
//! /{storage}/DCIM/IMG.jpg    -> nested object
//! ```
//!
//! Paths are AeroFTP-invented strings for UI/cwd. They are NOT OS paths and
//! must never be passed to local filesystem commands.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use crate::providers::types::ProviderError;

/// Normalize a virtual MTP path to a canonical form starting with `/`.
///
/// Rejects NUL, empty segments, `.`, and `..` (no path traversal on the
/// virtual tree). Collapses repeated `/`. Trailing slash is stripped except
/// for the root `/`.
pub fn normalize_virtual_path(path: &str) -> Result<String, ProviderError> {
    if path.contains('\0') {
        return Err(ProviderError::InvalidPath(
            "MTP path contains a null byte".to_string(),
        ));
    }
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed == "/" {
        return Ok("/".to_string());
    }
    let mut out = String::from("/");
    for part in trimmed.split('/') {
        if part.is_empty() {
            continue;
        }
        if part == "." {
            continue;
        }
        if part == ".." {
            return Err(ProviderError::InvalidPath(
                "MTP path must not contain '..'".to_string(),
            ));
        }
        if part.contains('\0') {
            return Err(ProviderError::InvalidPath(
                "MTP path segment contains a null byte".to_string(),
            ));
        }
        if out != "/" {
            out.push('/');
        }
        out.push_str(part);
    }
    if out.is_empty() {
        Ok("/".to_string())
    } else {
        Ok(out)
    }
}

/// Split a normalized virtual path into segments (no leading empty).
///
/// `/` -> `[]`
/// `/Internal` -> `["Internal"]`
/// `/Internal/DCIM` -> `["Internal", "DCIM"]`
pub fn split_segments(normalized: &str) -> Result<Vec<String>, ProviderError> {
    let norm = normalize_virtual_path(normalized)?;
    if norm == "/" {
        return Ok(Vec::new());
    }
    Ok(norm
        .trim_start_matches('/')
        .split('/')
        .map(|s| s.to_string())
        .collect())
}

/// Join a parent virtual directory and a leaf name into a normalized path.
pub fn join_virtual(parent: &str, name: &str) -> Result<String, ProviderError> {
    let leaf = name.trim();
    if leaf.is_empty() || leaf == "." || leaf == ".." || leaf.contains('/') || leaf.contains('\0') {
        return Err(ProviderError::InvalidPath(format!(
            "invalid MTP leaf name: {name:?}"
        )));
    }
    let parent_n = normalize_virtual_path(parent)?;
    if parent_n == "/" {
        normalize_virtual_path(&format!("/{leaf}"))
    } else {
        normalize_virtual_path(&format!("{parent_n}/{leaf}"))
    }
}

/// Parent of a virtual path, or `/` for root children. Root's parent is `/`.
pub fn parent_path(path: &str) -> Result<String, ProviderError> {
    let norm = normalize_virtual_path(path)?;
    if norm == "/" {
        return Ok("/".to_string());
    }
    match norm.rsplit_once('/') {
        Some(("", _)) => Ok("/".to_string()),
        Some((prefix, _)) => normalize_virtual_path(prefix),
        None => Ok("/".to_string()),
    }
}

/// Final leaf name of a virtual path (`/` -> empty string).
pub fn leaf_name(path: &str) -> Result<String, ProviderError> {
    let norm = normalize_virtual_path(path)?;
    if norm == "/" {
        return Ok(String::new());
    }
    Ok(norm.rsplit('/').next().unwrap_or("").to_string())
}

/// Sanitize a device-supplied object name for use as a local download leaf.
///
/// Takes only the final component if separators appear, strips `..` / `.`,
/// and replaces residual path separators. Does not invent extensions.
pub fn sanitize_leaf_for_download(name: &str) -> String {
    let base = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .trim()
        .trim_matches('.');
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c == '\0' || c == '/' || c == '\\' {
                '_'
            } else {
                c
            }
        })
        .collect();
    if cleaned.is_empty() || cleaned == ".." || cleaned == "." {
        "mtp-object".to_string()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_join_split_roundtrip() {
        let p = join_virtual("/Internal", "DCIM").unwrap();
        assert_eq!(p, "/Internal/DCIM");
        let segs = split_segments(&p).unwrap();
        assert_eq!(segs, vec!["Internal".to_string(), "DCIM".to_string()]);
        let file = join_virtual(&p, "IMG_01.JPG").unwrap();
        assert_eq!(file, "/Internal/DCIM/IMG_01.JPG");
        assert_eq!(leaf_name(&file).unwrap(), "IMG_01.JPG");
        assert_eq!(parent_path(&file).unwrap(), "/Internal/DCIM");
    }

    #[test]
    fn root_and_collapse_slashes() {
        assert_eq!(normalize_virtual_path("").unwrap(), "/");
        assert_eq!(normalize_virtual_path("/").unwrap(), "/");
        assert_eq!(normalize_virtual_path("///a//b/").unwrap(), "/a/b");
        assert!(split_segments("/").unwrap().is_empty());
    }

    #[test]
    fn path_rejects_dotdot() {
        assert!(normalize_virtual_path("/a/../b").is_err());
        assert!(join_virtual("/a", "..").is_err());
        assert!(normalize_virtual_path("..").is_err());
    }

    #[test]
    fn path_rejects_nul() {
        assert!(normalize_virtual_path("/a\0b").is_err());
        assert!(join_virtual("/", "x\0y").is_err());
    }

    #[test]
    fn sanitize_leaf_for_download_strips_traversal() {
        assert_eq!(sanitize_leaf_for_download("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_leaf_for_download(".."), "mtp-object");
        assert_eq!(sanitize_leaf_for_download("ok.jpg"), "ok.jpg");
        assert_eq!(sanitize_leaf_for_download("a\\b\\photo.png"), "photo.png");
    }
}
