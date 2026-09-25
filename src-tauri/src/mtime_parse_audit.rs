#![cfg(test)]
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Class-level pin: every consumer of a provider-reported modification time
//! reads it through `parse_remote_mtime` / `parse_remote_datetime` (`lib.rs`).
//!
//! Until 2026-09-25 the sync comparisons had eight independent date parsers.
//! They disagreed: four accepted `YYYY-MM-DD HH:MM`, which is the form an FTP
//! `LIST` date now takes (server-local, no zone), so they would have read a
//! zoneless minute as a UTC instant; the GUI copies read no RFC 2822, so a
//! WebDAV date was silently unknown in the GUI compare while the CLI read it.
//! One parser fixes both, and only stays one if a new copy cannot appear
//! unnoticed: that is the failure mode that recurs (addition), so this test
//! reads the sources and asserts that the date-parsing call sites outside the
//! tests are exactly [`ALLOWED`], counted per file.
//!
//! The allowed sites are the single parser itself, the providers that turn an
//! API field into `RemoteEntry.modified` (producers, not consumers), and dates
//! that are not file times (token expiry, chat history, update checks, user
//! input). Set equality in both directions, so the list cannot rot.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// (file relative to `src/`, call sites outside test modules, why it may parse).
const ALLOWED: &[(&str, usize, &str)] = &[
    ("lib.rs", 6, "parse_remote_mtime itself: the single parser"),
    ("chat_history.rs", 1, "chat timestamps, not file times"),
    (
        "rclone_import.rs",
        1,
        "OAuth token expiry of an imported rclone remote",
    ),
    (
        "providers/drime_cloud.rs",
        2,
        "producer: Drime `updated_at` to `modified`",
    ),
    (
        "providers/jottacloud.rs",
        2,
        "producer: Jottacloud `<modified>` to `modified`",
    ),
    (
        "providers/s3.rs",
        2,
        "producer: `x-amz-meta-mtime` / `Last-Modified` to `modified`",
    ),
    ("providers/sts.rs", 2, "STS credential expiry"),
    ("providers/filen/mod.rs", 1, "producer: Filen metadata time"),
    (
        "providers/github/actions.rs",
        1,
        "GitHub Actions run times, not file times",
    ),
    (
        "bin/aeroftp_cli.rs",
        6,
        "update-check cache, profile last-connected (two), --default-time input (three)",
    ),
];

const PATTERNS: &[&str] = &[
    "parse_from_rfc3339(",
    "parse_from_rfc2822(",
    "NaiveDateTime::parse_from_str(",
    "NaiveDate::parse_from_str(",
    "DateTime::parse_from_str(",
];

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read src") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// `source` with every `#[cfg(test)] mod name { ... }` block removed. Braces
/// inside strings, raw strings, char literals and comments do not count.
fn strip_test_modules(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = String::with_capacity(source.len());
    let mut i = 0;
    while let Some(found) = source[i..].find("#[cfg(test)]") {
        let attr = i + found;
        let after = &source[attr + "#[cfg(test)]".len()..];
        let trimmed = after.trim_start();
        let mut rest = trimmed;
        for prefix in ["pub(crate) ", "pub "] {
            rest = rest.strip_prefix(prefix).unwrap_or(rest);
        }
        let Some(module) = rest.strip_prefix("mod ") else {
            out.push_str(&source[i..attr + "#[cfg(test)]".len()]);
            i = attr + "#[cfg(test)]".len();
            continue;
        };
        let Some(open_rel) = module.find('{') else {
            break;
        };
        if module[..open_rel].contains(';') {
            // `#[cfg(test)] mod name;`: a file of its own, skipped by name.
            out.push_str(&source[i..attr + "#[cfg(test)]".len()]);
            i = attr + "#[cfg(test)]".len();
            continue;
        }
        out.push_str(&source[i..attr]);
        let mut j = source.len() - module.len() + open_rel + 1;
        let mut depth = 1usize;
        while j < bytes.len() && depth > 0 {
            let rest = &source[j..];
            if rest.starts_with("//") {
                j += rest.find('\n').unwrap_or(rest.len());
            } else if rest.starts_with("/*") {
                j += rest.find("*/").map_or(rest.len(), |e| e + 2);
            } else if let Some(hashes) = raw_string_open(source, j) {
                let close = format!("\"{}", "#".repeat(hashes));
                let body = j + 2 + hashes;
                j = body + source[body..].find(&close).map_or(0, |e| e + close.len());
            } else if bytes[j] == b'"' {
                j += 1;
                while j < bytes.len() && bytes[j] != b'"' {
                    j += if bytes[j] == b'\\' { 2 } else { 1 };
                }
                j += 1;
            } else if let Some(len) = char_literal_len(rest) {
                j += len;
            } else {
                match bytes[j] {
                    b'{' => depth += 1,
                    b'}' => depth -= 1,
                    _ => {}
                }
                j += 1;
            }
        }
        i = j;
    }
    out.push_str(&source[i.min(source.len())..]);
    out
}

