// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! What a `.aeroftp` profile import does with a profile that already exists.
//!
//! A profile in the file "already exists" when the active user holds a profile
//! of the same stable id, which is what a re-import of one's own backup looks
//! like. The user picks one [`ExistingProfileMode`] for the whole import:
//! - `Skip` keeps the existing profile and its secrets untouched;
//! - `Overwrite` replaces the profile's configuration in place, under the same
//!   id, so favourites and groups (both keyed by id) still hold it, and writes
//!   every secret the file carries over the existing one; a secret the file
//!   does not carry stays as it is;
//! - `Duplicate` imports the profile under a new id beside the existing one,
//!   with its secrets moved to that id; the profile write then gives it the
//!   first free " (1)", " (2)" name suffix.
//!
//! A profile that only shares host:port:username with an existing one is never
//! a conflict: it is added in every mode and flagged as `similar`, since two
//! profiles of one account legitimately differ by protocol, crypt overlay,
//! folder or auth mode. A profile whose id an earlier profile of the same file
//! already used is skipped, or stored under a new id with `Duplicate`, so no
//! import stores two profiles of one id. The GUI (`import_server_profiles`) and the CLI
//! (`profile-import`) both plan through [`plan_import`].

use std::collections::HashSet;

/// What to do with a profile of the file whose id the active user already has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ExistingProfileMode {
    /// Keep the existing profile and its secrets (the behaviour before modes).
    #[default]
    Skip,
    /// Replace the existing profile and the secrets the file carries.
    Overwrite,
    /// Import a copy under a new id and a suffixed name.
    Duplicate,
}

/// What the import did, or would do on a dry run, with one profile of the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ImportOutcome {
    /// New id: added.
    Added,
    /// Existing id, `Overwrite`: replaced in place.
    Overwritten,
    /// Existing id, `Duplicate`: added under a new id.
    Duplicated,
    /// Existing id, `Skip`: left alone.
    Skipped,
}

/// The decision for one profile of the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedProfile {
    /// Position of the profile in the file.
    pub index: usize,
    /// What the import does with it.
    pub outcome: ImportOutcome,
    /// The id the profile is stored under: the file's id, or a new one for a
    /// duplicate or a profile the file gives no id.
    pub id: String,
    /// Shares host:port:username with a profile of another id.
    pub similar: bool,
}

/// One profile of the file, as far as planning needs it.
pub struct ImportCandidate<'a> {
    /// The stable profile id, the key a conflict is decided on.
    pub id: &'a str,
    /// Host, with port and username the account a `similar` profile shares.
    pub host: &'a str,
    /// Port of that account.
    pub port: u64,
    /// Username of that account.
    pub username: &'a str,
}

/// The (host, port, username) two profiles of one account share. A tuple, not
/// a joined string: a host or username may contain the separator (an IPv6
/// host does), and two accounts must not collide.
fn account_key(host: &str, port: u64, username: &str) -> (String, u64, String) {
    (host.to_string(), port, username.to_string())
}

