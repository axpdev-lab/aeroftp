// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Saved-profile options that turn off a check the connection would otherwise
//! make (a certificate check, a host-key check, the PASV address, cleartext
//! consent), read from `src/shared/profile-security-relaxations.json`, the same
//! table the frontend reads (`src/utils/profileSecurity.ts`).
//!
//! Every import path brings these in their safe state unless the user keeps
//! them on purpose, and CLI strict mode refuses a profile that carries one
//! (#1506 Requested 188 and 198): an imported file is untrusted input, and an
//! option it sets can point a saved credential at a connection that no longer
//! checks who it is talking to.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::LazyLock;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RelaxationRule {
    key: String,
    #[serde(rename = "unsafe")]
    unsafe_values: Vec<Value>,
    loopback_exempt: bool,
}

#[derive(Debug, Deserialize)]
struct RelaxationTable {
    relaxations: Vec<RelaxationRule>,
}

fn table() -> &'static RelaxationTable {
    static TABLE: LazyLock<RelaxationTable> = LazyLock::new(|| {
        serde_json::from_str(include_str!(
            "../../src/shared/profile-security-relaxations.json"
        ))
        .expect("profile-security-relaxations.json is valid")
    });
    &TABLE
}

/// One option of a profile that relaxes a security check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Relaxation {
    /// The saved option key (`verifyCert`, `trustPasvAddress`, ...).
    pub key: String,
    /// The value the profile carries, as text.
    pub value: String,
}

fn value_matches(actual: &Value, unsafe_value: &Value) -> bool {
    match (actual, unsafe_value) {
        (Value::String(a), Value::String(u)) => a.trim().eq_ignore_ascii_case(u),
        (a, u) => a == u,
    }
}

fn shown(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// The relaxing options of a profile given by its parts: the host it connects
/// to, its preset id and its saved options. An S3 profile reaches
/// `options.endpoint`, every other one its `host`.
pub fn security_relaxations_of(
    host: &str,
    provider_id: Option<&str>,
    options: Option<&Value>,
) -> Vec<Relaxation> {
    let Some(options) = options.and_then(Value::as_object) else {
        return Vec::new();
    };
    let preset = provider_id.and_then(crate::connection_manifest::preset);
    let target = options
        .get("endpoint")
        .and_then(Value::as_str)
        .filter(|endpoint| !endpoint.trim().is_empty())
        .unwrap_or(host);
    let on_loopback = crate::util::endpoint_stays_on_this_machine(target);
    table()
        .relaxations
        .iter()
        .filter_map(|rule| {
            let actual = options.get(&rule.key)?;
            if !rule.unsafe_values.iter().any(|u| value_matches(actual, u)) {
                return None;
            }
            if preset.is_some_and(|preset| preset.defaults.contains_key(&rule.key)) {
                return None;
            }
            if rule.loopback_exempt && on_loopback {
                return None;
            }
            Some(Relaxation {
                key: rule.key.clone(),
                value: shown(actual),
            })
        })
        .collect()
}

/// One relaxing option the user chose to keep on an import: the profile's
/// place in the file and the option key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeptRelaxation {
    pub file_index: usize,
    pub key: String,
}

/// What an import does with the relaxing options it finds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum RelaxationPolicy {
    /// Every one comes in its safe state (the default, and the GUI's when the
    /// user ticks nothing).
    #[default]
    ResetAll,
    /// Every one is kept (CLI `--keep-insecure-options`).
    KeepAll,
    /// Only the listed ones are kept (the GUI preview checkboxes).
    Keep(Vec<KeptRelaxation>),
}

impl RelaxationPolicy {
    /// The keys kept for the profile at `file_index`, out of `relaxations`.
    pub fn kept_keys(&self, file_index: usize, relaxations: &[Relaxation]) -> Vec<String> {
        match self {
            Self::ResetAll => Vec::new(),
            Self::KeepAll => relaxations.iter().map(|r| r.key.clone()).collect(),
            Self::Keep(kept) => kept
                .iter()
                .filter(|k| k.file_index == file_index)
                .map(|k| k.key.clone())
                .collect(),
        }
    }
}

