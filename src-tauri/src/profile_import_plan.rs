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

use secrecy::zeroize::Zeroizing;

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

/// Whether an import may write a provider's custom OAuth app (client id, secret
/// and region, one unit) when the vault holds `stored_id` and `stored_secret`
/// and the file carries `file_id`: when the slots are empty, or the stored app
/// is this same one, whose secret or region the file then refreshes.
pub fn may_write_oauth_app(
    stored_id: Option<&str>,
    stored_secret: Option<&str>,
    file_id: Option<&str>,
) -> bool {
    match (file_id, stored_id) {
        // The same app: the file refreshes its secret or region.
        (Some(file), Some(stored)) => file == stored,
        // No app stored: a secret left without an id would pair up with the
        // file's id, so the slots must really be empty.
        (Some(_), None) | (None, None) => stored_secret.is_none(),
        (None, Some(_)) => false,
    }
}

/// One per-profile secret an import writes, with the type its partition copy
/// carries (`None` lets the key's prefix decide).
pub struct SecretWrite {
    pub key: String,
    pub kind: Option<&'static str>,
    pub value: Zeroizing<String>,
}

impl SecretWrite {
    pub fn new(key: String, kind: Option<&'static str>, value: &str) -> Self {
        Self {
            key,
            kind,
            value: Zeroizing::new(value.to_string()),
        }
    }
}

/// Where an import's per-profile secrets go. A trait so that a failure at any key
/// can be tested without a vault that fails.
pub trait SecretSlots {
    /// What a key holds before its write.
    type Snapshot;
    fn snapshot(&self, write: &SecretWrite) -> Result<Self::Snapshot, String>;
    fn write(&self, write: &SecretWrite) -> Result<(), String>;
    /// Undo `write` as [`rollback_action`] decides.
    fn restore(&self, write: &SecretWrite, before: Self::Snapshot) -> Result<(), String>;
}

/// How to undo a write of one key, in one place that holds it.
#[derive(Debug, PartialEq, Eq)]
pub enum RollbackAction<'a> {
    /// The key no longer holds what the import wrote: someone changed it since,
    /// and that change stays.
    Leave,
    Restore(&'a str),
    Delete,
}

/// Decide [`RollbackAction`] from what the key holds now, what the import wrote
/// and what it held before.
pub fn rollback_action<'a>(
    current: Option<&str>,
    written: &str,
    before: Option<&'a str>,
) -> RollbackAction<'a> {
    if current != Some(written) {
        return RollbackAction::Leave;
    }
    match before {
        Some(value) => RollbackAction::Restore(value),
        None => RollbackAction::Delete,
    }
}

/// Write `writes` in order, having first read what every key holds, so a key that
/// cannot be read stops the import before anything changes. A failed write puts
/// back every key written so far, itself included, since it may have reached one
/// of its two places. Returns what [`roll_back_secret_writes`] needs. Errors name
/// keys only: the underlying error stays in the log, as it could quote a value.
pub fn apply_secret_writes<S: SecretSlots>(
    slots: &S,
    writes: Vec<SecretWrite>,
) -> Result<Vec<(SecretWrite, S::Snapshot)>, String> {
    let mut pending = Vec::with_capacity(writes.len());
    for write in writes {
        match slots.snapshot(&write) {
            Ok(before) => pending.push((write, before)),
            Err(e) => {
                log::warn!("Import: {} could not be read: {e}", write.key);
                return Err(format!(
                    "the secret stored under {} could not be read, so nothing was imported",
                    write.key
                ));
            }
        }
    }
    let mut applied = Vec::with_capacity(pending.len());
    for (write, before) in pending {
        let result = slots.write(&write);
        applied.push((write, before));
        if let Err(e) = result {
            let failed = applied
                .last()
                .map(|(w, _)| w.key.clone())
                .unwrap_or_default();
            log::warn!("Import: {failed} could not be stored: {e}");
            let left = roll_back_secret_writes(slots, applied);
            let cause = format!("the secret {failed} could not be stored, so nothing was imported");
            return Err(if left.is_empty() {
                format!("{cause}; fix the cause and import the file again")
            } else {
                failed_rollback_message(&cause, &left)
            });
        }
    }
    Ok(applied)
}