/// Decide what happens to each profile of the file (`imported`, in file order)
/// against the active user's `current` profiles. `new_id` mints ids for
/// duplicates and for profiles the file gives no id (each is added under its
/// own, so two of them never share a vault key); one already taken by a
/// current profile or an earlier decision is drawn again.
pub fn plan_import(
    imported: &[ImportCandidate<'_>],
    current: &[serde_json::Value],
    mode: ExistingProfileMode,
    mut new_id: impl FnMut() -> String,
) -> Vec<PlannedProfile> {
    let existing_ids: HashSet<&str> = current
        .iter()
        .filter_map(|p| p.get("id").and_then(|v| v.as_str()))
        .filter(|s| !s.is_empty())
        .collect();
    let existing_accounts: HashSet<(String, u64, String)> = current
        .iter()
        .map(|p| {
            account_key(
                p.get("host").and_then(|v| v.as_str()).unwrap_or(""),
                p.get("port").and_then(|v| v.as_u64()).unwrap_or(0),
                p.get("username").and_then(|v| v.as_str()).unwrap_or(""),
            )
        })
        .collect();
    let mut taken: HashSet<String> = existing_ids.iter().map(|s| s.to_string()).collect();
    taken.extend(imported.iter().map(|p| p.id.to_string()));
    let mut seen_in_file: HashSet<&str> = HashSet::new();
    let mut mint = || {
        let mut id = new_id();
        while !taken.insert(id.clone()) {
            id = new_id();
        }
        id
    };

    imported
        .iter()
        .enumerate()
        .map(|(index, p)| {
            // A second profile of the file under an id the file already used
            // conflicts with that earlier one, not with a stored profile:
            // Overwrite replaces only what the device holds, so it skips the
            // repeat like Skip does, and Duplicate stores it under a new id.
            let repeated = !p.id.is_empty() && !seen_in_file.insert(p.id);
            let exists = !p.id.is_empty() && existing_ids.contains(p.id);
            let (outcome, id) = match (repeated, exists, mode) {
                _ if p.id.is_empty() => (ImportOutcome::Added, mint()),
                (_, _, ExistingProfileMode::Duplicate) if repeated || exists => {
                    (ImportOutcome::Duplicated, mint())
                }
                (true, _, _) | (false, true, ExistingProfileMode::Skip) => {
                    (ImportOutcome::Skipped, p.id.to_string())
                }
                (false, true, _) => (ImportOutcome::Overwritten, p.id.to_string()),
                (false, false, _) => (ImportOutcome::Added, p.id.to_string()),
            };
            // An overwrite replaces the one profile it shares an id with; it is
            // not a second profile of that account.
            let similar = matches!(outcome, ImportOutcome::Added | ImportOutcome::Duplicated)
                && existing_accounts.contains(&account_key(p.host, p.port, p.username));
            PlannedProfile {
                index,
                outcome,
                id,
                similar,
            }
        })
        .collect()
}

/// What the import refuses with when the active user is no longer the one whose
/// profiles it compared the file with: the secrets would land in another
/// account's partition.
pub const NOT_THE_ACTIVE_USER: &str = "the active account is not the one whose profiles this import read, so nothing was restored or written; make that account the active one (in the CLI, --user must name the active account) and run the import again";

/// A stored profile named in a report: its id and the name the file gave it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ImportedProfileRef {
    /// The id the profile is stored under.
    pub id: String,
    /// Its name in the file (see [`display_name`]).
    pub name: String,
}

/// Whether an import may write a provider-wide singleton (an OAuth app's
/// credentials, the legacy Jottacloud chain) that the vault holds as `stored`:
/// when it holds none, or when it holds `own`, the value this import's own
/// profile or app already had there, which the import then refreshes. Another
/// profile's value is never swapped for the file's.
pub fn may_write_shared(stored: Option<&str>, own: Option<&str>) -> bool {
    match stored {
        None => true,
        Some(value) => own == Some(value),
    }
}

/// How a report names a profile: its name, or `(host)` when it has none.
pub fn display_name(name: &str, host: &str) -> String {
    if name.is_empty() {
        format!("({host})")
    } else {
        name.to_string()
    }
}