/// The number of `#` of a raw string opening at `at` (`r"`, `r#"`, ...).
fn raw_string_open(source: &str, at: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    if bytes[at] != b'r'
        || (at > 0 && (bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'_'))
    {
        return None;
    }
    let hashes = bytes[at + 1..].iter().take_while(|b| **b == b'#').count();
    (bytes.get(at + 1 + hashes) == Some(&b'"')).then_some(hashes)
}

/// Length of a char literal at the start of `rest` (`'x'`, `'\n'`, `'{'`),
/// never a lifetime (`'a`).
fn char_literal_len(rest: &str) -> Option<usize> {
    let mut chars = rest.char_indices();
    if chars.next()?.1 != '\'' {
        return None;
    }
    let (_, c) = chars.next()?;
    if c == '\\' {
        let (_, _) = chars.next()?;
    }
    let (end, close) = chars.next()?;
    (close == '\'').then_some(end + 1)
}

fn date_parse_sites() -> BTreeMap<String, usize> {
    let root = src_dir();
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    let mut sites = BTreeMap::new();
    for path in files {
        let rel = path
            .strip_prefix(&root)
            .expect("under src")
            .to_string_lossy()
            .replace('\\', "/");
        let source = std::fs::read_to_string(&path).expect("read source");
        if source.trim_start().starts_with("#![cfg(test)]") || rel.ends_with("_tests.rs") {
            continue;
        }
        let body = strip_test_modules(&source);
        let count: usize = body
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .map(|line| PATTERNS.iter().filter(|p| line.contains(*p)).count().min(1))
            .sum();
        if count > 0 {
            sites.insert(rel, count);
        }
    }
    sites
}

#[test]
fn every_file_time_goes_through_the_single_parser() {
    let found = date_parse_sites();
    let allowed: BTreeMap<String, usize> = ALLOWED
        .iter()
        .map(|(file, count, _)| (file.to_string(), *count))
        .collect();
    assert_eq!(
        found, allowed,
        "\nA date-parsing call site appeared or disappeared outside the tests.\n\
         A consumer of a provider time must use crate::parse_remote_mtime / \
         parse_remote_datetime; a producer or a non-file date goes into ALLOWED \
         with its reason.\n"
    );
}

/// The stripping must see test modules as tests and nothing else, or the
/// count above would be blind: a parser inside `mod tests` is fine, one after
/// it is not.
#[test]
fn the_counter_sees_code_after_a_test_module() {
    let source = r##"
fn a() { let _ = chrono::DateTime::parse_from_rfc3339("x"); }
#[cfg(test)]
mod tests {
    fn t() { let _ = "}"; let _ = '{'; let _ = r#"}"#; /* } */ // }
        let _ = chrono::DateTime::parse_from_rfc3339("y"); }
}
fn b<'a>(s: &'a str) { let _ = chrono::DateTime::parse_from_rfc2822(s); }
"##;
    let body = strip_test_modules(source);
    assert!(body.contains("fn a()"));
    assert!(body.contains("fn b<'a>"));
    assert!(!body.contains("\"y\""));
}