/// Undo what [`apply_secret_writes`] wrote, last first, and return the keys that
/// could not be put back.
pub fn roll_back_secret_writes<S: SecretSlots>(
    slots: &S,
    applied: Vec<(SecretWrite, S::Snapshot)>,
) -> Vec<String> {
    let mut left = Vec::new();
    for (write, before) in applied.into_iter().rev() {
        if let Err(e) = slots.restore(&write, before) {
            log::warn!("Import: {} could not be put back: {e}", write.key);
            left.push(write.key.clone());
        }
    }
    left
}

/// What became of [`write_secrets_and_commit`].
pub enum SecretCommit<T> {
    /// The list is stored, with the secrets written.
    Done(T),
    /// The list changed since it was read: the secrets are back, plan again.
    Replan,
}

/// Write `writes`, then run `commit`, the list write. If that fails the secrets go
/// back, and a list another writer changed asks for one more plan while
/// `may_replan` holds. Any other failure, or a change on the last plan, is the
/// error, joined by the keys that could not be put back.
pub fn write_secrets_and_commit<S: SecretSlots, T>(
    slots: &S,
    writes: Vec<SecretWrite>,
    may_replan: bool,
    commit: impl FnOnce() -> Result<T, String>,
) -> Result<SecretCommit<T>, String> {
    let applied = apply_secret_writes(slots, writes)?;
    match commit() {
        Ok(done) => Ok(SecretCommit::Done(done)),
        Err(e) => {
            let left = roll_back_secret_writes(slots, applied);
            if !left.is_empty() {
                Err(failed_rollback_message(&e, &left))
            } else if may_replan && e == crate::user_partitions::PROFILES_CHANGED {
                Ok(SecretCommit::Replan)
            } else {
                Err(e)
            }
        }
    }
}

/// The error for a failed import that also left secrets it could not put back,
/// named by key and never by value.
pub fn failed_rollback_message(cause: &str, left: &[String]) -> String {
    format!(
        "{cause}; these keys could not be put back and keep the file's value: {}. Check the profiles they belong to, or import the file again",
        left.join(", ")
    )
}

