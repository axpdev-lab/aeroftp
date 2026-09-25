//! Structural guard for the path resolution of Google Drive and Zoho
//! WorkDrive.
//!
//! Both providers turn a path into a folder id in many places (25 in Drive,
//! 15 in Zoho). Until 2026-09-25 every site carried its own copy of the rule,
//! and the copies had drifted: a one-segment path went to the current folder
//! even with a leading slash, the Drive downloads sent it to the root, and a
//! longer relative path always went to the root. The rule now lives in one
//! `parent_folder_id` per provider. No HTTP double can drive these providers
//! (their token comes from the OAuth manager), so this test is what keeps the
//! sites from diverging again: it reads the production source and fails when
//! a path is resolved outside the functions allowed to do it.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/// The production part of a provider source: everything before its test
/// module, which names the patterns on purpose.
fn production(source: &str) -> &str {
    source
        .split("\n#[cfg(test)]\nmod tests {")
        .next()
        .filter(|part| part.len() < source.len())
        .expect("the source has a test module to cut at")
}

/// The name of the function a line opens, if it opens one.
fn opened_fn(line: &str) -> Option<&str> {
    let mut rest = line.trim_start();
    for prefix in ["pub(crate) ", "pub(super) ", "pub ", "async "] {
        rest = rest.strip_prefix(prefix).unwrap_or(rest);
    }
    let rest = rest.strip_prefix("async ").unwrap_or(rest);
    let name = rest.strip_prefix("fn ")?;
    let end = name.find(|c: char| !(c.is_alphanumeric() || c == '_'))?;
    Some(&name[..end])
}

/// Every `(line, function)` of `source` whose line contains `needle`.
fn sites<'a>(source: &'a str, needle: &str) -> Vec<(usize, &'a str)> {
    let mut current = "";
    let mut found = Vec::new();
    for (index, line) in source.lines().enumerate() {
        if let Some(name) = opened_fn(line) {
            current = name;
        }
        if line.contains(needle) {
            found.push((index + 1, current));
        }
    }
    found
}

/// Fail on any `needle` outside `allowed`, after proving the guard sees
/// the needle where it belongs: an empty match must not pass for a clean one.
fn assert_confined(file: &str, source: &str, needle: &str, allowed: &[&str]) {
    let found = sites(production(source), needle);
    assert!(
        found.iter().any(|(_, function)| allowed.contains(function)),
        "{file}: `{needle}` not found in {allowed:?}; the guard is not reading the real source"
    );
    let stray: Vec<_> = found
        .into_iter()
        .filter(|(_, function)| !allowed.contains(function))
        .collect();
    assert!(
        stray.is_empty(),
        "{file}: `{needle}` outside {allowed:?}, resolve through parent_folder_id instead: {stray:?}"
    );
}

#[test]
fn google_drive_resolves_every_path_through_parent_folder_id() {
    let source = include_str!("google_drive.rs");
    assert_confined(
        "google_drive.rs",
        source,
        "self.resolve_path(",
        &["parent_folder_id", "cd"],
    );
    assert_confined(
        "google_drive.rs",
        source,
        "self.current_folder_id.clone()",
        &["parent_folder_id", "list"],
    );
    assert_confined(
        "google_drive.rs",
        source,
        "\"root\".to_string()",
        &["new", "connect", "resolve_path"],
    );
}

#[test]
fn zoho_workdrive_resolves_every_path_through_parent_folder_id() {
    let source = include_str!("zoho_workdrive.rs");
    assert_confined(
        "zoho_workdrive.rs",
        source,
        "self.resolve_path(",
        &["parent_folder_id", "cd"],
    );
    assert_confined(
        "zoho_workdrive.rs",
        source,
        "self.current_folder_id.clone()",
        &["parent_folder_id", "list", "resolve_path"],
    );
    assert_confined(
        "zoho_workdrive.rs",
        source,
        ".get(\"/\")",
        &["resolve_path"],
    );
}
