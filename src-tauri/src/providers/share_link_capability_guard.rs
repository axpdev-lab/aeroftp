//! Structural guard: what a provider advertises in `share_link_capabilities`
//! is what it implements.
//!
//! The Share Link modal draws its options from `share_link_capabilities` of
//! the connected provider, so a flag here is a button there. Two flags had
//! drifted from the code next to them: Yandex Disk implemented
//! `list_share_links` but said it could not list, which hid the Manage tab,
//! and Zoho WorkDrive said it could revoke without implementing
//! `remove_share_link`, so its Revoke button could only fail on the trait
//! default. This test reads every provider source and fails when a flag and
//! the method it stands for disagree:
//!
//! - `supports_list_links` is true exactly when the file defines
//!   `list_share_links`;
//! - `supports_revoke` (revoking a listed link) needs `remove_share_link`
//!   or `remove_share_link_by_id`;
//! - any advanced option (`supports_expiration`, `supports_password`,
//!   `supports_permissions`) needs `create_share_link`.
//!
//! A provider without its own `share_link_capabilities` advertises the
//! default, which is all false. The walk reads the directory at test time, so
//! a new provider file is covered without being listed here.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use std::path::{Path, PathBuf};

/// The production part of a source: everything before its test module.
fn production(source: &str) -> &str {
    [
        "\n#[cfg(test)]\nmod tests {",
        "\r\n#[cfg(test)]\r\nmod tests {",
    ]
    .iter()
    .find_map(|marker| source.find(marker))
    .map_or(source, |at| &source[..at])
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("providers directory is readable") {
        let path = entry.expect("directory entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The body of `fn share_link_capabilities` in `source`, up to the closing
/// brace of a method at impl indentation.
fn capabilities_body(source: &str) -> Option<&str> {
    let start = source.find("fn share_link_capabilities(")?;
    let rest = &source[start..];
    let end = rest
        .find("\n    }\n")
        .or_else(|| rest.find("\r\n    }\r\n"))?;
    Some(&rest[..end])
}

fn flag(body: Option<&str>, name: &str) -> bool {
    body.is_some_and(|b| b.contains(&format!("{name}: true")))
}

#[test]
fn share_link_flags_match_the_methods_each_provider_defines() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/providers");
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    files.sort();

    let mut with_capabilities = 0;
    let mut mismatches = Vec::new();
    for path in &files {
        // The trait itself, whose defaults are the all-false baseline, and
        // this file, which names the patterns it looks for.
        if path == &root.join("mod.rs") || path == &root.join("share_link_capability_guard.rs") {
            continue;
        }
        let source = std::fs::read_to_string(path).expect("provider source is readable");
        let code = production(&source);
        let body = capabilities_body(code);
        if body.is_some() {
            with_capabilities += 1;
        }
        let name = path.strip_prefix(&root).unwrap_or(path).display();
        let defines = |method: &str| code.contains(&format!("fn {method}("));

        let lists = flag(body, "supports_list_links");
        if lists != defines("list_share_links") {
            mismatches.push(format!(
                "{name}: supports_list_links is {lists}, list_share_links is {}",
                if defines("list_share_links") {
                    "defined"
                } else {
                    "not defined"
                }
            ));
        }
        if flag(body, "supports_revoke")
            && !defines("remove_share_link")
            && !defines("remove_share_link_by_id")
        {
            mismatches.push(format!(
                "{name}: supports_revoke is true but neither remove_share_link nor remove_share_link_by_id is defined"
            ));
        }
        for option in [
            "supports_expiration",
            "supports_password",
            "supports_permissions",
        ] {
            if flag(body, option) && !defines("create_share_link") {
                mismatches.push(format!(
                    "{name}: {option} is true but create_share_link is not defined"
                ));
            }
        }
    }

    // A walk that found nothing would pass every assertion above.
    assert!(
        files.len() > 40,
        "only {} provider sources found",
        files.len()
    );
    assert!(
        with_capabilities >= 15,
        "only {with_capabilities} share_link_capabilities found"
    );
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}
