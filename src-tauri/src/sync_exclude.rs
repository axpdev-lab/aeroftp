//! One exclude matcher for every sync surface (GUI compare, plan and local
//! mirror, AeroCloud, CLI `sync`).
//!
//! Until this module the GUI and the CLI read the same exclude list with two
//! different engines, and each left paths uncovered that the other excluded:
//! the CLI never pruned a directory (`node_modules` did not exclude
//! `node_modules/x`), matched case-sensitively and dropped an invalid pattern in
//! silence; the GUI matched `*.ext` as a literal suffix, never read a glob
//! outside that form (`~*`, `src*`, `cache/**` matched nothing) and took an
//! interior `/` as a literal fragment.
//!
//! The rule here excludes a SUPERSET of what either engine excluded, so no path
//! an exclude protected before becomes eligible for a copy or a `--delete` now:
//!
//! - matching is case-insensitive, on `/`-separated relative paths (`\` is
//!   read as `/`); a trailing `/` on a pattern is dropped;
//! - a pattern without `/` is a glob against EVERY segment of the path, file or
//!   directory, so an excluded directory excludes its whole subtree; it is also
//!   tried against the whole relative path, where `*` crosses `/` (the CLI's
//!   former reading);
//! - a pattern with `/` is a glob against the relative path and against every
//!   contiguous run of its segments, so `build/output` excludes
//!   `a/build/output/x.o` and `**/cache/*` works anywhere; a leading `/`
//!   anchors it at the sync root;
//! - every pattern also matches its own literal text (a name that contains
//!   glob metacharacters), and a pattern that starts with `*` also matches any
//!   path ending with the rest taken literally (the GUI's former reading).
//!
//! An invalid pattern is an error returned to the caller, never dropped.
//! `.aeroignore` keeps its own gitignore engine (`sync_ignore`), which consults
//! this matcher only for the configured exclude list.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use globset::{GlobBuilder, GlobMatcher};

/// A pattern that is not a valid glob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExcludePatternError {
    pub pattern: String,
    pub reason: String,
}

impl std::fmt::Display for ExcludePatternError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "invalid exclude pattern '{}': {}",
            self.pattern, self.reason
        )
    }
}

impl std::error::Error for ExcludePatternError {}

#[derive(Debug, Clone)]
struct CompiledPattern {
    glob: GlobMatcher,
    /// Lowercased pattern without the trailing `/` and the leading anchor.
    literal: String,
    has_slash: bool,
    anchored: bool,
}

/// The compiled exclude list. Build it once per operation.
#[derive(Debug, Clone, Default)]
pub struct ExcludeMatcher {
    patterns: Vec<CompiledPattern>,
}

impl ExcludeMatcher {
    /// Compile `patterns`; blank entries are ignored, an invalid glob is an error.
    pub fn new<S: AsRef<str>>(patterns: &[S]) -> Result<Self, ExcludePatternError> {
        let mut compiled = Vec::with_capacity(patterns.len());
        for raw in patterns {
            let raw = raw.as_ref();
            let trimmed = raw.trim().trim_end_matches(['/', '\\']);
            if trimmed.is_empty() {
                continue;
            }
            let normalized = trimmed.replace('\\', "/").to_lowercase();
            let anchored = normalized.starts_with('/');
            let body = normalized.trim_start_matches('/').to_string();
            if body.is_empty() {
                continue;
            }
            let glob = GlobBuilder::new(&body)
                .case_insensitive(true)
                .literal_separator(false)
                .build()
                .map_err(|e| ExcludePatternError {
                    pattern: raw.to_string(),
                    reason: e.kind().to_string(),
                })?
                .compile_matcher();
            compiled.push(CompiledPattern {
                glob,
                has_slash: anchored || body.contains('/'),
                literal: body,
                anchored,
            });
        }
        Ok(Self { patterns: compiled })
    }

    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    /// Whether the file or directory at `rel_path` (relative to the sync root)
    /// is excluded. A path under an excluded directory is excluded too.
    pub fn is_excluded(&self, rel_path: &str) -> bool {
        if self.patterns.is_empty() {
            return false;
        }
        let norm = rel_path.replace('\\', "/").to_lowercase();
        let norm = norm.trim_matches('/');
        if norm.is_empty() {
            return false;
        }
        let segments: Vec<&str> = norm.split('/').filter(|s| !s.is_empty()).collect();
        self.patterns
            .iter()
            .any(|p| pattern_matches(p, norm, &segments))
    }
}

