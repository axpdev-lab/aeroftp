#![cfg(test)]
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Class-level pin: a quick-xml parser that reads element text also reads the
//! entity references in it.
//!
//! quick-xml hands element text over in pieces: `KDE&apos;s` arrives as
//! `Event::Text("KDE")` + `Event::GeneralRef("apos")` + `Event::Text("s")`.
//! A function that matches `Event::Text` with no `Event::GeneralRef` arm drops
//! the entity, and usually keeps only one of the pieces. That is how an S3
//! bucket named "KDE's Kate Settings" reached the bucket picker as
//! "s Kate Settings" (#1461), after the same defect had been fixed parser by
//! parser in the object listings (`a&b.txt` listed as `b.txt` on Storj). It
//! kept coming back one parser at a time, so this test reads the sources and
//! asserts that the functions matching `Event::Text` without an
//! `Event::GeneralRef` arm are exactly [`ALLOWED`]. Set equality in both
//! directions, so the list cannot rot.
//!
//! [`ALLOWED`] is empty, and "this text holds no entity" is no reason to fill
//! it: XML lets any character arrive as a numeric reference, so a size can be
//! `1&#50;3` and a base64 secret `a&#43;b`. Four functions were first listed
//! on that reasoning (sizes, hashes, dates, credentials) and all four turned
//! out to read such values wrong.

use crate::mtime_parse_audit::{
    char_literal_len, raw_string_open, rust_files, src_dir, strip_test_modules,
};

/// (file relative to `src/`, function, why it may ignore entity references):
/// only for text the function never uses, never for text it believes cannot
/// hold one.
const ALLOWED: &[(&str, &str, &str)] = &[];

/// `source` with comments, string and char literals blanked to spaces, so a
/// comment or a string naming `Event::GeneralRef` neither hides nor fakes an
/// arm. Byte offsets and line breaks are kept.
fn code_only(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = bytes.to_vec();
    let mut i = 0;
    while i < bytes.len() {
        let rest = &source[i..];
        let end = if rest.starts_with("//") {
            i + rest.find('\n').unwrap_or(rest.len())
        } else if rest.starts_with("/*") {
            i + rest.find("*/").map_or(rest.len(), |e| e + 2)
        } else if let Some(hashes) = raw_string_open(source, i) {
            let close = format!("\"{}", "#".repeat(hashes));
            let body = i + 2 + hashes;
            body + source[body..]
                .find(&close)
                .map_or(source.len() - body, |e| e + close.len())
        } else if bytes[i] == b'"' {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] != b'"' {
                j += if bytes[j] == b'\\' { 2 } else { 1 };
            }
            (j + 1).min(bytes.len())
        } else if let Some(len) = char_literal_len(rest) {
            i + len
        } else {
            i += rest.chars().next().map_or(1, char::len_utf8);
            continue;
        };
        for byte in &mut out[i..end] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
        i = end;
    }
    String::from_utf8(out).expect("blanking to ASCII spaces keeps UTF-8")
}

/// (name, body start, body end) of every `fn` with a body in `code`, which
/// must already be [`code_only`].
fn fn_bodies(code: &str) -> Vec<(String, usize, usize)> {
    let bytes = code.as_bytes();
    let mut out = Vec::new();
    let mut search = 0;
    while let Some(found) = code[search..].find("fn ") {
        let at = search + found;
        search = at + 3;
        if at > 0 && (bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'_') {
            continue;
        }
        let name: String = code[at + 3..]
            .trim_start()
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if name.is_empty() {
            continue;
        }
        // The body opens at the first `{` outside the parameters and any
        // array type (`-> [u8; 4]`); a `;` there first is a declaration.
        let mut depth = 0i32;
        let mut open = None;
        for (k, byte) in bytes.iter().enumerate().skip(at + 3) {
            match byte {
                b'(' | b'[' => depth += 1,
                b')' | b']' => depth -= 1,
                b'{' if depth == 0 => {
                    open = Some(k);
                    break;
                }
                b';' if depth == 0 => break,
                _ => {}
            }
        }
        let Some(open) = open else { continue };
        let mut level = 0usize;
        let mut close = code.len();
        for (k, byte) in bytes.iter().enumerate().skip(open) {
            match byte {
                b'{' => level += 1,
                b'}' => {
                    level -= 1;
                    if level == 0 {
                        close = k + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        out.push((name, open, close));
    }
    out
}

/// Names of the functions in `code` (already [`code_only`]) that match
/// `Event::Text` and have no `Event::GeneralRef` arm of their own. Each
/// occurrence belongs to the innermost function around it, so a nested
/// helper is judged on its own arms.
fn functions_missing_entity_refs(code: &str) -> Vec<String> {
    let fns = fn_bodies(code);
    let owners = |pattern: &str| -> std::collections::BTreeSet<usize> {
        code.match_indices(pattern)
            .filter_map(|(at, _)| {
                fns.iter()
                    .enumerate()
                    .filter(|(_, (_, start, end))| *start <= at && at < *end)
                    .min_by_key(|(_, (_, start, end))| end - start)
                    .map(|(index, _)| index)
            })
            .collect()
    };
    let refs = owners("Event::GeneralRef(");
    let mut names: Vec<String> = owners("Event::Text(")
        .difference(&refs)
        .map(|index| fns[*index].0.clone())
        .collect();
    names.sort();
    names
}

fn sites() -> Vec<(String, String)> {
    let root = src_dir();
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    let mut found = Vec::new();
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
        let code = code_only(&strip_test_modules(&source));
        for name in functions_missing_entity_refs(&code) {
            found.push((rel.clone(), name));
        }
    }
    found.sort();
    found
}

#[test]
fn every_xml_text_reader_also_reads_entity_references() {
    let allowed: Vec<(String, String)> = ALLOWED
        .iter()
        .map(|(file, function, _)| (file.to_string(), function.to_string()))
        .collect();
    assert_eq!(
        sites(),
        allowed,
        "\nA function matching quick-xml `Event::Text` without an `Event::GeneralRef` \
         arm appeared or disappeared outside the tests.\n\
         Element text must accumulate its pieces and expand each reference with \
         providers::xml_text::xml_entity_to_str. Any character can arrive as a \
         reference, so no text is exempt; only text the function never uses goes \
         into ALLOWED, with its reason.\n"
    );
}

/// The scanner must read code and nothing else, or the test above would be
/// blind: an arm named in a comment or a string is no arm, and a nested
/// function is judged on its own arms.
#[test]
fn the_scanner_reads_arms_in_code_only() {
    let source = r##"
fn listing() {
    match ev { Ok(Event::Text(t)) => {}, _ => {} }
    // Event::GeneralRef( in a comment is no arm
    let _ = "Event::GeneralRef(";
    let _ = r#"Event::GeneralRef("#;
    let _ = '{';
}
fn complete() {
    match ev { Ok(Event::Text(t)) => {}, Ok(Event::GeneralRef(r)) => {}, _ => {} }
}
fn outer() -> [u8; 4] {
    fn inner() { match ev { Event::Text(t) => {} } }
    match ev { Event::GeneralRef(r) => {} }
}
fn declared(x: u8);
"##;
    assert_eq!(
        functions_missing_entity_refs(&code_only(source)),
        vec!["inner", "listing"]
    );
}
