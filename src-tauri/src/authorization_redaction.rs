// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! The `Authorization` rules of the two Rust scrubs: the diagnostic log tail
//! (`debug_tests::redact`) and the error text handed to the UI, the CLI and
//! the models (`ai::sanitize_error_message`). One definition, so the two
//! cannot drift; the frontend table (src/utils/redactSensitive.ts) writes the
//! same rules and the three run the `authorization` cases of
//! src/shared/redaction-cases.json.
//!
//! Authorization (and Proxy-Authorization) carries `scheme credentials`, and
//! the whole value goes, not its first word: `Basic dXNlcjpwYXNz` is a
//! password. Each scrub puts the rules ahead of its own key and Bearer rules,
//! so `Authorization: Bearer sk-...` becomes one marker, and chooses the
//! marker text.

/// JSON inside a string: the value ends at the first lone `\"`, and a Digest
/// parameter's `\\\"` belongs to it. Replace with `$1` plus the marker.
pub(crate) const IN_JSON_STRING: &str =
    r#"(?i)(\\"authorization\\"[ \t]*:[ \t]*\\")(?:[^\\]|\\\\\\")*"#;

/// A quoted value (JSON, Debug) ends at its closing quote, so the keys after
/// it stay; a line cut inside the value has no closing quote and ends there.
/// Replace with `$1` plus the quoted marker.
pub(crate) const QUOTED: &str =
    r#"(?i)(\bauthorization["']?[ \t]*[:=][ \t]*)(?:"(?:[^"\\]|\\.)*"?|'[^']*'?)"#;

/// One RFC 7235 `auth-param`: `name=value`, the value a token or a quoted
/// string. A token may hold a `;` (AWS SigV4: `SignedHeaders=host;x-amz-date`).
macro_rules! auth_param {
    () => {
        r#"[A-Za-z0-9_-]+[ \t]*=[ \t]*(?:"(?:[^"\\]|\\.)*"|[^\s,'"<>\\]+)"#
    };
}

/// A bare value is `credentials`: a scheme, then a comma-separated list of
/// parameters (Digest, Hawk, ...), or a token68 or any other opaque
/// credential up to the next delimiter (`AWS AKIA...:c2ln...`). `$marker` is the
/// regex of the scrub's own marker: a value that is already the marker stays
/// as it is, so scrubbing a scrubbed line again does not take the word after
/// it for a credential. Replace with the key, a separator and the marker.
macro_rules! credentials {
    ($marker:literal) => {
        concat!(
            r#"(?i)\bauthorization["']?[ \t]*[:=][ \t]*(?:"#,
            $marker,
            r#"|[^\s,;'"<>\\]+(?:[ \t]+(?:"#,
            $crate::authorization_redaction::auth_param!(),
            r"(?:[ \t]*,[ \t]*",
            $crate::authorization_redaction::auth_param!(),
            r#")*|[^\s,'"<>\\]+))?)"#,
        )
    };
}

pub(crate) use auth_param;
pub(crate) use credentials;
