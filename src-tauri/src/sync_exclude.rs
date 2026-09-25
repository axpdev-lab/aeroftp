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
//! - Paths are relative and read twice: split on `/` only (a `\` is part of
//!   a remote name, as the CLI read it) and with `\` as a separator too (a
//!   Windows path, as the GUI read it); excluded under either is excluded.
//!   Where a walk knows the entry's own name (which may hold a `/`), it is
//!   matched too ([`ExcludeMatcher::is_excluded_entry`]).
//!   A pattern is taken as written: only a trailing `/` is dropped, spaces are
//!   significant, and `\` escapes the next character where globset does so
//!   (as the CLI always read it; not on Windows, globset's platform default).
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
        self.exact.is_match(candidate)
            || self.folded.as_ref().is_some_and(|f| f.is_match(candidate))
    }

    fn text_matches(&self, candidate: &str) -> bool {
        // Whole-string lowercasing, as `literal` was made: per-character
        // lowercasing misses context rules (a final `Σ` becomes `ς`).
        candidate.to_lowercase() == self.literal
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
            let written = pattern_separators(raw, cfg!(windows));
            let without_dir_slash = strip_dir_slashes(&written);
            let mut anchored = without_dir_slash.starts_with('/');
            let mut body = without_dir_slash.trim_start_matches('/');
            if body.is_empty() && raw.contains('\\') {
                // Only separators once a Windows `\\` is read as one (`\\`,
                // `/\\`): nothing is left to name a path, yet globset still
                // matches the pattern as written against a name that is a lone
                // backslash (a remote file), as the CLI did. Kept as written.
                // Elsewhere this is a dangling escape, refused as before.
                body = raw;
                anchored = false;
            }
            if body.is_empty() {
                continue;
            }
            // An anchored pattern is new (neither former engine ever matched
            // one), so it gets path-aware globs: `*` stays inside a segment.
            let exact = GlobBuilder::new(body)
                .literal_separator(anchored)
                .build()
                .map_err(|e| ExcludePatternError {
                    pattern: raw.to_string(),
                    reason: e.kind().to_string(),
                })?
                .compile_matcher();
            let folded = GlobBuilder::new(body)
                .literal_separator(anchored)
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
    ///
    /// The path is read twice. As written, split on `/` only: on a remote a
    /// `\` is a character of the name (`logs/app\2026.log` is a file named
    /// `app\2026.log`), which is how the CLI always read it. And with `\` as
    /// a separator too, which is how the GUI read a Windows path. Excluded
    /// under either reading is excluded.
    pub fn is_excluded(&self, rel_path: &str) -> bool {
        if self.everything {
            return true;
        }
        if self.patterns.is_empty() {
            return false;
        }
        self.matches_split(rel_path)
            || (rel_path.contains('\\') && self.matches_split(&rel_path.replace('\\', "/")))
    }

    /// [`Self::is_excluded`] for an entry whose own name is known. A name can
    /// hold a `/` (Google Drive allows it), and then the path's last segment
    /// is not the name; the CLI matched the entry's real name, so it is
    /// matched here as well.
    pub fn is_excluded_entry(&self, rel_path: &str, name: &str) -> bool {
        self.is_excluded(rel_path)
            || (!self.patterns.is_empty()
                && !name.is_empty()
                && self
                    .patterns
                    .iter()
                    .any(|p| !p.anchored && (p.glob_matches(name) || p.text_matches(name))))
    }

    /// Match `path` split on `/` only.
    fn matches_split(&self, path: &str) -> bool {
        let mut bounds: Vec<(usize, usize)> = Vec::new();
        let mut start = 0;
        for (i, ch) in path.char_indices() {
            if ch == '/' {
                if i > start {
                    bounds.push((start, i));
                }
                start = i + 1;
            }
        }
        if path.len() > start {
            bounds.push((start, path.len()));
        }
        if bounds.is_empty() {
            return false;
        }
        let whole = &path[bounds[0].0..bounds[bounds.len() - 1].1];
        let lower = whole.to_lowercase();
        self.patterns
            .iter()
            .any(|p| pattern_matches(p, path, whole, &lower, &bounds))
    }
}

/// A pattern with its separators as the platform writes them. On Windows a
/// `\\` is not an escape: globset itself turns it into `/` inside the glob.
/// This makes the rest of the matcher agree (whether the pattern holds a
/// separator, the trailing directory marker), so `build\\output` and
/// `node_modules\\` are the paths they look like. Elsewhere a `\\` is left to
/// globset, which reads it as an escape.
fn pattern_separators(raw: &str, windows: bool) -> std::borrow::Cow<'_, str> {
    if windows && raw.contains('\\') {
        std::borrow::Cow::Owned(raw.replace('\\', "/"))
    } else {
        std::borrow::Cow::Borrowed(raw)
    }
}