/// Lay an import's stored profiles (`servers`, each with its `importOutcome`)
/// over `list`, the list the plan was made on: an overwritten profile over the
/// stored one of its id, where it stands, so a field only this device has
/// survives and the list keeps its order ([`overwrite_profile`]); every other one
/// appended, without its outcome.
pub fn lay_import_over(list: &mut Vec<serde_json::Value>, servers: &[serde_json::Value]) {
    for profile in servers {
        let mut profile = profile.clone();
        let overwrite =
            profile.get("importOutcome").and_then(|v| v.as_str()) == Some("overwritten");
        if let Some(fields) = profile.as_object_mut() {
            fields.remove("importOutcome");
        }
        let slot = list
            .iter()
            .position(|p| p.get("id").is_some() && p.get("id") == profile.get("id"));
        match (overwrite, slot) {
            (true, Some(i)) => list[i] = overwrite_profile(&list[i], &profile),
            _ => list.push(profile),
        }
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
    /// The profiles the import stored or, on a preview, would store, redacted,
    /// each with its `importOutcome`.
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
    /// The whole list as stored for the account (on a preview, the list given).
    pub stored: Vec<serde_json::Value>,
    /// The profiles the write stored under another name than they were sent with.
    pub renamed: Vec<crate::util::profile_name::RenamedProfile>,
    /// Names of the stored profiles whose custom OAuth app differs from the one
    /// this device already has for the provider. The stored app is kept, because
    /// other profiles refresh their tokens with it, so these may need a new
    /// sign-in.
    pub oauth_apps_kept: Vec<String>,
    /// Names of the stored profiles a shared secret or native session of the file
    /// could not reach after the list was stored: they ask for a new sign-in.
    pub shared_secrets_failed: Vec<String>,
    /// Names of the profiles an Overwrite refused because the file points them at
    /// another server ([`same_target`]): they keep their configuration and
    /// secrets, and are also listed in `skipped`. Duplicate adds the file's copy.
    pub target_changed: Vec<String>,
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
const STORED_SECRET_FLAGS: [&str; 6] = [
    "hasStoredCredential",
    "hasStoredFilenApiKey",
    "hasStoredAeroCryptPassword",
    "hasStoredAeroCryptSalt",
    "hasStoredAeroCryptKeyfilePath",
    "hasStoredAeroCryptConfig",
];

/// Profile fields the export format omits when empty (`skip_serializing_if` on
/// `ServerProfileExport`), and an older file lacks altogether: a null here says
/// nothing about the backup. An overwrite keeps the stored value, which for
/// `aeroCryptOverlay` is also the safe side: turning an overlay off by accident
/// would upload plaintext into an encrypted folder.
const OMITTED_WHEN_EMPTY: [&str; 6] = [
    "publicUrlBase",
    "customIconUrl",
    "faviconUrl",
    "skipDeltaEligibilityPrompt",
    "aeroCryptOverlay",
    "persistModeCredentials",
];

/// The options that decide where a stored secret travels and how: which server
/// a connection reaches, and whether it is protected on the way (a backup that
/// turns TLS or certificate checks off must not take a saved password with it).
/// A bucket or a container is not here: it is a place on the same server, for
/// the same account, over the same connection.
const OPTIONS_THAT_GUARD_THE_SECRET: [&str; 13] = [
    "endpoint",
    "region",
    "accountId",
    "accountName",
    "jurisdiction",
    "pcloudRegion",
    "infinicloudNode",
    "tlsMode",
    "ftpsMode",
    "webdavScheme",
    "verifyCert",
    "allowCleartextEndpoint",
    "allowCleartextStorage",
];

/// Whether a stored secret may go where `b` connects, having been saved for
/// where `a` connects: the same protocol, provider, host, port and account, and
/// the same value for [`OPTIONS_THAT_GUARD_THE_SECRET`]. This answers a narrower
/// question than the frontend's `isSameConnection`, which also tells two buckets
/// apart: a backup that only changes a bucket, or any other setting, stays an
/// overwrite. Absent, null and empty values are the same absence, an absent
/// `protocol` is FTP and an absent `verifyCert` is true, as when connecting.
pub fn same_target(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    let present = |value: Option<&serde_json::Value>| -> serde_json::Value {
        match value {
            None | Some(serde_json::Value::Null) => serde_json::Value::Null,
            Some(serde_json::Value::String(s)) if s.is_empty() => serde_json::Value::Null,
            Some(value) => value.clone(),
        }
    };
    // `host:2222` with port 2222 reads as `host`, as in `isSameConnection`. A
    // scheme and a host name are case-insensitive, and only those are folded: a
    // user name, path or query around them can be case-sensitive.
    let host = |p: &serde_json::Value| -> String {
        let host = p.get("host").and_then(|v| v.as_str()).unwrap_or("");
        let port = p.get("port").and_then(|v| v.as_u64()).unwrap_or(0);
        let host = host.strip_suffix(&format!(":{port}")).unwrap_or(host);
        let scheme_end = host.find("://").map_or(0, |at| at + 3);
        let end = host[scheme_end..]
            .find(['/', '?', '#'])
            .map_or(host.len(), |at| scheme_end + at);
        let name_start = host[scheme_end..end]
            .rfind('@')
            .map_or(scheme_end, |at| scheme_end + at + 1);
        format!(
            "{}{}{}{}",
            host[..scheme_end].to_ascii_lowercase(),
            &host[scheme_end..name_start],
            host[name_start..end].to_ascii_lowercase(),
            &host[end..]
        )
    };
    // The value a connection would use when a field is left out.
    let protocol = |p: &serde_json::Value| match present(p.get("protocol")) {
        serde_json::Value::Null => serde_json::json!("ftp"),
        value => value,
    };
    let option =
        |p: &serde_json::Value, key: &str| match present(p.get("options").and_then(|o| o.get(key)))
        {
            serde_json::Value::Bool(true) if key == "verifyCert" => serde_json::Value::Null,
            value => value,
        };
    host(a) == host(b)
        && protocol(a) == protocol(b)
        && ["port", "username", "providerId"]
            .iter()
            .all(|key| present(a.get(key)) == present(b.get(key)))
        && OPTIONS_THAT_GUARD_THE_SECRET
            .iter()
            .all(|key| option(a, key) == option(b, key))
}

/// The profile an overwrite stores: `existing` with the fields of `imported`
/// laid over it. A field the format always writes follows the file, null
/// included, and so does `options` as a whole, except that an options-borne
/// secret the file does not carry (it was exported without credentials) stays.
/// A field of [`OMITTED_WHEN_EMPTY`] left null keeps the stored value, as does
/// a field outside the format, one only this device knows. Each
/// [`STORED_SECRET_FLAGS`] flag is set when either side has it. An import that
/// would point the profile at another server never gets here ([`same_target`]).
pub fn overwrite_profile(
    existing: &serde_json::Value,
    imported: &serde_json::Value,
) -> serde_json::Value {
    let mut merged = existing.clone();
    if let (Some(target), Some(source)) = (merged.as_object_mut(), imported.as_object()) {
        for (key, value) in source {
            if value.is_null() && OMITTED_WHEN_EMPTY.contains(&key.as_str()) {
                continue;
            }
            target.insert(key.clone(), value.clone());
        }
        let kept: serde_json::Map<String, serde_json::Value> = existing
            .get("options")
            .and_then(|v| v.as_object())
            .into_iter()
            .flatten()
            .filter(|(key, _)| crate::profile_export::OPTIONS_SECRET_KEYS.contains(&key.as_str()))
            .filter(|(key, _)| {
                source
                    .get("options")
                    .and_then(|v| v.get(key.as_str()))
                    .is_none()
            })
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        if !kept.is_empty() {
            let mut options = target
                .get("options")
                .and_then(|v| v.as_object())
                .cloned()
                .unwrap_or_default();
            options.extend(kept);
            target.insert("options".to_string(), serde_json::Value::Object(options));
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
    use std::cell::RefCell;
    use std::collections::HashMap;

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

    /// The file's fields win, nulls and options included, except what the file
    /// cannot speak to: a field the format omits when empty, an options-borne
    /// secret it does not carry, a field only the device has. A stored secret
    /// flag stays set.
    #[test]
    fn overwrite_lays_the_file_over_the_profile_and_keeps_its_stored_flags() {
        let existing = json!({
            "id": "srv_a", "name": "Old", "host": "old.example", "localOnly": "kept",
            "color": "#f00", "initialPath": "/old",
            "aeroCryptOverlay": {"enabled": true, "kind": "rclone-crypt"},
            "hasStoredCredential": true, "hasStoredAeroCryptPassword": false,
            "options": {"verifyCert": false, "timeout": 30, "totp_secret": "stored-seed",
                "sasToken": "stored-sas"}
        });
        let imported = json!({
            "id": "srv_a", "name": "New", "host": "new.example",
            "color": null, "initialPath": null,
            "aeroCryptOverlay": null, "hasStoredCredential": false,
            "hasStoredAeroCryptPassword": true,
            "options": {"timeout": 60, "sasToken": "file-sas"}
        });
        let merged = overwrite_profile(&existing, &imported);
        assert_eq!(merged["name"], "New");
        assert_eq!(merged["host"], "new.example");
        // A field the format always writes follows the file, null included.
        assert_eq!(merged["color"], serde_json::Value::Null);
        assert_eq!(merged["initialPath"], serde_json::Value::Null);
        // A field the format omits when empty says nothing when null: an
        // overlay is never turned off by a file that does not mention it.
        assert_eq!(merged["aeroCryptOverlay"]["kind"], "rclone-crypt");
        // The file's options are the profile's, plus a stored secret the file
        // does not carry; a local non-secret option does not survive.
        assert_eq!(
            merged["options"],
            json!({"timeout": 60, "sasToken": "file-sas", "totp_secret": "stored-seed"})
        );

        // A backup exported without credentials (its options stripped to
        // nothing) still leaves the stored secret in place.
        let stripped = json!({"id": "srv_a", "options": null});
        let merged = overwrite_profile(&existing, &stripped);
        assert_eq!(
            merged["options"],
            json!({"totp_secret": "stored-seed", "sasToken": "stored-sas"})
        );

        let merged = overwrite_profile(&existing, &imported);
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

    /// The OAuth app unit: a new or the same app is written, another app is
    /// not, and a secret without an id only goes into empty slots.
    #[test]
    fn an_oauth_app_is_written_only_when_free_or_the_same() {
        assert!(may_write_oauth_app(None, None, Some("a")));
        assert!(!may_write_oauth_app(None, Some("orphan"), Some("a")));
        assert!(may_write_oauth_app(Some("a"), Some("s"), Some("a")));
        assert!(!may_write_oauth_app(Some("a"), Some("s"), Some("b")));
        assert!(may_write_oauth_app(None, None, None));
        assert!(!may_write_oauth_app(None, Some("s"), None));
        assert!(!may_write_oauth_app(Some("a"), None, None));
    }

    /// What decides a move to another server or a weaker connection, and what
    /// does not.
    #[test]
    fn same_target_tells_another_server_from_another_setting() {
        let stored = json!({
            "host": "old.example", "port": 21, "username": "u", "protocol": "s3",
            "options": {"endpoint": "https://a.example", "bucket": "one", "timeout": 30}
        });
        // A bucket, a path or any other setting stays on the same server.
        let same = [
            json!({"host": "old.example", "port": 21, "username": "u", "protocol": "s3",
                "initialPath": "/x", "options": {"endpoint": "https://a.example", "bucket": "two"}}),
            json!({"host": "old.example:21", "port": 21, "username": "u", "protocol": "s3",
                "providerId": "", "options": {"endpoint": "https://a.example"}}),
        ];
        for target in same {
            assert!(same_target(&stored, &target), "{target}");
        }
        for key in ["host", "port", "username", "protocol", "providerId"] {
            let mut moved = stored.clone();
            moved[key] = json!(if key == "port" {
                json!(22)
            } else {
                json!("other")
            });
            assert!(!same_target(&stored, &moved), "{key}");
        }
        for key in OPTIONS_THAT_GUARD_THE_SECRET {
            let mut moved = stored.clone();
            moved["options"][key] = json!("other");
            assert!(!same_target(&stored, &moved), "{key}");
        }
        // A host name is case-insensitive; a user name, path or query is not.
        let name = |host: &str| json!({"host": host, "port": 21, "username": "u"});
        assert!(!same_target(&name("Alice@host"), &name("alice@host")));
        assert!(same_target(&name("alice@HOST"), &name("alice@host")));
        assert!(!same_target(
            &name("https://h?Token=A"),
            &name("https://h?token=a")
        ));
        assert!(same_target(
            &name("FTP.Example.com"),
            &name("ftp.example.com")
        ));
        assert!(same_target(
            &name("HTTPS://DAV.Example.com/Files"),
            &name("https://dav.example.com/Files")
        ));
        assert!(!same_target(
            &name("https://dav.example.com/Files"),
            &name("https://dav.example.com/files")
        ));
        // A backup that weakens the connection takes no saved password with it.
        for (key, value) in [
            ("tlsMode", json!("none")),
            ("webdavScheme", json!("http")),
            ("verifyCert", json!(false)),
        ] {
            let mut weaker = stored.clone();
            weaker["options"][key] = value;
            assert!(!same_target(&stored, &weaker), "{key}");
        }
        // The defaults a connection would use read as the same target.
        let mut explicit = stored.clone();
        explicit["options"]["verifyCert"] = json!(true);
        assert!(same_target(&stored, &explicit));
        let no_protocol = json!({"host": "h", "port": 21, "username": "u"});
        let ftp = json!({"host": "h", "port": 21, "username": "u", "protocol": "ftp"});
        assert!(same_target(&no_protocol, &ftp));
    }

    /// A fake store: its keys, and the keys whose write or restore fails, with an
    /// error text that quotes the value as a real one could.
    #[derive(Default)]
    struct FakeSlots {
        values: RefCell<HashMap<String, String>>,
        fail_write: Option<&'static str>,
        fail_restore: Option<&'static str>,
    }

    impl SecretSlots for FakeSlots {
        type Snapshot = Option<String>;
        fn snapshot(&self, write: &SecretWrite) -> Result<Self::Snapshot, String> {
            Ok(self.values.borrow().get(&write.key).cloned())
        }
        fn write(&self, write: &SecretWrite) -> Result<(), String> {
            if self.fail_write == Some(write.key.as_str()) {
                return Err(format!("rejected value {}", write.value.as_str()));
            }
            self.values
                .borrow_mut()
                .insert(write.key.clone(), write.value.to_string());
            Ok(())
        }
        fn restore(&self, write: &SecretWrite, before: Self::Snapshot) -> Result<(), String> {
            if self.fail_restore == Some(write.key.as_str()) {
                return Err(format!("rejected value {}", write.value.as_str()));
            }
            let mut values = self.values.borrow_mut();
            let current = values.get(&write.key).cloned();
            match rollback_action(current.as_deref(), &write.value, before.as_deref()) {
                RollbackAction::Leave => {}
                RollbackAction::Restore(value) => {
                    values.insert(write.key.clone(), value.to_string());
                }
                RollbackAction::Delete => {
                    values.remove(&write.key);
                }
            }
            Ok(())
        }
    }

    fn secret_writes() -> Vec<SecretWrite> {
        vec![
            SecretWrite::new("server_a".into(), None, "file-a"),
            SecretWrite::new("server_b".into(), None, "file-b"),
            SecretWrite::new("server_c".into(), None, "file-c"),
        ]
    }

    #[test]
    fn rollback_touches_a_key_only_while_it_holds_what_the_import_wrote() {
        assert_eq!(
            rollback_action(Some("file"), "file", Some("old")),
            RollbackAction::Restore("old")
        );
        assert_eq!(
            rollback_action(Some("file"), "file", None),
            RollbackAction::Delete
        );
        // Another writer stored its value since: it stays.
        assert_eq!(
            rollback_action(Some("newer"), "file", Some("old")),
            RollbackAction::Leave
        );
        assert_eq!(
            rollback_action(None, "file", Some("old")),
            RollbackAction::Leave
        );
    }

    /// A write failing at any key leaves every key as it was before the import,
    /// and the error names that key, not a value.
    #[test]
    fn a_failed_write_puts_back_every_key_and_names_only_the_key() {
        for failing in ["server_a", "server_b", "server_c"] {
            let slots = FakeSlots {
                fail_write: Some(failing),
                ..Default::default()
            };
            slots
                .values
                .borrow_mut()
                .insert("server_a".into(), "stored-a".into());
            let message = match apply_secret_writes(&slots, secret_writes()) {
                Ok(_) => panic!("the write of {failing} must fail"),
                Err(message) => message,
            };
            let values = slots.values.borrow();
            assert_eq!(values.get("server_a").map(String::as_str), Some("stored-a"));
            assert!(!values.contains_key("server_b"));
            assert!(!values.contains_key("server_c"));
            assert!(message.contains(failing), "{message}");
            assert!(message.contains("nothing was imported"), "{message}");
            assert!(!message.contains("file-"), "{message}");
        }
    }

    /// A key that cannot be put back is named in the error, without its value,
    /// and the others are still put back.
    #[test]
    fn a_failed_rollback_names_the_key_and_never_its_value() {
        let slots = FakeSlots {
            fail_write: Some("server_c"),
            fail_restore: Some("server_b"),
            ..Default::default()
        };
        let message = match apply_secret_writes(&slots, secret_writes()) {
            Ok(_) => panic!("the write of server_c must fail"),
            Err(message) => message,
        };
        assert!(message.contains("server_b"), "{message}");
        assert!(message.contains("keep the file's value"), "{message}");
        assert!(!message.contains("file-b"), "{message}");
        let values = slots.values.borrow();
        assert!(!values.contains_key("server_a"));
        assert_eq!(values.get("server_b").map(String::as_str), Some("file-b"));
    }

    /// A list write that fails takes the secrets back, except a value another
    /// writer stored since, and the import fails with that write's error.
    #[test]
    fn a_failed_list_write_puts_the_secrets_back_and_keeps_a_newer_value() {
        let slots = FakeSlots::default();
        let result = write_secrets_and_commit(&slots, secret_writes(), true, || {
            slots
                .values
                .borrow_mut()
                .insert("server_b".into(), "newer".into());
            Err::<(), _>("disk full".to_string())
        });
        assert_eq!(result.err().as_deref(), Some("disk full"));
        let values = slots.values.borrow();
        assert!(!values.contains_key("server_a"));
        assert_eq!(values.get("server_b").map(String::as_str), Some("newer"));
        assert!(!values.contains_key("server_c"));
    }

    /// A list changed under the import asks for one more plan, with the secrets
    /// back; changed again on the last plan, it is the error.
    #[test]
    fn a_changed_list_is_planned_once_more_and_then_refused() {
        use crate::user_partitions::PROFILES_CHANGED;
        let slots = FakeSlots::default();
        let replan = write_secrets_and_commit(&slots, secret_writes(), true, || {
            Err::<(), _>(PROFILES_CHANGED.to_string())
        });
        assert!(matches!(replan, Ok(SecretCommit::Replan)));
        assert!(slots.values.borrow().is_empty());
        let last = write_secrets_and_commit(&slots, secret_writes(), false, || {
            Err::<(), _>(PROFILES_CHANGED.to_string())
        });
        assert_eq!(last.err().as_deref(), Some(PROFILES_CHANGED));
        assert!(slots.values.borrow().is_empty());
    }

    #[test]
    fn a_stored_list_keeps_the_secrets_and_a_key_left_behind_is_named() {
        let slots = FakeSlots::default();
        let done = write_secrets_and_commit(&slots, secret_writes(), true, || Ok(7));
        assert!(matches!(done, Ok(SecretCommit::Done(7))));
        assert_eq!(slots.values.borrow().len(), 3);

        let slots = FakeSlots {
            fail_restore: Some("server_a"),
            ..Default::default()
        };
        let message = write_secrets_and_commit(&slots, secret_writes(), true, || {
            Err::<(), _>("disk full".to_string())
        })
        .err()
        .expect("the list write failed");
        assert!(message.starts_with("disk full"), "{message}");
        assert!(message.contains("server_a"), "{message}");
        assert!(!message.contains("file-a"), "{message}");
    }

    #[test]
    fn an_import_lays_overwrites_in_place_and_appends_the_rest() {
        let mut list = vec![
            json!({"id": "a", "name": "kept", "deviceOnly": 1}),
            json!({"id": "b", "name": "other"}),
        ];
        lay_import_over(
            &mut list,
            &[
                json!({"id": "a", "name": "file", "importOutcome": "overwritten"}),
                json!({"id": "c", "name": "new", "importOutcome": "added"}),
            ],
        );
        assert_eq!(list[0]["name"], "file");
        assert_eq!(list[0]["deviceOnly"], 1);
        assert_eq!(list[1]["name"], "other");
        assert_eq!(list[2], json!({"id": "c", "name": "new"}));
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
