//! Explicit caller policy carried through recursive sync, including late I/O admission.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

pub trait LocalPathAuthorizer: Send + Sync + std::fmt::Debug {
    fn validate(&self, path: &Path) -> Result<(), String>;
}

/// Resolve existing ancestors without mistaking permission errors or dangling
/// links for missing download parents. This does not pin a future open.
pub(crate) fn resolve_missing(path: &Path) -> Result<PathBuf, String> {
    let mut ancestor = path;
    let mut tail = Vec::new();
    loop {
        match std::fs::symlink_metadata(ancestor) {
            Ok(_) => {
                let mut resolved = std::fs::canonicalize(ancestor)
                    .map_err(|_| "local path resolution refused".to_string())?;
                for part in tail.iter().rev() {
                    resolved.push(part);
                }
                return Ok(resolved);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tail.push(
                    ancestor
                        .file_name()
                        .ok_or("local path resolution refused")?
                        .to_os_string(),
                );
                ancestor = ancestor.parent().ok_or("local path resolution refused")?;
            }
            Err(_) => return Err("local path resolution refused".into()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct LocalAccessScope {
    root: PathBuf,
    resolved_root: PathBuf,
    authorizer: Arc<dyn LocalPathAuthorizer>,
}

#[derive(Debug)]
pub struct AccessRefusal {
    pub rel_path: String,
}

impl LocalAccessScope {
    pub fn new(root: &Path, authorizer: Arc<dyn LocalPathAuthorizer>) -> Result<Self, String> {
        let root = std::path::absolute(root).map_err(|_| "invalid local root")?;
        if root.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err("invalid local root".into());
        }
        authorizer.validate(&root)?;
        let resolved_root = resolve_missing(&root)?;
        Ok(Self {
            root,
            resolved_root,
            authorizer,
        })
    }

    pub fn validate_root(&self, root: &Path) -> Result<(), String> {
        if std::path::absolute(root).map_err(|_| "invalid local root")? != self.root
            || resolve_missing(&self.root)? != self.resolved_root
        {
            return Err("local access root changed".into());
        }
        self.authorizer.validate(&self.root)
    }

    pub fn validate_local(&self, path: &Path) -> Result<(), String> {
        let rel = path
            .strip_prefix(&self.root)
            .map_err(|_| "path outside access scope")?;
        let rel = rel.to_str().ok_or("unrepresentable local path")?;
        self.validate_relative(rel)
            .map_err(|_| "policy_denied".into())
    }

    /// Earliest refused native component protects the entire counterpart prefix.
    pub fn validate_relative(&self, rel: &str) -> Result<(), AccessRefusal> {
        let refusal = |rel_path: String| AccessRefusal { rel_path };
        if self.validate_root(&self.root).is_err() {
            return Err(refusal(String::new()));
        }
        if crate::sync::validate_relative_path(rel).is_err() {
            return Err(refusal(String::new()));
        }
        let mut prefix = PathBuf::new();
        for part in Path::new(rel).components() {
            if !matches!(part, Component::Normal(_)) {
                continue;
            }
            prefix.push(part);
            if self.authorizer.validate(&self.root.join(&prefix)).is_err() {
                let key = crate::sync::normalize_relative_key(
                    prefix.to_str().ok_or_else(|| refusal(String::new()))?,
                );
                return Err(refusal(key));
            }
        }
        Ok(())
    }
}

/// Refresh the inventory union before any planner or orphan probes consume it.
/// Memory for new refusals shares the scanner's bounded boundary budget.
pub(crate) fn preflight(
    opts: &super::scan::ScanOptions,
    locals: &[super::scan::LocalEntry],
    remotes: &[super::scan::RemoteEntry],
    boundaries: &mut super::scan::ScanBoundaries,
) {
    let Some(scope) = opts.local_access.as_deref() else {
        return;
    };
    if scope.validate_root(&scope.root).is_err() {
        boundaries.unbounded.get_or_insert("policy_denied");
        return;
    }
    let cap = opts.max_entries.unwrap_or(super::scan::MAX_SCAN_ENTRIES);
    let mut denied_keys: std::collections::HashSet<String> = boundaries
        .unseen
        .iter()
        .map(|e| e.rel_path.clone())
        .collect();
    for rel in locals
        .iter()
        .map(|e| &e.rel_path)
        .chain(remotes.iter().map(|e| &e.rel_path))
    {
        if let Err(refused) = scope.validate_relative(rel) {
            if refused.rel_path.is_empty() {
                boundaries.unbounded.get_or_insert("policy_denied");
            } else if !denied_keys.contains(&refused.rel_path) {
                if boundaries.unseen.len() + boundaries.links.len() >= cap {
                    boundaries.unbounded.get_or_insert("entry_cap");
                    break;
                }
                denied_keys.insert(refused.rel_path.clone());
                boundaries.unseen(&refused.rel_path, "policy_denied", None);
            }
        }
    }
}

/// Used again immediately before each side effect and each retry.
pub(crate) fn authorize(
    scope: Option<&LocalAccessScope>,
    root: &str,
    rel: &str,
) -> Result<(), String> {
    if let Some(scope) = scope {
        scope.validate_root(Path::new(root))?;
        scope
            .validate_relative(rel)
            .map_err(|_| "policy_denied".to_string())?;
    }
    Ok(())
}
