//! .aeroignore: gitignore-style pattern exclusion for AeroCloud sync.
//!
//! Reads a `.aeroignore` file from the sync root directory and provides
//! pattern matching compatible with `.gitignore` / `.stignore` syntax:
//! - `#` comments
//! - `*` and `**` globs
//! - `!` negation (re-include previously excluded)
//! - Trailing `/` matches directories only
//! - Case-sensitive on Linux, case-insensitive on Windows/macOS

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use globset::Glob;
use std::path::Path;

/// A compiled .aeroignore rule: pattern + whether it negates (re-includes).
#[derive(Debug, Clone)]
struct IgnoreRule {
    /// Original pattern text (for debugging/display)
    _pattern: String,
    /// Whether this is a negation rule (starts with `!`)
    negated: bool,
    /// Whether this rule only applies to directories (ends with `/`)
    dir_only: bool,
}

/// Parsed and compiled .aeroignore file.
#[derive(Debug)]
pub struct AeroIgnore {
    /// Rules in file order: needed for negation precedence (last-match-wins)
    rules: Vec<IgnoreRule>,
    /// Individual compiled globs matching the rules (same indices)
    individual_globs: Vec<globset::GlobMatcher>,
}

/// Default .aeroignore template with common patterns (commented out).
pub const DEFAULT_AEROIGNORE_TEMPLATE: &str = "\
# AeroCloud ignore file: uncomment patterns as needed
# Syntax: same as .gitignore
#
# node_modules/
# .git/
# *.tmp
# *.log
# *.swp
# __pycache__/
# target/
# .DS_Store
# Thumbs.db
";

impl AeroIgnore {
    /// Load and parse `.aeroignore` from the given sync root directory.
    /// Returns `None` if the file doesn't exist or is empty.
    pub fn load(sync_root: &Path) -> Option<Self> {
        let path = sync_root.join(".aeroignore");
        let content = std::fs::read_to_string(&path).ok()?;
        Self::parse(&content)
    }

    /// Parse .aeroignore content from a string.
    pub fn parse(content: &str) -> Option<Self> {
        let mut rules = Vec::new();
        let mut individual_globs = Vec::new();
        let mut has_patterns = false;

        for line in content.lines() {
            let trimmed = line.trim();

            // Skip empty lines and comments
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }

            let (negated, pattern) = if let Some(rest) = trimmed.strip_prefix('!') {
                (true, rest.trim())
            } else {
                (false, trimmed)
            };

            if pattern.is_empty() {
                continue;
            }

            // Check for directory-only marker
            let dir_only = pattern.ends_with('/');
            let clean = pattern.trim_end_matches('/');

            // Build glob pattern:
            // - If pattern contains `/`, it's anchored to root
            // - Otherwise, match anywhere in the path (prepend `**/`)
            let glob_pattern = if clean.contains('/') {
                clean.to_string()
            } else {
                format!("**/{}", clean)
            };

            match Glob::new(&glob_pattern) {
                Ok(glob) => {
                    individual_globs.push(glob.compile_matcher());
                    rules.push(IgnoreRule {
                        _pattern: trimmed.to_string(),
                        negated,
                        dir_only,
                    });
                    has_patterns = true;
                }
                Err(e) => {
                    tracing::warn!(".aeroignore: invalid pattern '{}': {}", trimmed, e);
                }
            }
        }

        if !has_patterns {
            return None;
        }

        Some(Self {
            rules,
            individual_globs,
        })
    }

    /// Check whether a relative path should be ignored.
    ///
    /// Uses last-match-wins semantics (like .gitignore):
    /// if a path matches both an exclude and a `!` re-include pattern,
    /// the last matching rule in the file determines the outcome.
    pub fn is_ignored(&self, relative_path: &str, is_dir: bool) -> bool {
        let normalized = relative_path.replace('\\', "/");
        let mut ignored = false;

        for (i, rule) in self.rules.iter().enumerate() {
            // Skip directory-only rules when checking a file
            if rule.dir_only && !is_dir {
                continue;
            }

            if self.individual_globs[i].is_match(&normalized) {
                ignored = !rule.negated;
            }
        }

        ignored
    }

    /// Check whether a path should be excluded, considering both
    /// .aeroignore rules AND config exclude_patterns.
    /// .aeroignore `!` negation overrides config patterns.
    pub fn should_exclude(
        &self,
        relative_path: &str,
        is_dir: bool,
        config_patterns: &[String],
    ) -> bool {
        // First check .aeroignore (has negation support)
        let aeroignore_result = self.is_ignored(relative_path, is_dir);

        // If .aeroignore explicitly re-includes with `!`, that wins
        // Check if the last matching rule was a negation
        let normalized = relative_path.replace('\\', "/");
        let mut last_match_negated = false;
        let mut had_match = false;
        for (i, rule) in self.rules.iter().enumerate() {
            if rule.dir_only && !is_dir {
                continue;
            }
            if self.individual_globs[i].is_match(&normalized) {
                last_match_negated = rule.negated;
                had_match = true;
            }
        }

        // If .aeroignore explicitly negated (re-included), skip config check
        if had_match && last_match_negated {
            return false;
        }

        // If .aeroignore says ignore, it's ignored
        if aeroignore_result {
            return true;
        }

        // Fall back to config exclude_patterns
        crate::sync::should_exclude(relative_path, config_patterns)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_patterns() {
        let ignore = AeroIgnore::parse("*.tmp\nnode_modules/\n.git/").unwrap();

        assert!(ignore.is_ignored("file.tmp", false));
        assert!(ignore.is_ignored("deep/path/file.tmp", false));
        assert!(ignore.is_ignored("node_modules", true));
        assert!(!ignore.is_ignored("node_modules_extra", false));
        assert!(ignore.is_ignored(".git", true));
        assert!(!ignore.is_ignored("file.txt", false));
    }

    #[test]
    fn test_negation() {
        let ignore = AeroIgnore::parse("*.log\n!important.log").unwrap();

        assert!(ignore.is_ignored("debug.log", false));
        assert!(!ignore.is_ignored("important.log", false));
    }

    #[test]
    fn test_dir_only() {
        let ignore = AeroIgnore::parse("build/").unwrap();

        assert!(ignore.is_ignored("build", true));
        // dir_only rule should NOT match files
        assert!(!ignore.is_ignored("build", false));
    }

    #[test]
    fn test_comments_and_empty() {
        let ignore = AeroIgnore::parse("# comment\n\n  # another\n*.tmp").unwrap();
        assert!(ignore.is_ignored("test.tmp", false));
    }

    #[test]
    fn test_empty_file() {
        assert!(AeroIgnore::parse("").is_none());
        assert!(AeroIgnore::parse("# only comments").is_none());
    }
}