/// The result of an import, or of its dry run.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileImportReport {
    /// The profiles to store, redacted, each with its `importOutcome`: added
    /// and duplicated ones to append, overwritten ones to lay over the stored
    /// profile of their id with [`overwrite_profile`] when the list is written,
    /// against the record read in that same write, so an edit made since the
    /// import read the list is kept.
    pub servers: Vec<serde_json::Value>,
    /// Names (see [`display_name`]) of the profiles skipped because the active
    /// user already has them, or the file already gave their id.
    pub skipped: Vec<String>,
    /// Stored profiles that share host:port:username with a profile of another id.
    pub similar: usize,
    /// The stored Jottacloud profiles whose refresh chain came with the file.
    /// Jottacloud rotates that chain on every connection, so the device or
    /// profile that connects first with it signs the other copies out. The id
    /// finds the name the profile write gave it.
    pub jotta_chains: Vec<ImportedProfileRef>,
    /// How many stored profiles had secrets in the file that were restored
    /// (0 on a dry run). The restore comes before the caller writes the list,
    /// so this is what a failed write leaves in the vault.
    pub secrets_stored: usize,
    /// The user whose profiles the file was compared with and whose partition
    /// the secrets went to: the caller writes the list for this user only.
    pub user_id: Option<i64>,
    /// How many profiles the file holds, whatever their outcome.
    pub total_in_file: usize,
    /// The file's public envelope metadata.
    pub metadata: serde_json::Value,
}

/// A new profile id in the frontend's `srv_<millis>_<9 chars>` shape.
pub fn new_profile_id() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let random = uuid::Uuid::new_v4().simple().to_string();
    format!("srv_{millis}_{}", &random[..9])
}

/// The secret-presence flags of a profile. On an overwrite a secret the file
/// does not carry stays in the vault, so its flag stays set too.
pub const STORED_SECRET_FLAGS: [&str; 6] = [
    "hasStoredCredential",
    "hasStoredFilenApiKey",
    "hasStoredAeroCryptPassword",
    "hasStoredAeroCryptSalt",
    "hasStoredAeroCryptKeyfilePath",
    "hasStoredAeroCryptConfig",
];