/// [`security_relaxations_of`] for a profile stored as JSON (`host`,
/// `providerId`, `options`).
pub fn security_relaxations(profile: &Value) -> Vec<Relaxation> {
    security_relaxations_of(
        profile.get("host").and_then(Value::as_str).unwrap_or(""),
        profile.get("providerId").and_then(Value::as_str),
        profile.get("options"),
    )
}

/// Bring `options` to its safe state: every relaxing option found by
/// [`security_relaxations_of`] whose key is not in `keep` is removed (an absent
/// option is the safe default for each of them). Returns the keys removed.
pub fn make_options_safe(
    options: &mut Option<Value>,
    relaxations: &[Relaxation],
    keep: &[String],
) -> Vec<String> {
    let Some(map) = options.as_mut().and_then(Value::as_object_mut) else {
        return Vec::new();
    };
    relaxations
        .iter()
        .filter(|relaxation| !keep.contains(&relaxation.key))
        .filter_map(|relaxation| map.remove(&relaxation.key).map(|_| relaxation.key.clone()))
        .collect()
}

/// [`make_options_safe`] for a profile stored as JSON. Returns the keys
/// removed.
pub fn make_profile_safe(profile: &mut Value, keep: &[String]) -> Vec<String> {
    let relaxations = security_relaxations(profile);
    let Some(object) = profile.as_object_mut() else {
        return Vec::new();
    };
    let mut options = object.remove("options");
    let removed = make_options_safe(&mut options, &relaxations, keep);
    if let Some(options) = options {
        object.insert("options".to_string(), options);
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Deserialize)]
    struct Case {
        name: String,
        profile: Value,
        relaxed: Vec<String>,
    }

    #[derive(Deserialize)]
    struct Cases {
        cases: Vec<Case>,
    }

    /// The shared cases, the same ones `src/utils/profileSecurity.test.ts`
    /// runs, so the two readers of the table cannot disagree.
    #[test]
    fn the_shared_cases_hold() {
        let cases: Cases = serde_json::from_str(include_str!(
            "../../src/shared/profile-security-relaxations.json"
        ))
        .unwrap();
        assert!(cases.cases.len() >= 10);
        for case in cases.cases {
            let keys: Vec<String> = security_relaxations(&case.profile)
                .into_iter()
                .map(|r| r.key)
                .collect();
            assert_eq!(keys, case.relaxed, "{}", case.name);
        }
    }

    #[test]
    fn every_rule_names_a_key_the_profile_loader_reads() {
        for rule in &table().relaxations {
            let backend = crate::profile_loader::normalize_profile_option_key(&rule.key);
            assert!(!backend.is_empty(), "{}", rule.key);
            assert!(!rule.unsafe_values.is_empty(), "{}", rule.key);
        }
    }

    #[test]
    fn making_a_profile_safe_removes_only_what_is_not_kept() {
        let mut profile = json!({
            "host": "files.example.com",
            "options": {
                "verifyCert": false,
                "trustPasvAddress": true,
                "trustUnknownHosts": true,
                "timeout": 30
            }
        });
        let removed = make_profile_safe(&mut profile, &["trustPasvAddress".to_string()]);
        assert_eq!(removed, vec!["verifyCert", "trustUnknownHosts"]);
        assert_eq!(
            profile["options"],
            json!({"trustPasvAddress": true, "timeout": 30})
        );
        assert_eq!(
            security_relaxations(&profile),
            vec![Relaxation {
                key: "trustPasvAddress".into(),
                value: "true".into()
            }]
        );
    }

    #[test]
    fn a_profile_without_options_is_left_alone() {
        let mut profile = json!({"host": "files.example.com"});
        assert!(make_profile_safe(&mut profile, &[]).is_empty());
        assert_eq!(profile, json!({"host": "files.example.com"}));
    }
}
