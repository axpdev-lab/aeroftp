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

/// The body of a value whose quotes are escaped (`\"v\"`, from a Debug or
/// JSON dump of the line), up to its closing `\"`. One level down every
/// backslash is doubled: `\\` is an escape of the value itself and takes the
/// next character with it (`\\\"` is a quote inside the value, `\\\\` a
/// backslash), so a backslash in the value does not end it early; any other
/// `\x` is one character. The frontend table writes the same text.
macro_rules! escaped_body {
    () => {
        r#"(?:[^\\]|\\[^"\\]|\\\\(?:[^\\]|\\.))*"#
    };
}

pub(crate) use escaped_body;

/// JSON inside a string: the value ends at the first lone `\"`, and a Digest
/// parameter's `\\\"` belongs to it. Replace with `$1` plus the marker.
pub(crate) const IN_JSON_STRING: &str = concat!(
    r#"(?i)(\\"authorization\\"[ \t]*:[ \t]*\\")"#,
    escaped_body!()
);

/// A quoted value (JSON, Debug) ends at its closing quote, so the keys after
/// it stay; a line cut inside the value has no closing quote and ends there.
/// A value whose quotes are escaped (`authorization=\"Basic ...\"`, from a
/// Debug or JSON dump of the line) is read first, and takes its closing `\"`.
/// Replace with `$1` plus the quoted marker.
pub(crate) const QUOTED: &str = concat!(
    r#"(?i)(\bauthorization["']?[ \t]*[:=][ \t]*)(?:\\""#,
    escaped_body!(),
    r#"(?:\\")?|"(?:[^"\\]|\\.)*"?|'[^']*'?)"#
);

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

/// The inputs of src/shared/redaction-matrix.json, built the way the frontend
/// test builds them: every key form x separator x value form and every
/// Authorization scheme, each in the ways a log line can carry them (plain,
/// JSON, JSON inside a JSON string, Debug-escaped). The secret never survives,
/// the key does, and scrubbing twice changes nothing.
#[cfg(test)]
pub(crate) mod matrix {
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct Credential {
        credential: String,
        secret: String,
    }

    #[derive(Deserialize)]
    struct Matrix {
        secret: String,
        keys: Vec<String>,
        separators: Vec<String>,
        #[serde(rename = "valueForms")]
        value_forms: Vec<String>,
        values: Vec<String>,
        authorization: Vec<Credential>,
        unchanged: Vec<String>,
    }

    pub(crate) struct Probe {
        pub(crate) input: String,
        pub(crate) leaked: String,
        pub(crate) kept: String,
    }

    fn matrix() -> Matrix {
        serde_json::from_str(include_str!("../../src/shared/redaction-matrix.json"))
            .expect("redaction matrix parses")
    }

    fn json_string(text: &str) -> String {
        serde_json::to_string(text).expect("a string serializes")
    }

    fn object(key: &str, value: &str) -> String {
        let mut map = serde_json::Map::new();
        map.insert(key.into(), value.into());
        map.insert("next".into(), 1.into());
        serde_json::Value::Object(map).to_string()
    }

    fn in_a_string(inner: String) -> String {
        let mut map = serde_json::Map::new();
        map.insert("msg".into(), inner.into());
        serde_json::Value::Object(map).to_string()
    }

    /// The text with its quotes and backslashes escaped twice, inside `\"..\"`.
    fn escaped(text: &str) -> String {
        let twice = json_string(&json_string(text));
        twice[3..twice.len() - 3].to_string()
    }

    pub(crate) fn secret_field_probes() -> Vec<Probe> {
        let m = matrix();
        let mut probes = Vec::new();
        for key in &m.keys {
            let mut probe = |input: String| {
                probes.push(Probe {
                    input,
                    leaked: m.secret.clone(),
                    kept: key.clone(),
                })
            };
            // The secret alone and the secret after a backslash, which an
            // escaped value doubles once per level and must not end it early.
            for template in &m.values {
                let value = template.replace("{v}", &m.secret);
                for sep in &m.separators {
                    for form in &m.value_forms {
                        probe(format!("{key}{sep}{} next", form.replace("{v}", &value)));
                    }
                }
                probe(object(key, &value));
                probe(format!(
                    r#"{{"{key}": {}, "next": 1}}"#,
                    json_string(&value)
                ));
                probe(in_a_string(object(key, &value)));
                probe(format!("Config {{ {key}: \\\"{}\\\" }}", escaped(&value)));
            }
        }
        probes
    }

    pub(crate) fn authorization_probes() -> Vec<Probe> {
        let mut probes = Vec::new();
        for Credential { credential, secret } in matrix().authorization {
            let mut probe = |input: String| {
                probes.push(Probe {
                    input,
                    leaked: secret.clone(),
                    kept: "authorization".into(),
                })
            };
            probe(format!("Authorization: {credential} next"));
            probe(format!("authorization={credential} next"));
            probe(format!("proxy-authorization: {credential}"));
            probe(format!("authorization={} next", json_string(&credential)));
            probe(format!(
                "authorization=\\\"{}\\\" next",
                escaped(&credential)
            ));
            probe(object("authorization", &credential));
            probe(in_a_string(object("authorization", &credential)));
        }
        probes
    }

    pub(crate) fn unchanged_lines() -> Vec<String> {
        matrix().unchanged
    }

    pub(crate) fn assert_redacted(redact: impl Fn(&str) -> String, probes: &[Probe]) {
        assert!(probes.len() >= 20, "{} probes", probes.len());
        for probe in probes {
            let out = redact(&probe.input);
            assert!(!out.contains(&probe.leaked), "{} -> {out}", probe.input);
            assert!(
                out.to_lowercase().contains(&probe.kept.to_lowercase()),
                "{} -> {out}",
                probe.input
            );
            assert_eq!(redact(&out), out, "again: {out}");
        }
    }
}
