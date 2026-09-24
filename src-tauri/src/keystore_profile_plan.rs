// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Per-profile plan for a keystore import: what the backup would change in
//! the server profile list, and what the list and its secrets become once the
//! user has accepted, rejected or kept both versions of each change (#347).
//!
//! Everything here is pure: the lists and the secret values come in, the final
//! list and the secret writes go out. Reading the vault, the partition
//! database and the backup, and writing the result, stay in `keystore_export`.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use zeroize::Zeroizing;

/// Fields that change on every connection. A profile that differs only here
/// is not a change worth a decision, and the local copy (the more recent one
/// on this machine) is kept.
const VOLATILE_FIELDS: &[&str] = &["lastConnected", "lastQuota"];

/// Fields whose values are shown in the preview. Everything else is listed by
/// name only: icons are large data URLs, and `options` can carry anything a
/// provider stores, so its values never leave the backend.
const SHOWN_FIELDS: &[&str] = &[
    "name",
    "host",
    "port",
    "username",
    "protocol",
    "providerId",
    "initialPath",
    "localInitialPath",
    "color",
    "publicUrlBase",
];

/// Where the backup's profile list comes from.
#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProfileListSource {
    /// The backup's `user_partitions.db`, which the import restores whole.
    Partition,
    /// The backup's `config_server_profiles` vault blob.
    Vault,
    /// The import does not touch the profile list.
    None,
}

#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProfileChangeKind {
    /// Only in the backup.
    Added,
    /// Only on this machine, and the import replaces the list.
    Removed,
    /// On both sides, with different fields or credentials.
    Changed,
}

