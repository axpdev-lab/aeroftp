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

use globset::GlobBuilder;
use std::path::Path;

/// Compile one `.aeroignore` glob with the module's matching policy:
/// `literal_separator` so a lone `*`/`?` never crosses `/` (gitignore-style;
/// `**` still spans directories), plus case-insensitivity on Windows/macOS as
/// the module doc-comment promises. Returns `None` if globset rejects it.
/// CLAUDE-AV-B3-03.
fn build_ignore_matcher(pattern: &str) -> Option<globset::GlobMatcher> {
    GlobBuilder::new(pattern)
        .literal_separator(true)
        .case_insensitive(cfg!(any(windows, target_os = "macos")))
        .build()
        .ok()
        .map(|g| g.compile_matcher())
}

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

            // CLAUDE-AV-B3-03: gitignore-style anchoring. A leading '/' or an
            // interior '/' anchors the pattern to the sync root; a bare name
            // matches in any directory (prepend `**/`). The leading '/' is
            // stripped because relative paths carry no leading slash, so the
            // pre-fix code compiled `/secrets` to a glob that never matched.
            let anchored = clean.contains('/');
            let core = clean.strip_prefix('/').unwrap_or(clean);
            let glob_pattern = if anchored {
                core.to_string()
            } else {
                format!("**/{}", core)
            };

            let matcher = build_ignore_matcher(&glob_pattern).or_else(|| {
                // CLAUDE-AV-B3-03: fail CLOSED. A pattern globset rejects (bad
                // char class, stray bracket, ...) must NOT silently vanish and
                // let the file it names get synced/deleted. Retry it as a
                // literal so the user's intended path is still excluded.
                tracing::warn!(
                    ".aeroignore: invalid glob '{}', matching it literally",
                    trimmed
                );
                let literal = if anchored {
                    globset::escape(core)
                } else {
                    format!("**/{}", globset::escape(core))
                };
                build_ignore_matcher(&literal)
            });

            match matcher {
                Some(m) => {
                    individual_globs.push(m);
                    rules.push(IgnoreRule {
                        _pattern: trimmed.to_string(),
                        negated,
                        dir_only,
                    });
                    has_patterns = true;
                }
                None => {
                    tracing::warn!(".aeroignore: unparseable pattern '{}' dropped", trimmed);
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

    /// CLAUDE-AV-B3-03 (F2-06): a leading-slash pattern anchors to the sync
    /// root. Pre-fix `/secrets` compiled verbatim and never matched a relative
    /// path (which has no leading slash), silently syncing the secrets dir.
    #[test]
    fn leading_slash_anchors_to_root() {
        let ignore = AeroIgnore::parse("/secrets").unwrap();
        assert!(ignore.is_ignored("secrets", true));
        assert!(!ignore.is_ignored("sub/secrets", true));
    }

    /// CLAUDE-AV-B3-03 (F2-07): a lone `*` must not cross `/`. Pre-fix
    /// `foo*bar` compiled without `literal_separator` and over-matched across
    /// directories, silently excluding files the user meant to sync.
    #[test]
    fn star_does_not_cross_separator() {
        let ignore = AeroIgnore::parse("foo*bar").unwrap();
        assert!(ignore.is_ignored("fooXYZbar", false));
        assert!(!ignore.is_ignored("foo/deep/bar", false));
    }

    /// CLAUDE-AV-B3-03 (F2-04): a pattern globset rejects must fail CLOSED
    /// (still exclude its literal), not silently vanish. Pre-fix `sec[ret`
    /// (unclosed char class) was dropped, so the file it named got synced.
    #[test]
    fn invalid_pattern_fails_closed_as_literal() {
        let ignore = AeroIgnore::parse("sec[ret").expect("fail-closed keeps the rule");
        assert!(ignore.is_ignored("sec[ret", false));
    }
}