/// Drop the trailing `/` that marks a directory pattern, but not one escaped
/// by a backslash (`a\\/` is an escaped `/`, and stripping it would leave a
/// dangling escape that no longer compiles).
fn strip_dir_slashes(raw: &str) -> &str {
    let mut end = raw.len();
    while end > 0 && raw.as_bytes()[end - 1] == b'/' {
        let backslashes = raw.as_bytes()[..end - 1]
            .iter()
            .rev()
            .take_while(|b| **b == b'\\')
            .count();
        if backslashes % 2 == 1 {
            break;
        }
        end -= 1;
    }
    &raw[..end]
}

/// `bounds` index `path`; `whole` is `path` from its first to its last segment.
fn pattern_matches(
    p: &CompiledPattern,
    path: &str,
    whole: &str,
    lower: &str,
    bounds: &[(usize, usize)],
) -> bool {
    // The GUI's former `*X` suffix reading. An anchored pattern (`/*.tmp`)
    // never had it: it names root-level entries only.
    if !p.anchored {
        if let Some(rest) = p.literal.strip_prefix('*') {
            if lower.ends_with(rest) {
                return true;
            }
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
    // An anchored pattern starts at the root, and also right after an empty
    // segment (`a//b`, or `a\\/b` read with `\\` as a separator): the GUI
    // matched `/b` as the text `/b` there, and that exclusion is kept.
    let starts_at_a_root = |i: usize| {
        i == 0 || {
            let start = bounds[i].0;
            start >= 2 && &path[start - 2..start] == "//"
        }
    };
    for i in 0..bounds.len() {
        if p.anchored && !starts_at_a_root(i) {
            continue;
        }
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
    /// globset options, the relative path or the entry's own name, files only.
    fn old_cli(path: &str, name: &str, patterns: &[&str]) -> bool {
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

    /// The entry name a walk reports for `path` when the name holds no `/`:
    /// the last `/`-separated component, a `\` included (a remote name).
    fn last_name(path: &str) -> &str {
        path.rsplit('/').next().unwrap_or(path)
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
        // Character classes keep their case-sensitive meaning too.
        ("[!a-z]*", "README", true, false, true),
        ("[!A-Z]*", "readme.md", true, false, true),
        ("*[!a-z0-9.]*", "Notes.txt", true, false, true),
        ("[Z-a]*", "_x", true, false, true),
        // A `\` inside a remote name is a character of the name, which the CLI
        // matched (review of #939: reading it as `/` turned the file into a
        // path, and `--delete` removed it as an orphan).
        ("app*.log", "logs/app\\2026.log", true, false, true),
        ("app*.log", "app\\2026.log", true, false, true),
        // A final sigma lowercases in context (`Σ` at the end is `ς`), so the
        // literal name must be lowercased as a whole, as the GUI did.
        ("ΑΣ[1]", "docs/ΑΣ[1]", true, true, false),
        // A `\\` before a `/` reads as an empty segment with `\\` as a
        // separator, and the GUI matched `/a` as the text `/a` after it (found
        // by the property test).
        ("/a", "z\\/a", true, true, false),
    ];

    /// Backslash escapes, as the CLI read it. globset escapes with `\` only
    /// off Windows (its platform default, which the former CLI shared), so on
    /// Windows these patterns hold a literal backslash for both engines.
    #[cfg(not(windows))]
    const ESCAPES: &[(&str, &str, bool, bool, bool)] = &[
        ("a\\*", "a*", true, false, true),
        ("file\\[1\\].jpg", "photos/file[1].jpg", true, false, true),
        ("a\\\\b", "x/a\\b", true, false, true),
        ("*\\\\*", "x/a\\b", true, false, true),
    ];
    #[cfg(windows)]
    const ESCAPES: &[(&str, &str, bool, bool, bool)] = &[];

    fn all_rows() -> impl Iterator<Item = &'static (&'static str, &'static str, bool, bool, bool)> {
        TABLE.iter().chain(ESCAPES.iter())
    }

    #[test]
    fn table_of_cases_against_the_new_and_both_old_engines() {
        for (pattern, path, now, gui, cli) in all_rows() {
            assert_eq!(new(path, &[pattern]), *now, "new: {pattern} vs {path}");
            assert_eq!(
                old_gui(path, &[pattern]),
                *gui,
                "old GUI: {pattern} vs {path}"
            );
            assert_eq!(
                old_cli(path, last_name(path), &[pattern]),
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
            let before =
                old_gui(path, PRESET_DEFAULTS) || old_cli(path, last_name(path), PRESET_DEFAULTS);
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
        for (pattern, path, now, gui, cli) in all_rows() {
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
        // A pattern that compiled for the former CLI still compiles, including
        // one that ends with an escaped `/`.
        assert!(ExcludeMatcher::new(&["[Z-a]*"]).is_ok());
        assert!(ExcludeMatcher::new(&["[Z-a]A\\/"]).is_ok());
    }

    #[test]
    fn an_anchored_pattern_names_root_level_entries_only() {
        let m = ExcludeMatcher::new(&["/*.tmp"]).unwrap();
        assert!(m.is_excluded("a.tmp"));
        assert!(!m.is_excluded("sub/a.tmp"));
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
        "a",
        "b",
        "A",
        "B",
        "z",
        "Z",
        "_",
        ".",
        " ",
        "*",
        "?",
        "**",
        "/",
        "\\",
        "[a-z]",
        "[!a-z]",
        "[A-Z]",
        "[!A-Z]",
        "[Z-a]",
        "[ab]",
        "{a,b}",
        "node_modules",
        "tmp",
    ];
    const SEGMENT_ATOMS: &[&str] = &[
        "\\",
        "Σ",
        "İ",
        "a",
        "b",
        "A",
        "B",
        "z",
        "Z",
        "_",
        ".",
        " ",
        "*",
        "[",
        "]",
        "node_modules",
        "tmp",
    ];

    fn random_pattern(rng: &mut Lcg) -> String {
        (0..rng.upto(4)).map(|_| rng.pick(PATTERN_ATOMS)).collect()
    }

    fn random_segment(rng: &mut Lcg) -> String {
        (0..rng.upto(3)).map(|_| rng.pick(SEGMENT_ATOMS)).collect()
    }

    /// A relative path and the name its walk reports: directories, then a
    /// name that now and then holds a `/` (Google Drive allows one), in which
    /// case the path's last segment is not the name.
    fn random_entry(rng: &mut Lcg) -> (String, String) {
        let mut name = random_segment(rng);
        if rng.upto(5) == 0 {
            name = format!("{name}/{}", random_segment(rng));
        }
        let mut parts: Vec<String> = (0..rng.upto(2)).map(|_| random_segment(rng)).collect();
        parts.push(name.clone());
        (parts.join("/"), name)
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
            // The former CLI oracle, compiled once per pattern (same reading as
            // `old_cli`: default options, the path or the file name).
            let old_cli_glob = globset::Glob::new(&pattern)
                .ok()
                .map(|g| g.compile_matcher());
            for _ in 0..24 {
                let (path, name) = random_entry(&mut rng);
                let cli_before = old_cli_glob
                    .as_ref()
                    .is_some_and(|m| m.is_match(&path) || m.is_match(&name));
                if old_gui(&path, &[pattern.as_str()]) || cli_before {
                    assert!(
                        matcher.is_excluded_entry(&path, &name),
                        "{pattern:?} excluded {path:?} (name {name:?}) before and not now"
                    );
                    // A walk that knows only the path loses nothing either,
                    // as long as the name holds no `/`.
                    if !name.contains('/') {
                        assert!(
                            matcher.is_excluded(&path),
                            "{pattern:?} excluded {path:?} before and not now (path only)"
                        );
                    }
                    checked += 1;
                }
            }
        }
        // The generator must actually exercise the promise.
        assert!(
            checked > 5_000,
            "only {checked} former exclusions generated"
        );
    }

    #[test]
    fn everything_excludes_every_path_and_is_not_empty() {
        let all = ExcludeMatcher::everything();
        assert!(all.excludes_everything());
        assert!(!all.is_empty());
        assert!(all.is_excluded("src/main.rs"));
    }

    /// On Windows a `\\` in a pattern is a separator: it cannot be an escape
    /// there, and `build\\output` used to match nothing at all.
    #[test]
    fn a_windows_pattern_reads_backslash_as_a_separator() {
        assert_eq!(pattern_separators("build\\output", true), "build/output");
        assert_eq!(pattern_separators("node_modules\\", true), "node_modules/");
        assert_eq!(pattern_separators("a\\*", false), "a\\*");
        #[cfg(windows)]
        {
            assert!(new("src/build/output/x.o", &["build\\output"]));
            assert!(new("web/node_modules/x.js", &["node_modules\\"]));
            // A pattern of backslashes only still names a remote file called
            // `\\`, as the former CLI read it (found by the property test).
            assert!(new(" A_/\\", &["\\"]));
        }
        #[cfg(not(windows))]
        assert!(
            ExcludeMatcher::new(&["\\"]).is_err(),
            "a lone backslash is a dangling escape off Windows, as before"
        );
    }

    /// A name holding a `/` (Google Drive) is matched as the entry's own name,
    /// as the CLI did, where the path alone would split it.
    #[test]
    fn a_name_with_a_slash_is_matched_as_a_name() {
        let m = ExcludeMatcher::new(&["x?y", "report*.pdf"]).unwrap();
        assert!(!m.is_excluded("d/x/y"));
        assert!(m.is_excluded_entry("d/x/y", "x/y"));
        assert!(m.is_excluded_entry("d/report 1/2.pdf", "report 1/2.pdf"));
        assert!(old_cli("d/x/y", "x/y", &["x?y"]));
        // An anchored pattern names a root entry; the bare name is not one.
        let anchored = ExcludeMatcher::new(&["/y"]).unwrap();
        assert!(!anchored.is_excluded_entry("d/y", "y"));
    }

    #[test]
    fn literal_names_with_glob_characters_still_match() {
        // `[1]` is a character class as a glob; the literal file name matches too.
        assert!(new("photos/file[1].jpg", &["file[1].jpg"]));
        assert!(new("x/copy[1]", &["*[1]"]));
    }
}
