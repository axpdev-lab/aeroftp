//! One exclude matcher for every sync surface (GUI compare, plan and local
//! mirror, AeroCloud, CLI `sync`, `sync --watch`, `sync-doctor`, `reconcile`,
//! and the AeroAgent / MCP sync tools).
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
//! an exclude protected before becomes eligible for a copy or a `--delete` now.
//! A deterministic property test pins that promise against verbatim copies of
//! both former engines (kept in the tests only, as an oracle).
//!
//! - Paths are `/`-separated and relative (`\` in a path is read as `/`).
//!   A pattern is taken as written: only a trailing `/` is dropped, spaces are
//!   significant, and `\` escapes the next character where globset does so
//!   (as the CLI always read it).
//! - A glob matches as written, and also case-insensitively (globset's own
//!   option, so character classes keep their meaning).
//! - A pattern without `/` is tried against EVERY segment of the path, file or
//!   directory, so an excluded directory excludes its whole subtree, and against
//!   the whole relative path, where `*` crosses `/`.
//! - A pattern with `/` is tried against every contiguous run of segments, so
//!   `build/output` excludes `a/build/output/x.o` and `**/cache/*` works
//!   anywhere; a leading `/` anchors it at the sync root.
//! - A pattern also matches its own text case-insensitively (a segment, or a
//!   run of segments, spelled exactly like it), and a pattern `*X` also matches
//!   any path ending with `X` taken literally.
//!
//! A pattern that is not a valid glob is an error returned to the caller, never
//! dropped. `.aeroignore` keeps its own gitignore engine (`sync_ignore`), which
//! consults this matcher only for the configured exclude list.

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
    /// The glob as written (case-sensitive).
    exact: GlobMatcher,
    /// The same glob with globset's case-insensitive option; `None` when it
    /// only compiles case-sensitively.
    folded: Option<GlobMatcher>,
    /// Lowercased text of the pattern, without the trailing `/` or the anchor.
    literal: String,
    has_slash: bool,
    anchored: bool,
}

impl CompiledPattern {
    fn glob_matches(&self, candidate: &str) -> bool {
        self.exact.is_match(candidate) || self.folded.as_ref().is_some_and(|f| f.is_match(candidate))
    }

    fn text_matches(&self, candidate: &str) -> bool {
        candidate
            .chars()
            .flat_map(char::to_lowercase)
            .eq(self.literal.chars())
    }
}

/// The compiled exclude list. Build it once per operation.
#[derive(Debug, Clone, Default)]
pub struct ExcludeMatcher {
    patterns: Vec<CompiledPattern>,
    everything: bool,
}

impl ExcludeMatcher {
    /// Compile `patterns`; empty entries are ignored, an invalid glob is an error.
    pub fn new<S: AsRef<str>>(patterns: &[S]) -> Result<Self, ExcludePatternError> {
        let mut compiled = Vec::with_capacity(patterns.len());
        for raw in patterns {
            let raw = raw.as_ref();
            let without_dir_slash = raw.trim_end_matches('/');
            let anchored = without_dir_slash.starts_with('/');
            let body = without_dir_slash.trim_start_matches('/');
            if body.is_empty() {
                continue;
            }
            let exact = GlobBuilder::new(body)
                .literal_separator(false)
                .build()
                .map_err(|e| ExcludePatternError {
                    pattern: raw.to_string(),
                    reason: e.kind().to_string(),
                })?
                .compile_matcher();
            let folded = GlobBuilder::new(body)
                .literal_separator(false)
                .case_insensitive(true)
                .build()
                .ok()
                .map(|g| g.compile_matcher());
            compiled.push(CompiledPattern {
                exact,
                folded,
                literal: body.to_lowercase(),
                has_slash: anchored || body.contains('/'),
                anchored,
            });
        }
        Ok(Self {
            patterns: compiled,
            everything: false,
        })
    }

