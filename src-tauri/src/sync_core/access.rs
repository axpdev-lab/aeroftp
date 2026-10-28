//! Explicit caller policy carried through recursive sync, including late I/O admission.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

pub trait LocalPathAuthorizer: Send + Sync + std::fmt::Debug {
    fn validate(&self, path: &Path) -> Result<(), String>;

    /// Validate one admission's root and ordered prefixes. Implementations may
    /// share fresh policy resolutions within this call, never across admissions.
    /// The first failing index identifies the counterpart prefix to protect.
    fn validate_many(&self, paths: &mut dyn Iterator<Item = PathBuf>) -> Result<(), usize> {
        for (index, path) in paths.enumerate() {
            self.validate(&path).map_err(|_| index)?;
        }
        Ok(())
    }
}

#[cfg(test)]
thread_local! {
    pub(crate) static RESOLUTION_COUNT: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// Resolve existing ancestors without mistaking permission errors or dangling
/// links for missing download parents. This does not pin a future open.
pub(crate) fn resolve_missing(path: &Path) -> Result<PathBuf, String> {
    let mut ancestor = path;
    let mut tail = Vec::new();
    loop {
        match std::fs::symlink_metadata(ancestor) {
            Ok(_) => {
                #[cfg(test)]
                RESOLUTION_COUNT.with(|count| count.set(count.get().map(|n| n + 1)));
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
        self.check_root(root)?;
        self.authorizer.validate(&self.root)
    }

    fn check_root(&self, root: &Path) -> Result<(), String> {
        if std::path::absolute(root).map_err(|_| "invalid local root")? != self.root
            || resolve_missing(&self.root)? != self.resolved_root
        {
            return Err("local access root changed".into());
        }
        Ok(())
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
        self.validate_relative_at(&self.root, rel)
    }

    fn validate_relative_at(&self, root: &Path, rel: &str) -> Result<(), AccessRefusal> {
        let refusal = |rel_path: String| AccessRefusal { rel_path };
        if self.check_root(root).is_err() {
            return Err(refusal(String::new()));
        }
        if crate::sync::validate_relative_path(rel).is_err() {
            return Err(refusal(String::new()));
        }
        let mut prefix = PathBuf::new();
        let mut paths = std::iter::once(self.root.clone()).chain(
            Path::new(rel)
                .components()
                .filter(|part| matches!(part, Component::Normal(_)))
                .map(|part| {
                    prefix.push(part);
                    self.root.join(&prefix)
                }),
        );
        if let Err(index) = self.authorizer.validate_many(&mut paths) {
            let mut parts = Path::new(rel)
                .components()
                .filter(|part| matches!(part, Component::Normal(_)));
            let mut denied = PathBuf::new();
            for _ in 0..index {
                denied.push(parts.next().ok_or_else(|| refusal(String::new()))?);
            }
            let key = denied.to_str().ok_or_else(|| refusal(String::new()))?;
            return Err(refusal(crate::sync::normalize_relative_key(key)));
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
        boundaries.unbounded = Some("policy_denied");
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
                boundaries.unbounded = Some("policy_denied");
            } else if !denied_keys.contains(&refused.rel_path) {
                if boundaries.unseen.len() + boundaries.links.len() >= cap {
                    // A policy refusal without room for its counterpart
                    // boundary cannot become an ordinary partial comparison.
                    boundaries.unbounded = Some("policy_denied");
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
        scope
            .validate_relative_at(Path::new(root), rel)
            .map_err(|_| "policy_denied".to_string())?;
    }
    Ok(())
}
