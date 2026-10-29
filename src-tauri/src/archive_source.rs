//! Caller-specific archive admission. Ordinary GUI/CLI compression has no policy.
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use std::path::{Path, PathBuf};

// Restricted ZIP now retains a source manifest as 7z/TAR already do. Bound it
// before allocation; exceeding the limit is an error, never a partial success.
const MAX_POLICY_SOURCES: usize = 100_000;

pub(crate) type SourcePolicy<'a> = dyn Fn(&Path) -> Result<(), String> + Send + Sync + 'a;

#[derive(Default)]
pub(crate) struct ArchiveAccess<'a> {
    pub app: Option<tauri::AppHandle>,
    pub policy: Option<&'a SourcePolicy<'a>>,
}

impl From<Option<tauri::AppHandle>> for ArchiveAccess<'_> {
    fn from(app: Option<tauri::AppHandle>) -> Self {
        Self { app, policy: None }
    }
}

pub(crate) struct Source {
    pub path: PathBuf,
    pub name: String,
    pub directory: bool,
    pub bytes: u64,
}

/// A pathname check at each admission, not a descriptor-based race guarantee.
pub(crate) fn authorize(path: &Path, policy: Option<&SourcePolicy<'_>>) -> Result<(), String> {
    if let Some(policy) = policy {
        policy(path)?;
        let resolved = std::fs::canonicalize(path)
            .map_err(|e| format!("Archive source cannot be resolved: {}: {e}", path.display()))?;
        policy(&resolved)?;
    }
    Ok(())
}

pub(crate) fn authorize_file(path: &Path, policy: Option<&SourcePolicy<'_>>) -> Result<(), String> {
    authorize(path, policy)?;
    if policy.is_some() {
        let meta = std::fs::symlink_metadata(path).map_err(|e| {
            format!(
                "Archive source cannot be inspected: {}: {e}",
                path.display()
            )
        })?;
        if !meta.is_file() {
            return Err(format!(
                "Archive source is not a regular file: {}",
                path.display()
            ));
        }
    }
    Ok(())
}

impl ArchiveAccess<'_> {
    /// Outputs may have a missing tail; the initiating policy handles that case.
    pub fn authorize_output(&self, path: &Path) -> Result<(), String> {
        if let Some(policy) = self.policy {
            policy(path)?;
        }
        Ok(())
    }

    /// Collect before creating the temporary archive. Admit directories before
    /// read_dir (WalkDir opens them before yielding them to its caller). Never
    /// hide a walk error or publish a silently incomplete restricted archive.
    /// Nested safe symlinks are omitted; selected symlink roots are refused.
    pub fn collect(&self, paths: &[String]) -> Result<Option<Vec<Source>>, String> {
        if self.policy.is_none() {
            return Ok(None);
        }
        self.collect_bounded(paths, MAX_POLICY_SOURCES).map(Some)
    }

    fn collect_bounded(
        &self,
        paths: &[String],
        mut remaining: usize,
    ) -> Result<Vec<Source>, String> {
        let mut sources = Vec::new();
        for path in paths {
            remaining = remaining
                .checked_sub(1)
                .ok_or("Archive source entry limit exceeded")?;
            let root = Path::new(path);
            let mut pending = vec![root.to_path_buf()];
            while let Some(path) = pending.pop() {
                authorize(&path, self.policy)?;
                let meta = std::fs::symlink_metadata(&path).map_err(|e| {
                    format!(
                        "Archive source cannot be inspected: {}: {e}",
                        path.display()
                    )
                })?;
                if meta.is_file() || (meta.is_dir() && path != root) {
                    let name = path
                        .strip_prefix(root.parent().unwrap_or(root))
                        .map_err(|e| format!("Archive source path error: {e}"))?;
                    sources.push(source(&path, name, &meta)?);
                }
                if meta.is_dir() {
                    // Recheck immediately before directory I/O, including roots.
                    authorize(&path, self.policy)?;
                    let entries = std::fs::read_dir(&path).map_err(|e| {
                        format!("Archive source walk failed: {}: {e}", path.display())
                    })?;
                    for entry in entries {
                        let entry =
                            entry.map_err(|e| format!("Archive source walk failed: {e}"))?;
                        remaining = remaining
                            .checked_sub(1)
                            .ok_or("Archive source entry limit exceeded")?;
                        pending.push(entry.path());
                    }
                } else if !meta.is_file() && path == root {
                    return Err(format!(
                        "Archive source must be a regular file or directory: {}",
                        root.display()
                    ));
                }
            }
        }
        Ok(sources)
    }
}

fn source(path: &Path, name: &Path, meta: &std::fs::Metadata) -> Result<Source, String> {
    Ok(Source {
        path: path.to_path_buf(),
        name: name
            .to_str()
            .ok_or("Archive source name is not valid UTF-8")?
            .to_string(),
        directory: meta.is_dir(),
        bytes: if meta.is_file() { meta.len() } else { 0 },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn requested102_non_utf8_manifest_name_is_refused() {
        use std::os::unix::ffi::OsStrExt;

        // Keep metadata real while testing name conversion independently of
        // filesystem encoding rules: APFS cannot create the invalid name.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fixture.txt");
        std::fs::write(&path, b"fixture").unwrap();
        let metadata = std::fs::metadata(&path).unwrap();
        let name = Path::new(std::ffi::OsStr::from_bytes(b"invalid-\xff.txt"));
        assert!(source(&path, name, &metadata)
            .err()
            .unwrap()
            .contains("not valid UTF-8"));
    }

    #[test]
    fn requested102_manifest_limit_refuses_instead_of_truncating() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("one.txt"), b"one").unwrap();
        std::fs::write(dir.path().join("two.txt"), b"two").unwrap();
        let allow = |_: &Path| Ok(());
        let access = ArchiveAccess {
            app: None,
            policy: Some(&allow),
        };
        let roots = [dir.path().to_str().unwrap().into()];
        assert!(access
            .collect_bounded(&roots, 2)
            .err()
            .unwrap()
            .contains("entry limit exceeded"));
        assert_eq!(access.collect_bounded(&roots, 3).unwrap().len(), 2);
    }

    #[test]
    fn requested102_review_collect_uses_documented_production_limit() {
        // Repeated selection of one real file exercises every production
        // admission without creating 100,000 filesystem objects or a test seam.
        const DOCUMENTED_LIMIT: usize = 100_000;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("fixture.txt");
        std::fs::write(&file, b"fixture").unwrap();
        let allow = |_: &Path| Ok(());
        let access = ArchiveAccess {
            app: None,
            policy: Some(&allow),
        };
        let mut roots = vec![file.to_str().unwrap().to_string(); DOCUMENTED_LIMIT];
        let collected = access.collect(&roots).unwrap().unwrap();
        assert_eq!(collected.len(), DOCUMENTED_LIMIT);
        drop(collected);
        roots.push(file.to_str().unwrap().to_string());
        assert!(access
            .collect(&roots)
            .err()
            .unwrap()
            .contains("entry limit exceeded"));
    }
}