/// What to do with one change. `Accept` is the backup's side (add, remove,
/// take the backup version); `Reject` keeps this machine's side; `Both` keeps
/// the local profile and adds the backup version as a copy with a new id.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProfileDecision {
    Accept,
    Reject,
    Both,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProfileFieldChange {
    pub field: String,
    /// Values are filled only for [`SHOWN_FIELDS`]; `hidden` marks the rest.
    pub local: Option<String>,
    pub backup: Option<String>,
    pub hidden: bool,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProfileChange {
    pub id: String,
    pub kind: ProfileChangeKind,
    pub local_name: Option<String>,
    pub backup_name: Option<String>,
    pub protocol: Option<String>,
    pub host: Option<String>,
    pub fields: Vec<ProfileFieldChange>,
    /// A per-profile secret (password, token, overlay key) differs. Never the
    /// values, only the fact.
    pub credentials_differ: bool,
    /// What the import does today with no decision: the dialog starts here.
    pub default_decision: ProfileDecision,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProfilePreview {
    pub source: ProfileListSource,
    /// Where this device's list was read: its partition, or the vault blob
    /// when the partition could not be read. The dialog says so in the second
    /// case, because the comparison is then against a list My Servers may not
    /// show.
    pub local_source: ProfileListSource,
    /// The import replaces the list (a restored partition, or "overwrite"),
    /// so a profile only on this machine is removed unless kept.
    pub replaces_list: bool,
    pub unchanged: u32,
    pub changes: Vec<ProfileChange>,
}

/// One decision from the dialog. `copy_name` is the translated name for the
/// copy made by [`ProfileDecision::Both`].
#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ProfileDecisionInput {
    pub id: String,
    pub decision: ProfileDecision,
    #[serde(default)]
    pub copy_name: Option<String>,
}

/// The two lists and the secrets the plan works on.
pub struct PlanInputs {
    pub local: Vec<Value>,
    pub local_source: ProfileListSource,
    /// `None` when the import leaves the profile list alone.
    pub backup: Option<Vec<Value>>,
    pub source: ProfileListSource,
    pub replaces_list: bool,
    /// "Skip existing": a change starts at this device's version. The name of
    /// the strategy is the user's intent, even where the restored partition
    /// would replace the list without a decision.
    pub keep_local_by_default: bool,
    /// This machine's value for every per-profile key of every id on either
    /// side, read before the import writes anything. `None` = absent.
    pub local_secrets: HashMap<String, Option<Zeroizing<String>>>,
    /// The backup's value for the same keys. Absent keys are not in the map.
    pub backup_secrets: HashMap<String, Zeroizing<String>>,
}

/// A vault write the plan needs: `Some` stores the value, `None` deletes.
pub type SecretOp = (String, Option<Zeroizing<String>>);

pub fn profile_id(profile: &Value) -> Option<&str> {
    profile.get("id").and_then(Value::as_str)
}

pub fn profile_protocol(profile: &Value) -> Option<&str> {
    profile.get("protocol").and_then(Value::as_str)
}

fn string_field(profile: &Value, field: &str) -> Option<String> {
    match profile.get(field)? {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

/// Every per-profile vault key of `profile`, as `per_profile_vault_keys` lists
/// them for its protocol.
pub fn secret_keys(profile: &Value) -> Vec<String> {
    profile_id(profile)
        .map(|id| crate::per_profile_vault_keys(id, profile_protocol(profile)))
        .unwrap_or_default()
}

/// Keys of both versions of one id (a profile may have changed provider).
fn keys_for(local: Option<&Value>, backup: Option<&Value>) -> Vec<String> {
    let mut seen = HashSet::new();
    local
        .into_iter()
        .chain(backup)
        .flat_map(secret_keys)
        .filter(|k| seen.insert(k.clone()))
        .collect()
}

fn index_by_id(list: &[Value]) -> HashMap<&str, &Value> {
    list.iter()
        .filter_map(|p| profile_id(p).map(|id| (id, p)))
        .collect()
}

fn field_changes(local: &Value, backup: &Value) -> Vec<ProfileFieldChange> {
    let empty = serde_json::Map::new();
    let l = local.as_object().unwrap_or(&empty);
    let b = backup.as_object().unwrap_or(&empty);
    let mut names: Vec<&String> = l.keys().chain(b.keys()).collect();
    names.sort();
    names.dedup();
    names
        .into_iter()
        .filter(|f| !VOLATILE_FIELDS.contains(&f.as_str()))
        .filter(|f| l.get(*f) != b.get(*f))
        .map(|f| {
            let shown = SHOWN_FIELDS.contains(&f.as_str());
            ProfileFieldChange {
                field: f.clone(),
                local: shown.then(|| string_field(local, f)).flatten(),
                backup: shown.then(|| string_field(backup, f)).flatten(),
                hidden: !shown,
            }
        })
        .collect()
}

fn credentials_differ(inputs: &PlanInputs, keys: &[String]) -> bool {
    keys.iter().any(|k| {
        let local = inputs
            .local_secrets
            .get(k)
            .and_then(|v| v.as_ref())
            .map(|v| v.as_str());
        let backup = inputs.backup_secrets.get(k).map(|v| v.as_str());
        // A key the backup does not carry is left alone by the import.
        backup.is_some() && local != backup
    })
}

/// The per-profile diff between this machine and the backup.
pub fn preview(inputs: &PlanInputs) -> ProfilePreview {
    let Some(backup) = inputs.backup.as_deref() else {
        return ProfilePreview {
            source: ProfileListSource::None,
            local_source: inputs.local_source,
            replaces_list: false,
            unchanged: inputs.local.len() as u32,
            changes: Vec::new(),
        };
    };
    let local_by_id = index_by_id(&inputs.local);
    let backup_by_id = index_by_id(backup);
    let mut changes = Vec::new();
    let mut unchanged = 0u32;

    for b in backup {
        let Some(id) = profile_id(b) else { continue };
        let keys = keys_for(local_by_id.get(id).copied(), Some(b));
        let creds = credentials_differ(inputs, &keys);
        match local_by_id.get(id) {
            None => changes.push(ProfileChange {
                id: id.to_string(),
                kind: ProfileChangeKind::Added,
                local_name: None,
                backup_name: string_field(b, "name"),
                protocol: profile_protocol(b).map(str::to_owned),
                host: string_field(b, "host"),
                fields: Vec::new(),
                credentials_differ: creds,
                default_decision: ProfileDecision::Accept,
            }),
            Some(l) => {
                let fields = field_changes(l, b);
                if fields.is_empty() && !creds {
                    unchanged += 1;
                    continue;
                }
                changes.push(ProfileChange {
                    id: id.to_string(),
                    kind: ProfileChangeKind::Changed,
                    local_name: string_field(l, "name"),
                    backup_name: string_field(b, "name"),
                    protocol: profile_protocol(b).map(str::to_owned),
                    host: string_field(b, "host"),
                    fields,
                    credentials_differ: creds,
                    default_decision: if inputs.keep_local_by_default {
                        ProfileDecision::Reject
                    } else {
                        ProfileDecision::Accept
                    },
                });
            }
        }
    }

    for l in &inputs.local {
        let Some(id) = profile_id(l) else { continue };
        if backup_by_id.contains_key(id) {
            continue;
        }
        if !inputs.replaces_list {
            // A union keeps it, so there is nothing to decide.
            unchanged += 1;
            continue;
        }
        changes.push(ProfileChange {
            id: id.to_string(),
            kind: ProfileChangeKind::Removed,
            local_name: string_field(l, "name"),
            backup_name: None,
            protocol: profile_protocol(l).map(str::to_owned),
            host: string_field(l, "host"),
            fields: Vec::new(),
            credentials_differ: false,
            default_decision: if inputs.keep_local_by_default {
                ProfileDecision::Reject
            } else {
                ProfileDecision::Accept
            },
        });
    }

    ProfilePreview {
        source: inputs.source,
        local_source: inputs.local_source,
        replaces_list: inputs.replaces_list,
        unchanged,
        changes,
    }
}

/// The final profile list, and the secret writes that go with it.
pub struct PlanOutcome {
    pub profiles: Vec<Value>,
    pub secret_ops: Vec<SecretOp>,
}

/// Put back this machine's pre-import value of every key in `keys`.
fn restore_local(inputs: &PlanInputs, keys: &[String], ops: &mut Vec<SecretOp>) {
    for k in keys {
        let value = inputs.local_secrets.get(k).and_then(|v| v.clone());
        ops.push((k.clone(), value));
    }
}

/// Write the backup's value of every key it carries. A key it does not carry
/// gets no write: the import left it as it was, and on a restored partition a
/// write of this machine's value would replace the credential the backup
/// brought with it.
fn take_backup(inputs: &PlanInputs, keys: &[String], ops: &mut Vec<SecretOp>) {
    for k in keys {
        if let Some(v) = inputs.backup_secrets.get(k) {
            ops.push((k.clone(), Some(v.clone())));
        }
    }
}

/// The backup profile under a new id, with its secrets copied to the new
/// id's keys. `per_profile_vault_keys` lists keys in a fixed order for a given
/// protocol, so the two lists pair up index by index.
fn copy_of(
    inputs: &PlanInputs,
    backup: &Value,
    new_id: &str,
    name: String,
    ops: &mut Vec<SecretOp>,
) -> Value {
    let mut copy = backup.clone();
    if let Some(obj) = copy.as_object_mut() {
        obj.insert("id".into(), Value::String(new_id.to_string()));
        obj.insert("name".into(), Value::String(name));
    }
    let protocol = profile_protocol(backup);
    let old_id = profile_id(backup).unwrap_or_default();
    let old_keys = crate::per_profile_vault_keys(old_id, protocol);
    let new_keys = crate::per_profile_vault_keys(new_id, protocol);
    for (old, new) in old_keys.iter().zip(&new_keys) {
        if let Some(v) = inputs.backup_secrets.get(old) {
            ops.push((new.clone(), Some(v.clone())));
        }
    }
    copy
}

/// Build the list the user chose, and the secret writes that make each
/// profile's credentials match the version kept.
///
/// An id with no decision gets its preview default, so a partial answer from
/// the dialog behaves like the import without decisions for the rest. The
/// order follows the list the import would have produced: the backup's when
/// it replaces the list, this machine's (with additions at the end) when it
/// merges.
/// Refuse decisions that do not belong to this plan, before anything is
/// written: an id the plan does not list means the backup or this device
/// changed since the preview the user answered, and "keep both" exists only
/// for a changed profile. Either way the dialog is out of date, and applying
/// the rest would quietly do something the user did not choose.
pub fn validate(inputs: &PlanInputs, decisions: &[ProfileDecisionInput]) -> Result<(), String> {
    let plan = preview(inputs);
    let kinds: HashMap<&str, ProfileChangeKind> = plan
        .changes
        .iter()
        .map(|c| (c.id.as_str(), c.kind))
        .collect();
    for d in decisions {
        match kinds.get(d.id.as_str()) {
            None => {
                return Err(format!(
                    "Server profile {} is not among the changes this import makes; review the changes again",
                    d.id
                ))
            }
            Some(kind) if d.decision == ProfileDecision::Both && *kind != ProfileChangeKind::Changed => {
                return Err(format!(
                    "\"Keep both\" applies only to a changed profile, not to {}",
                    d.id
                ))
            }
            Some(_) => {}
        }
    }
    Ok(())
}

pub fn apply(
    inputs: &PlanInputs,
    decisions: &[ProfileDecisionInput],
    new_id: &mut dyn FnMut() -> String,
) -> Result<PlanOutcome, String> {
    validate(inputs, decisions)?;
    let Some(backup) = inputs.backup.as_deref() else {
        return Ok(PlanOutcome {
            profiles: inputs.local.clone(),
            secret_ops: Vec::new(),
        });
    };
    let plan = preview(inputs);
    let chosen: HashMap<&str, &ProfileDecisionInput> =
        decisions.iter().map(|d| (d.id.as_str(), d)).collect();
    let change_by_id: HashMap<&str, &ProfileChange> =
        plan.changes.iter().map(|c| (c.id.as_str(), c)).collect();
    let local_by_id = index_by_id(&inputs.local);
    let backup_by_id = index_by_id(backup);

    let mut ops = Vec::new();
    // Resolve each id to the profiles it contributes, in place.
    let mut resolve = |id: &str, ops: &mut Vec<SecretOp>| -> Vec<Value> {
        let local = local_by_id.get(id).copied();
        let bak = backup_by_id.get(id).copied();
        let keys = keys_for(local, bak);
        let Some(change) = change_by_id.get(id) else {
            // Unchanged, or local-only under a merge: this machine's copy.
            return local.or(bak).cloned().into_iter().collect();
        };
        let input = chosen.get(id);
        let decision = input.map_or(change.default_decision, |d| d.decision);
        match (change.kind, decision) {
            (ProfileChangeKind::Added, ProfileDecision::Accept) => {
                take_backup(inputs, &keys, ops);
                bak.cloned().into_iter().collect()
            }
            (ProfileChangeKind::Added, _) => {
                restore_local(inputs, &keys, ops);
                Vec::new()
            }
            (ProfileChangeKind::Removed, ProfileDecision::Accept) => {
                // Removing a profile in the GUI purges its secrets; an
                // accepted removal must not leave them behind (CWE-459).
                for k in &keys {
                    ops.push((k.clone(), None));
                }
                Vec::new()
            }
            (ProfileChangeKind::Removed, _) => {
                restore_local(inputs, &keys, ops);
                local.cloned().into_iter().collect()
            }
            (ProfileChangeKind::Changed, ProfileDecision::Accept) => {
                take_backup(inputs, &keys, ops);
                bak.cloned().into_iter().collect()
            }
            (ProfileChangeKind::Changed, ProfileDecision::Reject) => {
                restore_local(inputs, &keys, ops);
                local.cloned().into_iter().collect()
            }
            (ProfileChangeKind::Changed, ProfileDecision::Both) => {
                restore_local(inputs, &keys, ops);
                let (Some(l), Some(b)) = (local, bak) else {
                    return Vec::new();
                };
                let name = input
                    .and_then(|d| d.copy_name.clone())
                    .filter(|n| !n.trim().is_empty())
                    .unwrap_or_else(|| {
                        format!("{} (imported)", string_field(b, "name").unwrap_or_default())
                    });
                let copy = copy_of(inputs, b, &new_id(), name, ops);
                vec![l.clone(), copy]
            }
        }
    };

    let mut profiles = Vec::new();
    let mut placed = HashSet::new();
    let (first, second): (&[Value], &[Value]) = if inputs.replaces_list {
        (backup, &inputs.local)
    } else {
        (&inputs.local, backup)
    };
    for p in first.iter().chain(second) {
        let Some(id) = profile_id(p) else { continue };
        if placed.insert(id.to_string()) {
            profiles.extend(resolve(id, &mut ops));
        }
    }

    Ok(PlanOutcome {
        profiles,
        secret_ops: ops,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn secrets(pairs: &[(&str, &str)]) -> HashMap<String, Zeroizing<String>> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), Zeroizing::new(v.to_string())))
            .collect()
    }

    fn local_secrets(pairs: &[(&str, &str)]) -> HashMap<String, Option<Zeroizing<String>>> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), Some(Zeroizing::new(v.to_string()))))
            .collect()
    }

    /// This machine: A (unchanged), B (renamed in the backup, other password),
    /// C (only here). Backup: A, B', D (only in the backup).
    fn inputs(replaces_list: bool) -> PlanInputs {
        PlanInputs {
            local: vec![
                json!({"id": "srv_a", "name": "Alpha", "host": "a.example", "protocol": "sftp", "lastConnected": "2026-09-20"}),
                json!({"id": "srv_b", "name": "Beta", "host": "b.example", "protocol": "sftp", "color": "#f00"}),
                json!({"id": "srv_c", "name": "Gamma", "host": "c.example", "protocol": "ftp"}),
            ],
            backup: Some(vec![
                json!({"id": "srv_a", "name": "Alpha", "host": "a.example", "protocol": "sftp", "lastConnected": "2026-01-01"}),
                json!({"id": "srv_b", "name": "Beta NAS", "host": "b.example", "protocol": "sftp", "options": {"x": 1}}),
                json!({"id": "srv_d", "name": "Delta", "host": "d.example", "protocol": "sftp"}),
            ]),
            local_source: ProfileListSource::Partition,
            source: if replaces_list {
                ProfileListSource::Partition
            } else {
                ProfileListSource::Vault
            },
            replaces_list,
            // Overwrite over a partition, "skip existing" over a vault blob:
            // the two combinations the import had before the preview.
            keep_local_by_default: !replaces_list,
            local_secrets: local_secrets(&[
                ("server_srv_a", "pw-a"),
                ("server_srv_b", "pw-b-local"),
                ("server_srv_c", "pw-c"),
            ]),
            backup_secrets: secrets(&[
                ("server_srv_a", "pw-a"),
                ("server_srv_b", "pw-b-backup"),
                ("server_srv_d", "pw-d"),
            ]),
        }
    }

    fn ids(list: &[Value]) -> Vec<&str> {
        list.iter().filter_map(profile_id).collect()
    }

    fn op<'a>(ops: &'a [SecretOp], key: &str) -> Option<Option<&'a str>> {
        ops.iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_ref().map(|v| v.as_str()))
    }

    #[test]
    fn preview_lists_added_changed_and_removed_with_their_fields() {
        let plan = preview(&inputs(true));
        assert_eq!(plan.unchanged, 1, "srv_a differs only in lastConnected");
        let kinds: Vec<(&str, ProfileChangeKind)> = plan
            .changes
            .iter()
            .map(|c| (c.id.as_str(), c.kind))
            .collect();
        assert_eq!(
            kinds,
            vec![
                ("srv_b", ProfileChangeKind::Changed),
                ("srv_d", ProfileChangeKind::Added),
                ("srv_c", ProfileChangeKind::Removed),
            ]
        );
        let b = &plan.changes[0];
        assert!(b.credentials_differ);
        let fields: Vec<&str> = b.fields.iter().map(|f| f.field.as_str()).collect();
        assert_eq!(fields, vec!["color", "name", "options"]);
        let name = b.fields.iter().find(|f| f.field == "name").unwrap();
        assert_eq!(name.local.as_deref(), Some("Beta"));
        assert_eq!(name.backup.as_deref(), Some("Beta NAS"));
        let options = b.fields.iter().find(|f| f.field == "options").unwrap();
        assert!(options.hidden && options.local.is_none() && options.backup.is_none());
    }

    #[test]
    fn a_merge_keeps_local_only_profiles_out_of_the_decisions() {
        let plan = preview(&inputs(false));
        assert!(plan
            .changes
            .iter()
            .all(|c| c.kind != ProfileChangeKind::Removed));
        let b = plan.changes.iter().find(|c| c.id == "srv_b").unwrap();
        assert_eq!(b.default_decision, ProfileDecision::Reject);
    }

    /// With no decisions the outcome is the list the import makes today: the
    /// backup's when it replaces, the union when it merges.
    #[test]
    fn defaults_reproduce_the_import_without_decisions() {
        let mut next = || "srv_new".to_string();
        let replaced = apply(&inputs(true), &[], &mut next).unwrap();
        assert_eq!(ids(&replaced.profiles), vec!["srv_a", "srv_b", "srv_d"]);
        assert_eq!(replaced.profiles[1]["name"], "Beta NAS");
        assert_eq!(
            op(&replaced.secret_ops, "server_srv_b"),
            Some(Some("pw-b-backup"))
        );

        let merged = apply(&inputs(false), &[], &mut next).unwrap();
        assert_eq!(
            ids(&merged.profiles),
            vec!["srv_a", "srv_b", "srv_c", "srv_d"]
        );
        assert_eq!(merged.profiles[1]["name"], "Beta");
        assert_eq!(
            op(&merged.secret_ops, "server_srv_b"),
            Some(Some("pw-b-local"))
        );
    }

    #[test]
    fn an_accepted_removal_purges_the_profile_secrets() {
        let out = apply(&inputs(true), &[], &mut || "srv_new".to_string()).unwrap();
        assert!(!ids(&out.profiles).contains(&"srv_c"));
        assert_eq!(op(&out.secret_ops, "server_srv_c"), Some(None));
    }

    #[test]
    fn rejecting_restores_this_machine_and_drops_additions() {
        let decisions = [
            ProfileDecisionInput {
                id: "srv_b".into(),
                decision: ProfileDecision::Reject,
                copy_name: None,
            },
            ProfileDecisionInput {
                id: "srv_c".into(),
                decision: ProfileDecision::Reject,
                copy_name: None,
            },
            ProfileDecisionInput {
                id: "srv_d".into(),
                decision: ProfileDecision::Reject,
                copy_name: None,
            },
        ];
        let out = apply(&inputs(true), &decisions, &mut || "srv_new".to_string()).unwrap();
        assert_eq!(ids(&out.profiles), vec!["srv_a", "srv_b", "srv_c"]);
        assert_eq!(out.profiles[1]["name"], "Beta");
        assert_eq!(out.profiles[1]["color"], "#f00");
        assert_eq!(
            op(&out.secret_ops, "server_srv_b"),
            Some(Some("pw-b-local"))
        );
        assert_eq!(op(&out.secret_ops, "server_srv_c"), Some(Some("pw-c")));
        // The rejected addition leaves nothing behind in the vault.
        assert_eq!(op(&out.secret_ops, "server_srv_d"), Some(None));
    }

    #[test]
    fn keeping_both_adds_the_backup_as_a_copy_with_its_own_secrets() {
        let decisions = [ProfileDecisionInput {
            id: "srv_b".into(),
            decision: ProfileDecision::Both,
            copy_name: Some("Beta NAS (backup)".into()),
        }];
        let out = apply(&inputs(true), &decisions, &mut || "srv_copy".to_string()).unwrap();
        assert_eq!(
            ids(&out.profiles),
            vec!["srv_a", "srv_b", "srv_copy", "srv_d"]
        );
        assert_eq!(out.profiles[1]["name"], "Beta");
        assert_eq!(out.profiles[2]["name"], "Beta NAS (backup)");
        assert_eq!(out.profiles[2]["options"], json!({"x": 1}));
        assert_eq!(
            op(&out.secret_ops, "server_srv_b"),
            Some(Some("pw-b-local"))
        );
        assert_eq!(
            op(&out.secret_ops, "server_srv_copy"),
            Some(Some("pw-b-backup"))
        );
    }

    /// A decision the plan cannot honour is refused, never downgraded.
    #[test]
    fn decisions_outside_the_plan_are_refused() {
        let both_on_added = [ProfileDecisionInput {
            id: "srv_d".into(),
            decision: ProfileDecision::Both,
            copy_name: None,
        }];
        let err = apply(&inputs(true), &both_on_added, &mut || {
            "srv_copy".to_string()
        })
        .err()
        .expect("keep both on an added profile");
        assert!(err.contains("Keep both"), "{err}");

        let unknown = [ProfileDecisionInput {
            id: "srv_gone".into(),
            decision: ProfileDecision::Accept,
            copy_name: None,
        }];
        let err = validate(&inputs(true), &unknown).unwrap_err();
        assert!(err.contains("srv_gone"), "{err}");
        // An unchanged profile is not a change either.
        let unchanged = [ProfileDecisionInput {
            id: "srv_a".into(),
            decision: ProfileDecision::Accept,
            copy_name: None,
        }];
        assert!(validate(&inputs(true), &unchanged).is_err());
    }

    /// "Skip existing" keeps this device's side by default even where the
    /// restored partition replaces the list.
    #[test]
    fn skip_existing_defaults_to_this_device_over_a_partition() {
        let mut i = inputs(true);
        i.keep_local_by_default = true;
        let plan = preview(&i);
        for c in &plan.changes {
            let expected = match c.kind {
                ProfileChangeKind::Added => ProfileDecision::Accept,
                _ => ProfileDecision::Reject,
            };
            assert_eq!(c.default_decision, expected, "{}", c.id);
        }
        let out = apply(&i, &[], &mut || unreachable!()).unwrap();
        assert_eq!(ids(&out.profiles), vec!["srv_a", "srv_b", "srv_d", "srv_c"]);
        assert_eq!(out.profiles[1]["name"], "Beta");
    }

    #[test]
    fn nothing_to_plan_when_the_import_leaves_the_list_alone() {
        let mut i = inputs(true);
        i.backup = None;
        assert_eq!(preview(&i).source, ProfileListSource::None);
        let out = apply(&i, &[], &mut || unreachable!()).unwrap();
        assert_eq!(ids(&out.profiles), vec!["srv_a", "srv_b", "srv_c"]);
        assert!(out.secret_ops.is_empty());
    }
}