fn pattern_matches(p: &CompiledPattern, norm: &str, segments: &[&str]) -> bool {
    if let Some(rest) = p.literal.strip_prefix('*') {
        if !rest.is_empty() && norm.ends_with(rest) {
            return true;
        }
    }
    if !p.has_slash {
        return segments
            .iter()
            .any(|seg| *seg == p.literal || p.glob.is_match(seg))
            || p.glob.is_match(norm);
    }
    let starts: &[usize] = if p.anchored { &[0] } else { &[] };
    let all_starts: Vec<usize> = (0..segments.len()).collect();
    let starts = if p.anchored { starts } else { &all_starts[..] };
    for &i in starts {
        for j in (i + 1)..=segments.len() {
            let window = segments[i..j].join("/");
            if window == p.literal || p.glob.is_match(&window) {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The GUI engine before this module (`sync::should_exclude`), verbatim.
    fn old_gui(path: &str, patterns: &[&str]) -> bool {
        let path_lower = path.to_lowercase();
        let path_segments: Vec<&str> = path_lower.split(&['/', '\\'][..]).collect();
        for pattern in patterns {
            let pattern_lower = pattern.to_lowercase();
            let pattern_clean = pattern_lower.trim_end_matches('/');
            if pattern_clean.is_empty() {
                continue;
            }
            if let Some(ext) = pattern_clean.strip_prefix('*') {
                if path_lower.ends_with(ext) {
                    return true;
                }
            } else if pattern_clean.contains('/') {
                let norm = path_lower.replace('\\', "/");
                if norm == pattern_clean
                    || norm.starts_with(&format!("{}/", pattern_clean))
                    || norm.contains(&format!("/{}/", pattern_clean))
                    || norm.ends_with(&format!("/{}", pattern_clean))
                {
                    return true;
                }
            } else {
                for segment in &path_segments {
                    if segment == &pattern_clean {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// The CLI engine before this module (`sync_core::scan` matchers): default
    /// globset options, the relative path or the file name, files only.
    fn old_cli(path: &str, patterns: &[&str]) -> bool {
        let name = path.rsplit('/').next().unwrap_or(path);
        patterns.iter().any(|pat| {
            globset::Glob::new(pat)
                .ok()
                .map(|g| g.compile_matcher())
                .is_some_and(|m| m.is_match(path) || m.is_match(name))
        })
    }

    fn new(path: &str, patterns: &[&str]) -> bool {
        ExcludeMatcher::new(patterns).unwrap().is_excluded(path)
    }

    // (pattern, path, excluded now, old GUI excluded, old CLI excluded)
    const TABLE: &[(&str, &str, bool, bool, bool)] = &[
        // A bare name prunes the directory: the CLI never excluded the children.
        (
            "node_modules",
            "node_modules/react/index.js",
            true,
            true,
            false,
        ),
        ("node_modules", "src/node_modules/a.js", true, true, false),
        ("node_modules/", "node_modules/a.js", true, true, false),
        (".git", ".git/config", true, true, false),
        // Case-insensitive: the CLI missed a different case.
        ("*.TMP", "cache.tmp", true, true, false),
        ("Thumbs.db", "photos/thumbs.db", true, true, false),
        // Globs the GUI read literally and so never matched.
        ("~*", "~lock.docx", true, false, true),
        ("src*", "src2/main.rs", true, false, true),
        ("cache/**", "cache/a/b.bin", true, false, true),
        ("**/build/*.o", "x/build/main.o", true, false, true),
        ("*.{jpg,png}", "img/a.png", true, false, true),
        // A path fragment anywhere: the CLI only matched the whole path.
        ("build/output", "a/build/output/x.o", true, true, false),
        ("build/output", "build/output", true, true, true),
        // Both engines already agreed.
        ("*.pyc", "pkg/__pycache__/m.pyc", true, true, true),
        ("__pycache__", "pkg/__pycache__/m.pyc", true, true, false),
        // `*` crossing `/` on the whole path (the CLI's reading) is kept.
        ("a*b", "a/x/b", true, false, true),
        // A leading `/` anchors at the root.
        ("/build", "build/x.o", true, false, false),
        ("/build", "src/build/x.o", false, false, false),
        // Near misses stay included.
        (
            "node_modules",
            "node_modules_extra/a.js",
            false,
            false,
            false,
        ),
        ("build/output", "build/other/x.o", false, false, false),
        ("*.tmp", "tmp/readme.md", false, false, false),
        ("target", "src/targets.rs", false, false, false),
    ];

    #[test]
    fn table_of_cases_against_the_new_and_both_old_engines() {
        for (pattern, path, now, gui, cli) in TABLE {
            assert_eq!(new(path, &[pattern]), *now, "new: {pattern} vs {path}");
            assert_eq!(
                old_gui(path, &[pattern]),
                *gui,
                "old GUI: {pattern} vs {path}"
            );
            assert_eq!(
                old_cli(path, &[pattern]),
                *cli,
                "old CLI: {pattern} vs {path}"
            );
        }
    }

    #[test]
    fn each_old_engine_missed_something_the_new_one_excludes() {
        assert!(TABLE.iter().any(|(_, _, now, gui, _)| *now && !gui));
        assert!(TABLE.iter().any(|(_, _, now, _, cli)| *now && !cli));
    }

    /// Default excludes shipped with AeroCloud and the sync templates.
    const PRESET_DEFAULTS: &[&str] = &[
        "node_modules",
        ".git",
        ".DS_Store",
        "Thumbs.db",
        "__pycache__",
        "*.pyc",
        ".env",
        "target",
        "*.tmp",
        "*.swp",
        "~*",
        ".aeroignore",
        ".aeroversions",
        "*.aerocorrect",
        "cache/**",
        "*.part",
    ];

    const PROBE_PATHS: &[&str] = &[
        "node_modules/a/b.js",
        "web/node_modules/x/y.js",
        "node_modules",
        ".git",
        ".git/HEAD",
        "sub/.git/objects/ab/cd",
        ".DS_Store",
        "photos/.ds_store",
        "Thumbs.db",
        "pkg/__pycache__/m.cpython.pyc",
        "a.pyc",
        ".env",
        "config/.env",
        "target/debug/app",
        "crates/x/target/release/lib.rlib",
        "doc.tmp",
        "notes.swp",
        "~$report.docx",
        "dir/~lock",
        ".aeroignore",
        ".aeroversions/v1/a.txt",
        "f.bin.aerocorrect",
        "cache/a/b",
        "download.part",
        "src/main.rs",
        "README.md",
        "docs/cache.md",
        "targets/list.txt",
        "Node_Modules/pkg/i.js",
        "a\\b\\node_modules\\c.js",
    ];

    #[test]
    fn preset_defaults_exclude_a_superset_of_both_old_engines() {
        let matcher = ExcludeMatcher::new(PRESET_DEFAULTS).unwrap();
        for path in PROBE_PATHS {
            let before = old_gui(path, PRESET_DEFAULTS) || old_cli(path, PRESET_DEFAULTS);
            if before {
                assert!(
                    matcher.is_excluded(path),
                    "{path} was excluded before and is not now"
                );
            }
        }
        // And the everyday source files stay in.
        for path in [
            "src/main.rs",
            "README.md",
            "docs/cache.md",
            "targets/list.txt",
        ] {
            assert!(!matcher.is_excluded(path), "{path} must stay included");
        }
    }

    #[test]
    fn every_table_row_is_a_superset_of_both_old_engines() {
        for (pattern, path, now, gui, cli) in TABLE {
            if *gui || *cli {
                assert!(*now, "{pattern} vs {path} was excluded before");
            }
        }
    }

    #[test]
    fn an_invalid_pattern_is_an_error_not_a_silent_drop() {
        let err = ExcludeMatcher::new(&["ok", "a[b"]).unwrap_err();
        assert_eq!(err.pattern, "a[b");
        assert!(err.to_string().contains("invalid exclude pattern 'a[b'"));
        assert!(ExcludeMatcher::new(&["", "  ", "/"]).unwrap().is_empty());
    }

    #[test]
    fn literal_names_with_glob_characters_still_match() {
        // `[1]` is a character class as a glob; the literal file name matches too.
        assert!(new("photos/file[1].jpg", &["file[1].jpg"]));
        assert!(new("x/copy[1]", &["*[1]"]));
    }
}