    /// A matcher that excludes every path: the fail-closed stand-in for a list
    /// that could not be compiled where no error can be returned.
    pub fn everything() -> Self {
        Self {
            patterns: Vec::new(),
            everything: true,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty() && !self.everything
    }

    /// Whether this matcher excludes everything (see [`Self::everything`]).
    pub fn excludes_everything(&self) -> bool {
        self.everything
    }

    /// Whether the file or directory at `rel_path` (relative to the sync root)
    /// is excluded. A path under an excluded directory is excluded too.
    pub fn is_excluded(&self, rel_path: &str) -> bool {
        if self.everything {
            return true;
        }
        if self.patterns.is_empty() {
            return false;
        }
        let normalized = rel_path.replace('\\', "/");
        let mut bounds: Vec<(usize, usize)> = Vec::new();
        let mut start = 0;
        for (i, ch) in normalized.char_indices() {
            if ch == '/' {
                if i > start {
                    bounds.push((start, i));
                }
                start = i + 1;
            }
        }
        if normalized.len() > start {
            bounds.push((start, normalized.len()));
        }
        if bounds.is_empty() {
            return false;
        }
        let whole = &normalized[bounds[0].0..bounds[bounds.len() - 1].1];
        let lower = whole.to_lowercase();
        self.patterns
            .iter()
            .any(|p| pattern_matches(p, whole, &lower, &bounds))
    }
}

fn pattern_matches(p: &CompiledPattern, whole: &str, lower: &str, bounds: &[(usize, usize)]) -> bool {
    if let Some(rest) = p.literal.strip_prefix('*') {
        if lower.ends_with(rest) {
            return true;
        }
    }
    // `bounds` index the original path, `whole` starts at bounds[0].0.
    let base = bounds[0].0;
    let slice = |i: usize, j: usize| &whole[bounds[i].0 - base..bounds[j].1 - base];
    if !p.has_slash {
        return (0..bounds.len()).any(|i| {
            let seg = slice(i, i);
            p.glob_matches(seg) || p.text_matches(seg)
        }) || p.glob_matches(whole);
    }
    let last_start = if p.anchored { 0 } else { bounds.len() - 1 };
    for i in 0..=last_start {
        for j in i..bounds.len() {
            let window = slice(i, j);
            if p.glob_matches(window) || p.text_matches(window) {
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
        // Spaces are significant (review of #939: a trim dropped these).
        ("node_modules ", "node_modules /a.js", true, true, false),
        ("*.tmp ", "a.tmp ", true, true, true),
        (" build", " build/x", true, true, false),
        // Backslash escapes, as the CLI read it.
        ("a\\*", "a*", true, false, true),
        ("file\\[1\\].jpg", "photos/file[1].jpg", true, false, true),
        // Character classes keep their case-sensitive meaning too.
        ("[!a-z]*", "README", true, false, true),
        ("[!A-Z]*", "readme.md", true, false, true),
        ("*[!a-z0-9.]*", "Notes.txt", true, false, true),
        ("[Z-a]*", "_x", true, false, true),
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
        assert!(ExcludeMatcher::new(&["", "/", "//"]).unwrap().is_empty());
        // A pattern that compiled for the former CLI still compiles.
        assert!(ExcludeMatcher::new(&["[Z-a]*"]).is_ok());
    }

    /// Deterministic generator (fixed seed): patterns and paths over an alphabet
    /// that holds the characters the three former-engine gaps turned on
    /// (spaces, `\`, classes with `!` and ranges, both cases, `/`, `*`, `?`).
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0 >> 33
        }
        fn pick<'a>(&mut self, items: &'a [&'a str]) -> &'a str {
            items[(self.next() as usize) % items.len()]
        }
        fn upto(&mut self, max: u64) -> usize {
            (1 + self.next() % max) as usize
        }
    }

    const PATTERN_ATOMS: &[&str] = &[
        "a", "b", "A", "B", "z", "Z", "_", ".", " ", "*", "?", "**", "/", "\\", "[a-z]", "[!a-z]",
        "[A-Z]", "[!A-Z]", "[Z-a]", "[ab]", "{a,b}", "node_modules", "tmp",
    ];
    const SEGMENT_ATOMS: &[&str] = &[
        "a", "b", "A", "B", "z", "Z", "_", ".", " ", "*", "[", "]", "node_modules", "tmp",
    ];

    fn random_pattern(rng: &mut Lcg) -> String {
        (0..rng.upto(4)).map(|_| rng.pick(PATTERN_ATOMS)).collect()
    }

    fn random_path(rng: &mut Lcg) -> String {
        (0..rng.upto(3))
            .map(|_| (0..rng.upto(3)).map(|_| rng.pick(SEGMENT_ATOMS)).collect::<String>())
            .collect::<Vec<_>>()
            .join("/")
    }

    #[test]
    fn property_every_path_either_former_engine_excluded_stays_excluded() {
        let mut rng = Lcg(0x5eed_a3f7_2026_0925);
        let mut checked = 0usize;
        for _ in 0..4000 {
            let pattern = random_pattern(&mut rng);
            let matcher = match ExcludeMatcher::new(&[pattern.as_str()]) {
                Ok(m) => m,
                Err(_) => {
                    // Refusing a pattern is allowed only where the former CLI
                    // could not compile it either.
                    assert!(
                        globset::Glob::new(&pattern).is_err(),
                        "{pattern:?} compiled before and is refused now"
                    );
                    continue;
                }
            };
            for _ in 0..24 {
                let path = random_path(&mut rng);
                if old_gui(&path, &[pattern.as_str()]) || old_cli(&path, &[pattern.as_str()]) {
                    assert!(
                        matcher.is_excluded(&path),
                        "{pattern:?} excluded {path:?} before and not now"
                    );
                    checked += 1;
                }
            }
        }
        // The generator must actually exercise the promise.
        assert!(checked > 5_000, "only {checked} former exclusions generated");
    }

    #[test]
    fn everything_excludes_every_path_and_is_not_empty() {
        let all = ExcludeMatcher::everything();
        assert!(all.excludes_everything());
        assert!(!all.is_empty());
        assert!(all.is_excluded("src/main.rs"));
    }

    #[test]
    fn literal_names_with_glob_characters_still_match() {
        // `[1]` is a character class as a glob; the literal file name matches too.
        assert!(new("photos/file[1].jpg", &["file[1].jpg"]));
        assert!(new("x/copy[1]", &["*[1]"]));
    }
}