/// The profile an overwrite stores: `existing` with every field of `imported`
/// laid over it, and each [`STORED_SECRET_FLAGS`] flag set when either side has
/// it. The file is a whole snapshot of the profile's configuration: a field it
/// carries as null and its `options` object are the backup's values (a Crypt
/// overlay turned off on the source device must stay off), so they replace the
/// stored ones; only a field the export format does not carry, one only this
/// device knows, survives.
pub fn overwrite_profile(
    existing: &serde_json::Value,
    imported: &serde_json::Value,
) -> serde_json::Value {
    let mut merged = existing.clone();
    if let (Some(target), Some(source)) = (merged.as_object_mut(), imported.as_object()) {
        for (key, value) in source {
            target.insert(key.clone(), value.clone());
        }
        for flag in STORED_SECRET_FLAGS {
            let set = |p: &serde_json::Value| p.get(flag).and_then(|v| v.as_bool()) == Some(true);
            if set(existing) || set(imported) {
                target.insert(flag.to_string(), serde_json::Value::Bool(true));
            }
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A profile of the file on port 21 as user `u`.
    fn candidate<'a>(id: &'a str, host: &'a str) -> ImportCandidate<'a> {
        ImportCandidate {
            id,
            host,
            port: 21,
            username: "u",
        }
    }

    /// The device's profiles: `srv_a` and `srv_b`.
    fn current() -> Vec<serde_json::Value> {
        vec![
            json!({"id": "srv_a", "host": "a.example", "port": 21, "username": "u"}),
            json!({"id": "srv_b", "host": "b.example", "port": 21, "username": "u"}),
        ]
    }

    /// The outcomes of `plan`, in file order.
    fn outcomes(plan: &[PlannedProfile]) -> Vec<ImportOutcome> {
        plan.iter().map(|p| p.outcome).collect()
    }

    /// Skip, Overwrite and Duplicate act only on a profile whose id the device
    /// has; the others are added, and only an added copy of an account is similar.
    #[test]
    fn each_mode_decides_only_the_profiles_whose_id_exists() {
        let file = [
            candidate("srv_a", "a.example"),
            candidate("srv_new", "b.example"),
            candidate("srv_other", "c.example"),
        ];
        let mut n = 0;
        let mut mint = || {
            n += 1;
            format!("srv_minted_{n}")
        };
        let skip = plan_import(&file, &current(), ExistingProfileMode::Skip, &mut mint);
        assert_eq!(
            outcomes(&skip),
            [
                ImportOutcome::Skipped,
                ImportOutcome::Added,
                ImportOutcome::Added
            ]
        );
        let overwrite = plan_import(&file, &current(), ExistingProfileMode::Overwrite, &mut mint);
        assert_eq!(overwrite[0].outcome, ImportOutcome::Overwritten);
        assert_eq!(overwrite[0].id, "srv_a");
        let duplicate = plan_import(&file, &current(), ExistingProfileMode::Duplicate, &mut mint);
        assert_eq!(duplicate[0].outcome, ImportOutcome::Duplicated);
        assert_ne!(duplicate[0].id, "srv_a");
        assert_eq!(duplicate[1].id, "srv_new");
        // Only a new profile on an existing account is similar; the overwrite
        // and a profile on a new account are not.
        assert_eq!(
            overwrite.iter().map(|p| p.similar).collect::<Vec<_>>(),
            [false, true, false]
        );
        assert!(duplicate[0].similar);
    }

    /// A minted id already held by the device or the file is drawn again.
    #[test]
    fn duplicate_ids_avoid_every_id_in_use() {
        let file = [
            candidate("srv_a", "a.example"),
            candidate("srv_b", "b.example"),
        ];
        // The minter first returns ids already held by a current profile and by
        // another profile of the file, then the same fresh id twice.
        let mut queue = vec!["srv_a", "srv_b", "srv_fresh", "srv_fresh", "srv_fresh2"].into_iter();
        let plan = plan_import(&file, &current(), ExistingProfileMode::Duplicate, || {
            queue.next().unwrap().to_string()
        });
        assert_eq!(plan[0].id, "srv_fresh");
        assert_eq!(plan[1].id, "srv_fresh2");
    }

    /// A repeat of an id within the file is skipped, or duplicated with
    /// Duplicate, so no id is stored twice.
    #[test]
    fn an_id_the_file_repeats_is_skipped_or_duplicated_never_stored_twice() {
        // srv_dup is new to the device and appears twice; srv_a exists and
        // appears twice too.
        let file = [
            candidate("srv_dup", "d.example"),
            candidate("srv_dup", "d.example"),
            candidate("srv_a", "a.example"),
            candidate("srv_a", "a.example"),
        ];
        let mut n = 0;
        let mut mint = || {
            n += 1;
            format!("srv_minted_{n}")
        };
        let skip = plan_import(&file, &current(), ExistingProfileMode::Skip, &mut mint);
        assert_eq!(
            outcomes(&skip),
            [
                ImportOutcome::Added,
                ImportOutcome::Skipped,
                ImportOutcome::Skipped,
                ImportOutcome::Skipped
            ]
        );
        let overwrite = plan_import(&file, &current(), ExistingProfileMode::Overwrite, &mut mint);
        assert_eq!(
            outcomes(&overwrite),
            [
                ImportOutcome::Added,
                ImportOutcome::Skipped,
                ImportOutcome::Overwritten,
                ImportOutcome::Skipped
            ]
        );
        let duplicate = plan_import(&file, &current(), ExistingProfileMode::Duplicate, &mut mint);
        assert_eq!(
            outcomes(&duplicate),
            [
                ImportOutcome::Added,
                ImportOutcome::Duplicated,
                ImportOutcome::Duplicated,
                ImportOutcome::Duplicated
            ]
        );
        let ids: HashSet<&str> = duplicate.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids.len(), 4, "every stored profile has its own id");
    }

    /// Profiles the file gives no id each get their own new one, in every mode.
    #[test]
    fn profiles_without_an_id_get_their_own() {
        let file = [candidate("", "x.example"), candidate("", "y.example")];
        for mode in [
            ExistingProfileMode::Skip,
            ExistingProfileMode::Overwrite,
            ExistingProfileMode::Duplicate,
        ] {
            let mut n = 0;
            let plan = plan_import(&file, &current(), mode, || {
                n += 1;
                format!("srv_minted_{n}")
            });
            assert_eq!(
                outcomes(&plan),
                [ImportOutcome::Added, ImportOutcome::Added]
            );
            assert_eq!(plan[0].id, "srv_minted_1");
            assert_eq!(plan[1].id, "srv_minted_2");
        }
    }

    /// Accounts are compared field by field: a colon in the host or username
    /// (an IPv6 host) never makes two accounts look like one.
    #[test]
    fn accounts_with_colons_do_not_collide() {
        let device = vec![json!({"id": "srv_v6", "host": "fe80::1", "port": 21, "username": "u"})];
        let file = [ImportCandidate {
            id: "srv_other",
            host: "fe80:",
            port: 1,
            username: "21:u",
        }];
        let plan = plan_import(&file, &device, ExistingProfileMode::Skip, || unreachable!());
        assert!(!plan[0].similar);
    }

    /// New ids look like the frontend's `srv_<millis>_<9 chars>` and differ.
    #[test]
    fn new_ids_have_the_frontend_shape() {
        let id = new_profile_id();
        let parts: Vec<&str> = id.split('_').collect();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0], "srv");
        assert!(parts[1].chars().all(|c| c.is_ascii_digit()));
        assert_eq!(parts[2].len(), 9);
        assert_ne!(new_profile_id(), id);
    }

    /// The file's fields win, nulls and options included, a field only the
    /// device has stays, and a stored secret flag stays set.
    #[test]
    fn overwrite_lays_the_file_over_the_profile_and_keeps_its_stored_flags() {
        let existing = json!({
            "id": "srv_a", "name": "Old", "host": "old.example", "localOnly": "kept",
            "aeroCryptOverlay": {"enabled": true, "kind": "rclone-crypt"},
            "hasStoredCredential": true, "hasStoredAeroCryptPassword": false,
            "options": {"verifyCert": false, "timeout": 30}
        });
        let imported = json!({
            "id": "srv_a", "name": "New", "host": "new.example",
            "aeroCryptOverlay": null, "hasStoredCredential": false,
            "hasStoredAeroCryptPassword": true, "options": {"timeout": 60}
        });
        let merged = overwrite_profile(&existing, &imported);
        assert_eq!(merged["name"], "New");
        assert_eq!(merged["host"], "new.example");
        // The backup turned the Crypt overlay off: it stays off.
        assert_eq!(merged["aeroCryptOverlay"], serde_json::Value::Null);
        // The backup's options are the profile's options.
        assert_eq!(merged["options"], json!({"timeout": 60}));
        assert_eq!(merged["localOnly"], "kept");
        assert_eq!(merged["hasStoredCredential"], true);
        assert_eq!(merged["hasStoredAeroCryptPassword"], true);
    }

    /// A shared singleton is written when empty or when it is this import's
    /// own value, never over another profile's.
    #[test]
    fn shared_singletons_are_written_only_when_empty_or_own() {
        assert!(may_write_shared(None, None));
        assert!(may_write_shared(None, Some("app-a")));
        assert!(may_write_shared(Some("app-a"), Some("app-a")));
        assert!(!may_write_shared(Some("app-a"), Some("app-b")));
        assert!(!may_write_shared(Some("app-a"), None));
    }

    /// The GUI sends the mode as a lowercase string.
    #[test]
    fn modes_parse_from_lowercase() {
        for (text, mode) in [
            ("\"skip\"", ExistingProfileMode::Skip),
            ("\"overwrite\"", ExistingProfileMode::Overwrite),
            ("\"duplicate\"", ExistingProfileMode::Duplicate),
        ] {
            assert_eq!(
                serde_json::from_str::<ExistingProfileMode>(text).unwrap(),
                mode
            );
        }
    }
}
