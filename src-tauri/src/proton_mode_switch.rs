// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! A Proton Drive profile that changes client keeps pointing at the same
//! folders (Requested 79 on #1318).
//!
//! The two clients spell one folder differently. The official CLI shows the
//! account's sections at its root, so My Files is `/my-files`; AeroFTP's
//! native client is rooted in My Files, so the same folder is `/`. Both now
//! open in My Files ([`crate::providers::proton::START_DIR`]). A switch, from
//! the profile editor or from `profile-convert-mode`, translates every path
//! saved for the profile by adding or removing that exact prefix: the initial
//! folder, the AeroCloud folders bound to it, and the sync histories (index
//! and resumable journal) that record the profile. A path in another section
//! has no native equivalent and is reported, never dropped.
//!
//! Sync histories written before they recorded a profile cannot be told
//! apart from another server's with the same folders, and moving a baseline
//! by guess is how bisync would read files that were never there as deleted.
//! Those are never touched; the plan counts them.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cloud_config::CloudConfig;
use crate::cloud_pairs::CloudPairsConfig;

/// My Files as the CLI spells it, the prefix a switch adds or removes.
pub const MY_FILES: &str = crate::providers::proton::START_DIR;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProtonMode {
    /// AeroFTP's own client, rooted in My Files (`proton_mode: "native"`).
    Native,
    /// The official `proton-drive` CLI, rooted in the sections (no
    /// `proton_mode`, as the profile editor saves it).
    Cli,
}

impl ProtonMode {
    pub fn parse(label: &str) -> Option<Self> {
        match label.trim().to_ascii_lowercase().as_str() {
            "native" => Some(Self::Native),
            "cli" => Some(Self::Cli),
            _ => None,
        }
    }

    pub fn of_profile(profile: &Value) -> Self {
        let mode = profile
            .get("options")
            .and_then(|options| options.get("proton_mode"))
            .and_then(Value::as_str);
        if mode == Some("native") {
            Self::Native
        } else {
            Self::Cli
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Cli => "cli",
        }
    }
}

pub fn is_proton_profile(profile: &Value) -> bool {
    profile.get("protocol").and_then(Value::as_str) == Some("proton")
}

/// The folder a saved profile's relative paths resolve against when it saves
/// none: My Files for Proton Drive on the CLI, where a session now starts,
/// `/` for every other profile.
pub fn default_remote_base(profile: &Value) -> &'static str {
    if is_proton_profile(profile) && ProtonMode::of_profile(profile) == ProtonMode::Cli {
        MY_FILES
    } else {
        "/"
    }
}

/// The same folder under the other client, or `None` when it has none (the
/// CLI's root and its other sections). A relative or empty path is kept as it
/// is: both clients start in My Files, so it names the same folder either way.
/// A user folder called `my-files` stays a folder: only the leading prefix is
/// the section.
pub fn translate_path(path: &str, from: ProtonMode, to: ProtonMode) -> Option<String> {
    if from == to || !path.starts_with('/') {
        return Some(path.to_string());
    }
    match to {
        ProtonMode::Native => {
            let rest = path.strip_prefix(MY_FILES)?;
            if rest.is_empty() {
                Some("/".to_string())
            } else if rest.starts_with('/') {
                Some(rest.to_string())
            } else {
                // `/my-filesX` is not My Files.
                None
            }
        }
        ProtonMode::Cli => Some(if path == "/" {
            MY_FILES.to_string()
        } else {
            format!("{MY_FILES}{path}")
        }),
    }
}

/// One saved path a switch translates, or reports when it has no equivalent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PathChange {
    /// What holds it: `initialPath`, `aerocloud`, `aerocloudPair`,
    /// `syncHistory` or `pendingSync`.
    pub kind: &'static str,
    /// Which one, when there can be several: the pair's name, the local folder
    /// of a sync history. Empty for the initial folder.
    pub label: String,
    pub from: String,
    /// `None`: no equivalent in the target mode; the path is kept as it is.
    pub to: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwitchPlan {
    pub profile_id: String,
    pub from: ProtonMode,
    pub to: ProtonMode,
    pub changes: Vec<PathChange>,
    /// Sync histories that record no profile: never moved, and any of them
    /// that belongs to this profile compares without history next time, which
    /// copies files a deletion could have removed but deletes nothing.
    pub legacy_histories: usize,
    /// Why the switch cannot be made now. Empty when it can.
    pub blockers: Vec<String>,
}

impl SwitchPlan {
    pub fn translated(&self) -> impl Iterator<Item = &PathChange> {
        self.changes.iter().filter(|change| change.to.is_some())
    }

    pub fn without_equivalent(&self) -> impl Iterator<Item = &PathChange> {
        self.changes.iter().filter(|change| change.to.is_none())
    }
}

/// Where sync histories live: the sync index and the resumable journals.
pub(crate) struct HistoryDirs {
    pub index: PathBuf,
    pub journal: PathBuf,
}

impl HistoryDirs {
    pub(crate) fn on_disk() -> Result<Self, String> {
        Ok(Self {
            index: crate::sync::sync_index_dir()?,
            journal: crate::sync::sync_journal_dir()?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HistoryKind {
    Index,
    Journal,
}

/// One history file as read: its JSON stays whole, so a move keeps every
/// field this module does not know about.
#[derive(Debug, Clone)]
struct HistoryFile {
    kind: HistoryKind,
    path: PathBuf,
    raw: Value,
    bytes: Vec<u8>,
}

impl HistoryFile {
    fn text(&self, key: &str) -> &str {
        self.raw.get(key).and_then(Value::as_str).unwrap_or("")
    }

    fn profile_id(&self) -> Option<&str> {
        self.raw.get("profile_id").and_then(Value::as_str)
    }

    fn pending(&self) -> bool {
        self.kind == HistoryKind::Journal
            && !self
                .raw
                .get("completed")
                .and_then(Value::as_bool)
                .unwrap_or(false)
    }

    fn file_name(kind: HistoryKind, local: &str, remote: &str) -> String {
        match kind {
            HistoryKind::Index => crate::sync::index_filename(local, remote),
            HistoryKind::Journal => crate::sync::journal_filename(local, remote),
        }
    }
}

fn read_histories(dirs: &HistoryDirs) -> Vec<HistoryFile> {
    let mut out = Vec::new();
    for (kind, dir) in [
        (HistoryKind::Index, &dirs.index),
        (HistoryKind::Journal, &dirs.journal),
    ] {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let Ok(raw) = serde_json::from_slice::<Value>(&bytes) else {
                continue;
            };
            if raw.get("local_path").is_none() || raw.get("remote_path").is_none() {
                continue;
            }
            out.push(HistoryFile {
                kind,
                path,
                raw,
                bytes,
            });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

fn field<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

fn is_under(path: &str, folder: &str) -> bool {
    let folder = folder.trim_end_matches('/');
    if folder.is_empty() {
        return path.starts_with('/');
    }
    path == folder
        || path
            .strip_prefix(folder)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// What a switch of `profile` to `to` would change, and what stops it.
/// `aerocloud_busy` comes from the caller: the app knows whether its AeroCloud
/// service is syncing; the CLI cannot see the app and passes whether an
/// enabled AeroCloud sync uses the profile.
pub(crate) fn plan_in(
    dirs: &HistoryDirs,
    profile: &Value,
    to: ProtonMode,
    cloud: &CloudConfig,
    pairs: &CloudPairsConfig,
    aerocloud_busy: Option<String>,
) -> Result<SwitchPlan, String> {
    if !is_proton_profile(profile) {
        return Err("only a Proton Drive profile switches between native and CLI".to_string());
    }
    let id = field(profile, "id").to_string();
    let name = field(profile, "name");
    let from = ProtonMode::of_profile(profile);
    let mut plan = SwitchPlan {
        profile_id: id.clone(),
        from,
        to,
        changes: Vec::new(),
        legacy_histories: 0,
        blockers: Vec::new(),
    };
    if from == to {
        return Ok(plan);
    }

    // The paths saved for the profile, in the spelling of its current mode:
    // a legacy journal pending under one of them may be this profile's.
    let mut saved = Vec::new();
    let initial = field(profile, "initialPath");
    if !initial.is_empty() {
        plan.changes.push(PathChange {
            kind: "initialPath",
            label: String::new(),
            from: initial.to_string(),
            to: translate_path(initial, from, to),
        });
        if initial.starts_with('/') {
            saved.push(initial.to_string());
        }
    }
    if bound_config(cloud, &id, name) {
        let to_path = translate_path(&cloud.remote_folder, from, to);
        if to_path.is_none() {
            plan.blockers.push(format!(
                "AeroCloud syncs {} with this profile, a folder with no equivalent in {} mode: choose another folder or unbind the profile first",
                cloud.remote_folder,
                to.label()
            ));
        }
        plan.changes.push(PathChange {
            kind: "aerocloud",
            label: cloud.cloud_name.clone(),
            from: cloud.remote_folder.clone(),
            to: to_path,
        });
        saved.push(cloud.remote_folder.clone());
    }
    for pair in pairs
        .pairs
        .iter()
        .filter(|pair| !name.is_empty() && pair.server_profile == name)
    {
        let to_path = translate_path(&pair.remote_path, from, to);
        if to_path.is_none() {
            plan.blockers.push(format!(
                "the AeroCloud pair {} syncs {}, a folder with no equivalent in {} mode: change it first",
                pair.name,
                pair.remote_path,
                to.label()
            ));
        }
        plan.changes.push(PathChange {
            kind: "aerocloudPair",
            label: pair.name.clone(),
            from: pair.remote_path.clone(),
            to: to_path,
        });
        saved.push(pair.remote_path.clone());
    }
    if let Some(reason) = aerocloud_busy {
        plan.blockers.push(reason);
    }

    let keys = bound_index_keys(cloud, pairs, &id, name);
    let histories = read_histories(dirs);
    let moving: Vec<&HistoryFile> = histories
        .iter()
        .filter(|h| belongs(h, &id, &keys))
        .collect();
    let old_paths: HashSet<&Path> = moving.iter().map(|h| h.path.as_path()).collect();
    for history in &histories {
        if history.profile_id().is_none() && !belongs(history, &id, &keys) {
            plan.legacy_histories += 1;
            let remote = history.text("remote_path");
            if history.pending() && saved.iter().any(|folder| is_under(remote, folder)) {
                plan.blockers.push(format!(
                    "a sync of {} with {} is still pending: finish or discard it before switching mode",
                    history.text("local_path"),
                    remote
                ));
            }
        }
    }
    for history in moving {
        let local = history.text("local_path");
        let remote = history.text("remote_path");
        let to_path = translate_path(remote, from, to);
        if let Some(target) = &to_path {
            let new_path =
                history
                    .path
                    .with_file_name(HistoryFile::file_name(history.kind, local, target));
            if new_path.exists() && !old_paths.contains(new_path.as_path()) {
                plan.blockers.push(format!(
                    "a sync history of {local} with {target} already exists and records another profile or none: remove it from the sync history first"
                ));
            }
        }
        plan.changes.push(PathChange {
            kind: if history.pending() {
                "pendingSync"
            } else {
                "syncHistory"
            },
            label: local.to_string(),
            from: remote.to_string(),
            to: to_path,
        });
    }
    Ok(plan)
}

fn bound_config(cloud: &CloudConfig, id: &str, name: &str) -> bool {
    match cloud.server_profile_id.as_deref() {
        Some(bound) => bound == id,
        None => !name.is_empty() && cloud.server_profile == name,
    }
}

/// The index keys (local folder, remote folder) of the AeroCloud config and
/// pairs that sync with the profile. Their indexes belong to it even when they
/// record no profile: a pair names its profile, not the index.
fn bound_index_keys(
    cloud: &CloudConfig,
    pairs: &CloudPairsConfig,
    id: &str,
    name: &str,
) -> HashSet<(String, String)> {
    let mut keys = HashSet::new();
    if bound_config(cloud, id, name) {
        keys.insert((
            cloud.local_folder.to_string_lossy().into_owned(),
            cloud.remote_folder.clone(),
        ));
    }
    for pair in pairs
        .pairs
        .iter()
        .filter(|pair| !name.is_empty() && pair.server_profile == name)
    {
        keys.insert((
            pair.local_path.to_string_lossy().into_owned(),
            pair.remote_path.clone(),
        ));
    }
    keys
}

/// Whether a history moves with the profile: it records the profile, or it
/// is the index of an AeroCloud sync bound to it and records none.
fn belongs(history: &HistoryFile, id: &str, keys: &HashSet<(String, String)>) -> bool {
    match history.profile_id() {
        Some(recorded) => recorded == id,
        None => {
            history.kind == HistoryKind::Index
                && keys.contains(&(
                    history.text("local_path").to_string(),
                    history.text("remote_path").to_string(),
                ))
        }
    }
}

/// Where a switch writes besides the history files. The app and the CLI use
/// [`DiskTargets`]; tests replace it to fail a write half way.
pub(crate) trait SwitchTargets {
    fn store_cloud(&mut self, cloud: &CloudConfig, pairs: &CloudPairsConfig) -> Result<(), String>;
    /// Sets the mode and, when given, the initial folder on the stored
    /// profile, leaving every other field as the store holds it now.
    fn store_profile(
        &mut self,
        id: &str,
        to: ProtonMode,
        initial_path: Option<&str>,
    ) -> Result<(), String>;
}

/// Applies a switch: the plan is made again, refused when anything blocks it,
/// and written so that a failure leaves everything as it was. New history
/// files first, then AeroCloud, then the profile; the old history files go
/// last, once nothing can fail any more.
pub(crate) fn apply_in(
    dirs: &HistoryDirs,
    profile: &Value,
    to: ProtonMode,
    cloud: &CloudConfig,
    pairs: &CloudPairsConfig,
    aerocloud_busy: Option<String>,
    targets: &mut dyn SwitchTargets,
) -> Result<SwitchPlan, String> {
    let plan = plan_in(dirs, profile, to, cloud, pairs, aerocloud_busy)?;
    if !plan.blockers.is_empty() {
        return Err(plan.blockers.join("; "));
    }
    if plan.from == plan.to {
        return Ok(plan);
    }
    let id = plan.profile_id.clone();
    let from = plan.from;
    let keys = bound_index_keys(cloud, pairs, &id, field(profile, "name"));

    // Every history of the profile, with its bytes kept to put back.
    let histories: Vec<HistoryFile> = read_histories(dirs)
        .into_iter()
        .filter(|h| belongs(h, &id, &keys))
        .collect();
    let mut writes: Vec<(PathBuf, Vec<u8>)> = Vec::new();
    // The old files of the histories that move, with a journal's signature;
    // one without an equivalent stays where it is.
    let mut moved: Vec<PathBuf> = Vec::new();
    let mut originals: HashMap<PathBuf, Vec<u8>> = histories
        .iter()
        .map(|h| (h.path.clone(), h.bytes.clone()))
        .collect();
    for history in &histories {
        let local = history.text("local_path").to_string();
        let remote = history.text("remote_path");
        let Some(target) = translate_path(remote, from, to) else {
            continue;
        };
        let mut rewritten = history.raw.clone();
        rewritten["remote_path"] = Value::String(target.clone());
        let bytes = serde_json::to_vec(&rewritten).map_err(|e| e.to_string())?;
        let new_path =
            history
                .path
                .with_file_name(HistoryFile::file_name(history.kind, &local, &target));
        if history.kind == HistoryKind::Journal {
            // A journal is signed for its path pair: the signature moves too,
            // made again for the new pair when it held (see
            // `moved_journal_signature_in`).
            let dir = history.path.parent().unwrap_or(Path::new(""));
            let old_sig = dir.join(crate::sync::journal_sig_filename(&local, remote));
            if let Ok(old) = std::fs::read(&old_sig) {
                originals.insert(old_sig.clone(), old);
                moved.push(old_sig);
            }
            let signature = serde_json::from_value::<crate::sync::SyncJournal>(rewritten.clone())
                .ok()
                .and_then(|journal| crate::sync::moved_journal_signature_in(dir, &journal, remote));
            if let Some(signature) = signature {
                let new_sig = dir.join(crate::sync::journal_sig_filename(&local, &target));
                writes.push((new_sig, signature));
            }
        }
        writes.push((new_path, bytes));
        moved.push(history.path.clone());
    }
    let new_paths: HashSet<PathBuf> = writes.iter().map(|(path, _)| path.clone()).collect();

    let roll_back = |written: &[PathBuf]| {
        for path in written {
            match originals.get(path) {
                Some(bytes) => {
                    let _ = crate::sync::atomic_write(path, bytes);
                }
                None => {
                    let _ = std::fs::remove_file(path);
                }
            }
        }
    };

    let mut written = Vec::new();
    for (path, bytes) in &writes {
        if let Err(e) = crate::sync::atomic_write(path, bytes) {
            roll_back(&written);
            return Err(format!("sync history not moved, nothing was changed: {e}"));
        }
        written.push(path.clone());
    }

    let mut new_cloud = cloud.clone();
    let mut new_pairs = pairs.clone();
    let name = field(profile, "name").to_string();
    let mut cloud_changed = false;
    if bound_config(cloud, &id, &name) {
        if let Some(target) = translate_path(&cloud.remote_folder, from, to) {
            cloud_changed |= target != new_cloud.remote_folder;
            new_cloud.remote_folder = target;
        }
    }
    for pair in new_pairs
        .pairs
        .iter_mut()
        .filter(|pair| !name.is_empty() && pair.server_profile == name)
    {
        if let Some(target) = translate_path(&pair.remote_path, from, to) {
            cloud_changed |= target != pair.remote_path;
            pair.remote_path = target;
        }
    }
    if cloud_changed {
        if let Err(e) = targets.store_cloud(&new_cloud, &new_pairs) {
            roll_back(&written);
            return Err(format!("AeroCloud not updated, nothing was changed: {e}"));
        }
    }

    let initial = plan
        .changes
        .iter()
        .find(|change| change.kind == "initialPath")
        .and_then(|change| change.to.clone());
    if let Err(e) = targets.store_profile(&id, to, initial.as_deref()) {
        if cloud_changed {
            let _ = targets.store_cloud(cloud, pairs);
        }
        roll_back(&written);
        return Err(format!("profile not saved, nothing was changed: {e}"));
    }

    for old in &moved {
        if !new_paths.contains(old) {
            if let Err(e) = std::fs::remove_file(old) {
                tracing::warn!(
                    "proton mode switch: old sync history {} not removed: {e}",
                    old.display()
                );
            }
        }
    }
    Ok(plan)
}

/// The app's and the CLI's stores: AeroCloud's config files, and the vault's
/// profile list (the active account's partition, mirrored into the legacy
/// blob the way the CLI keeps it, or the blob alone before partitions).
pub struct DiskTargets<'a> {
    pub store: &'a crate::credential_store::CredentialStore,
    pub user_id: Option<i64>,
}

impl SwitchTargets for DiskTargets<'_> {
    fn store_cloud(&mut self, cloud: &CloudConfig, pairs: &CloudPairsConfig) -> Result<(), String> {
        let previous = crate::cloud_config::load_cloud_config();
        crate::cloud_config::save_cloud_config(cloud)?;
        if let Err(e) = crate::cloud_pairs::save_cloud_pairs_config(pairs) {
            let _ = crate::cloud_config::save_cloud_config(&previous);
            return Err(e);
        }
        Ok(())
    }

    fn store_profile(
        &mut self,
        id: &str,
        to: ProtonMode,
        initial_path: Option<&str>,
    ) -> Result<(), String> {
        let apply = |profiles: &mut Vec<Value>| {
            let Some(profile) = profiles.iter_mut().find(|p| field(p, "id") == id) else {
                return false;
            };
            set_mode(profile, to, initial_path);
            true
        };
        match self.user_id {
            Some(user) => {
                let (stored, written) =
                    crate::user_partitions::cli_update_server_profiles_for_user(
                        self.store, user, apply,
                    )?;
                if !written {
                    return Err("the profile was removed in the meantime".to_string());
                }
                let active = crate::user_partitions::cli_get_active_user(self.store)
                    .ok()
                    .flatten()
                    .map(|user| user.id);
                if active == Some(user) {
                    crate::user_partitions::mirror_active_profiles_to_legacy_blob(
                        self.store, &stored,
                    )?;
                }
                Ok(())
            }
            None => {
                let raw = self
                    .store
                    .get("config_server_profiles")
                    .map_err(|e| e.to_string())?;
                let mut profiles: Vec<Value> =
                    serde_json::from_str(&raw).map_err(|_| "invalid saved profiles".to_string())?;
                if !apply(&mut profiles) {
                    return Err("the profile was removed in the meantime".to_string());
                }
                self.store
                    .store(
                        "config_server_profiles",
                        &serde_json::to_string(&profiles).map_err(|e| e.to_string())?,
                    )
                    .map_err(|e| e.to_string())
            }
        }
    }
}

/// The mode as the profile editor saves it, and the translated initial folder.
pub fn set_mode(profile: &mut Value, to: ProtonMode, initial_path: Option<&str>) {
    if !profile["options"].is_object() {
        profile["options"] = serde_json::json!({});
    }
    if let Some(options) = profile["options"].as_object_mut() {
        match to {
            ProtonMode::Native => {
                options.insert("proton_mode".to_string(), Value::from("native"));
            }
            ProtonMode::Cli => {
                options.remove("proton_mode");
            }
        }
    }
    if let Some(path) = initial_path {
        profile["initialPath"] = Value::from(path);
    }
}

/// The plan for the profile stored under `id`, read from disk.
pub fn plan(
    profile: &Value,
    to: ProtonMode,
    aerocloud_busy: Option<String>,
) -> Result<SwitchPlan, String> {
    plan_in(
        &HistoryDirs::on_disk()?,
        profile,
        to,
        &crate::cloud_config::load_cloud_config(),
        &crate::cloud_pairs::load_cloud_pairs_config(),
        aerocloud_busy,
    )
}

/// Applies the switch on disk; see [`apply_in`].
pub fn apply(
    profile: &Value,
    to: ProtonMode,
    aerocloud_busy: Option<String>,
    targets: &mut DiskTargets<'_>,
) -> Result<SwitchPlan, String> {
    apply_in(
        &HistoryDirs::on_disk()?,
        profile,
        to,
        &crate::cloud_config::load_cloud_config(),
        &crate::cloud_pairs::load_cloud_pairs_config(),
        aerocloud_busy,
        targets,
    )
}

/// Why the CLI refuses a switch while AeroCloud uses the profile: the CLI
/// cannot see whether the app is running AeroCloud with the old folders.
pub fn cli_aerocloud_refusal(profile: &Value) -> Option<String> {
    let id = field(profile, "id");
    let name = field(profile, "name");
    let cloud = crate::cloud_config::load_cloud_config();
    if cloud.enabled && bound_config(&cloud, id, name) {
        return Some(format!(
            "AeroCloud syncs {} with this profile: run `aeroftp-cli aerocloud disable`, switch, then enable it again (or switch from the app, which updates AeroCloud itself)",
            cloud.remote_folder
        ));
    }
    let pairs = crate::cloud_pairs::load_cloud_pairs_config();
    pairs
        .pairs
        .iter()
        .find(|pair| pair.enabled && !name.is_empty() && pair.server_profile == name)
        .map(|pair| {
            format!(
                "the AeroCloud pair {} syncs {} with this profile: disable it, switch, then enable it again (or switch from the app)",
                pair.name, pair.remote_path
            )
        })
}

/// The vault the app has open, and the account whose profiles it shows.
fn app_store() -> Result<(crate::credential_store::CredentialStore, Option<i64>), String> {
    let store = crate::credential_store::CredentialStore::from_cache()
        .ok_or_else(|| "the vault is locked".to_string())?;
    let user_id = crate::user_partitions::cli_get_active_user(&store)
        .ok()
        .flatten()
        .map(|user| user.id);
    Ok((store, user_id))
}

/// The profile `id` as the vault holds it now.
fn stored_profile(
    store: &crate::credential_store::CredentialStore,
    user_id: Option<i64>,
    id: &str,
) -> Result<Value, String> {
    let profiles: Vec<Value> = match user_id {
        Some(user) => crate::user_partitions::cli_list_server_profiles_for_user(store, user)?,
        None => serde_json::from_str(
            &store
                .get("config_server_profiles")
                .map_err(|e| e.to_string())?,
        )
        .map_err(|_| "invalid saved profiles".to_string())?,
    };
    profiles
        .into_iter()
        .find(|profile| field(profile, "id") == id)
        .ok_or_else(|| "the profile is no longer saved".to_string())
}

const AEROCLOUD_SYNCING: &str = "AeroCloud is syncing: switch the mode when the sync has finished";

/// What a switch of the saved profile `profile_id` to `mode` would change, for
/// the editor's confirmation. Writes nothing.
#[tauri::command]
pub async fn proton_mode_switch_plan(
    profile_id: String,
    mode: String,
) -> Result<SwitchPlan, String> {
    tokio::task::spawn_blocking(move || {
        let to =
            ProtonMode::parse(&mode).ok_or_else(|| format!("unknown Proton Drive mode {mode}"))?;
        let (store, user_id) = app_store()?;
        let profile = stored_profile(&store, user_id, &profile_id)?;
        let busy = crate::CloudSyncGuard::acquire()
            .is_none()
            .then(|| AEROCLOUD_SYNCING.to_string());
        plan(&profile, to, busy)
    })
    .await
    .map_err(|e| format!("proton_mode_switch_plan task failed: {e}"))?
}

/// Switches the saved profile `profile_id` to `mode` and moves what is saved
/// for it, after the editor's confirmation. AeroCloud's sync lock is held for
/// the whole switch, so no cycle runs on the old folders; each cycle reads its
/// folders again, so the next one runs on the new ones.
#[tauri::command]
pub async fn proton_mode_switch_apply(
    profile_id: String,
    mode: String,
) -> Result<SwitchPlan, String> {
    tokio::task::spawn_blocking(move || {
        let to =
            ProtonMode::parse(&mode).ok_or_else(|| format!("unknown Proton Drive mode {mode}"))?;
        let (store, user_id) = app_store()?;
        let profile = stored_profile(&store, user_id, &profile_id)?;
        let guard = crate::CloudSyncGuard::acquire();
        let busy = guard.is_none().then(|| AEROCLOUD_SYNCING.to_string());
        let mut targets = DiskTargets {
            store: &store,
            user_id,
        };
        let applied = apply(&profile, to, busy, &mut targets);
        drop(guard);
        applied
    })
    .await
    .map_err(|e| format!("proton_mode_switch_apply task failed: {e}"))?
}

/// [`translate_path`] for the editor's Save as new, whose copy is not saved
/// yet. `None`: no equivalent in `to`.
#[tauri::command]
pub async fn proton_translate_path(
    path: String,
    from: String,
    to: String,
) -> Result<Option<String>, String> {
    let from =
        ProtonMode::parse(&from).ok_or_else(|| format!("unknown Proton Drive mode {from}"))?;
    let to = ProtonMode::parse(&to).ok_or_else(|| format!("unknown Proton Drive mode {to}"))?;
    Ok(translate_path(&path, from, to))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn translation_moves_only_the_my_files_prefix() {
        let cli = ProtonMode::Cli;
        let native = ProtonMode::Native;
        let to_native = |p: &str| translate_path(p, cli, native);
        let to_cli = |p: &str| translate_path(p, native, cli);
        assert_eq!(to_native("/my-files").as_deref(), Some("/"));
        assert_eq!(to_native("/my-files/").as_deref(), Some("/"));
        assert_eq!(to_native("/my-files/docs/a").as_deref(), Some("/docs/a"));
        assert_eq!(to_native("/my-files/docs/").as_deref(), Some("/docs/"));
        // A user folder called my-files inside My Files.
        assert_eq!(
            to_native("/my-files/my-files/a").as_deref(),
            Some("/my-files/a")
        );
        assert_eq!(
            to_cli("/my-files/a").as_deref(),
            Some("/my-files/my-files/a")
        );
        assert_eq!(to_cli("/").as_deref(), Some("/my-files"));
        assert_eq!(to_cli("/docs/a").as_deref(), Some("/my-files/docs/a"));
        // The CLI's root and its other sections have no native equivalent.
        for path in ["/", "/shared-with-me/x", "/devices", "/my-filesX"] {
            assert_eq!(to_native(path), None, "{path}");
        }
        // Relative and empty paths name the same folder in both modes.
        assert_eq!(to_native("docs").as_deref(), Some("docs"));
        assert_eq!(to_cli("").as_deref(), Some(""));
        // A round trip is the identity on everything that has an equivalent.
        for path in ["/", "/docs", "/my-files/a"] {
            assert_eq!(
                to_cli(path).and_then(|p| to_native(&p)).as_deref(),
                Some(path)
            );
        }
    }

    #[test]
    fn the_default_base_is_my_files_only_for_proton_on_the_cli() {
        assert_eq!(
            default_remote_base(&json!({"protocol": "proton"})),
            MY_FILES
        );
        assert_eq!(
            default_remote_base(
                &json!({"protocol": "proton", "options": {"proton_mode": "native"}})
            ),
            "/"
        );
        assert_eq!(default_remote_base(&json!({"protocol": "sftp"})), "/");
    }

    struct Fixture {
        _root: tempfile::TempDir,
        dirs: HistoryDirs,
    }

    fn fixture() -> Fixture {
        let root = tempfile::tempdir().unwrap();
        let dirs = HistoryDirs {
            index: root.path().join("sync-index"),
            journal: root.path().join("sync-journal"),
        };
        std::fs::create_dir_all(&dirs.index).unwrap();
        std::fs::create_dir_all(&dirs.journal).unwrap();
        Fixture { _root: root, dirs }
    }

    fn write_index(
        dirs: &HistoryDirs,
        local: &str,
        remote: &str,
        profile: Option<&str>,
        marker: &str,
    ) -> PathBuf {
        let mut index = json!({
            "version": 3,
            "last_sync": "2026-10-23T10:00:00Z",
            "local_path": local,
            "remote_path": remote,
            "files": {marker: {"size": 1, "modified": null, "is_dir": false}},
        });
        if let Some(id) = profile {
            index["profile_id"] = json!(id);
        }
        let path = dirs.index.join(crate::sync::index_filename(local, remote));
        std::fs::write(&path, serde_json::to_vec(&index).unwrap()).unwrap();
        path
    }

    fn write_journal(
        dirs: &HistoryDirs,
        local: &str,
        remote: &str,
        profile: Option<&str>,
        completed: bool,
    ) -> PathBuf {
        let mut journal = json!({
            "id": "j1",
            "local_path": local,
            "remote_path": remote,
            "entries": [],
            "completed": completed,
        });
        if let Some(id) = profile {
            journal["profile_id"] = json!(id);
        }
        let path = dirs
            .journal
            .join(crate::sync::journal_filename(local, remote));
        std::fs::write(&path, serde_json::to_vec(&journal).unwrap()).unwrap();
        path
    }

    fn profile(mode: ProtonMode, initial: &str) -> Value {
        let mut p =
            json!({"id": "p1", "name": "Proton", "protocol": "proton", "initialPath": initial});
        set_mode(&mut p, mode, None);
        p
    }

    #[derive(Default)]
    struct Memory {
        cloud: Option<(CloudConfig, CloudPairsConfig)>,
        profile: Option<(String, ProtonMode, Option<String>)>,
        fail_profile: bool,
    }

    impl SwitchTargets for Memory {
        fn store_cloud(
            &mut self,
            cloud: &CloudConfig,
            pairs: &CloudPairsConfig,
        ) -> Result<(), String> {
            self.cloud = Some((cloud.clone(), pairs.clone()));
            Ok(())
        }
        fn store_profile(
            &mut self,
            id: &str,
            to: ProtonMode,
            initial: Option<&str>,
        ) -> Result<(), String> {
            if self.fail_profile {
                return Err("vault locked".to_string());
            }
            self.profile = Some((id.to_string(), to, initial.map(str::to_string)));
            Ok(())
        }
    }

    fn files_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn read(path: &Path) -> Value {
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    #[test]
    fn a_switch_moves_the_profiles_histories_and_leaves_legacy_ones_counted() {
        let f = fixture();
        let ours = write_index(
            &f.dirs,
            "/home/u/docs",
            "/my-files/docs",
            Some("p1"),
            "a.txt",
        );
        let journal = write_journal(&f.dirs, "/home/u/docs", "/my-files/docs", Some("p1"), false);
        let legacy = write_index(&f.dirs, "/home/u/old", "/my-files/old", None, "b.txt");
        let other = write_index(&f.dirs, "/home/u/x", "/my-files/x", Some("p2"), "c.txt");
        let mut memory = Memory::default();
        let plan = apply_in(
            &f.dirs,
            &profile(ProtonMode::Cli, "/my-files/docs"),
            ProtonMode::Native,
            &CloudConfig::default(),
            &CloudPairsConfig::default(),
            None,
            &mut memory,
        )
        .unwrap();
        assert_eq!(plan.legacy_histories, 1);
        assert_eq!(
            memory.profile,
            Some((
                "p1".to_string(),
                ProtonMode::Native,
                Some("/docs".to_string())
            ))
        );
        assert!(!ours.exists() && !journal.exists());
        let moved = f
            .dirs
            .index
            .join(crate::sync::index_filename("/home/u/docs", "/docs"));
        assert_eq!(read(&moved)["remote_path"], "/docs");
        assert_eq!(read(&moved)["files"]["a.txt"]["size"], 1);
        assert!(f
            .dirs
            .journal
            .join(crate::sync::journal_filename("/home/u/docs", "/docs"))
            .exists());
        // Never touched: a history that records no profile, and another profile's.
        assert_eq!(read(&legacy)["remote_path"], "/my-files/old");
        assert_eq!(read(&other)["remote_path"], "/my-files/x");
    }

    /// A journal is signed for its path pair, and Verify reads the signature
    /// of the pair. A moved journal is signed again for its new pair when its
    /// signature held, keeps its old bytes (still a mismatch) when it did
    /// not, and stays unsigned when it was; no old signature is left behind.
    #[test]
    fn a_moved_journal_keeps_what_verify_says_of_it() {
        let f = fixture();
        let dir = f.dirs.journal.as_path();
        let save = |local: &str| {
            let mut journal = crate::sync::SyncJournal::new(
                local.to_string(),
                format!("/my-files{}", &local["/home/u".len()..]),
                crate::sync::CompareDirection::Bidirectional,
                crate::sync::RetryPolicy::default(),
                crate::sync::VerifyPolicy::default(),
            );
            journal.profile_id = Some("p1".to_string());
            journal.completed = true;
            crate::sync::save_sync_journal_in(dir, &journal).unwrap();
        };
        let sig =
            |local: &str, remote: &str| dir.join(crate::sync::journal_sig_filename(local, remote));
        let verify = |local: &str, remote: &str| {
            let key = crate::sync::journal_signing_key_in(dir, local, remote).unwrap();
            crate::sync::verify_journal_signature_in(dir, local, remote, &key)
        };
        save("/home/u/a");
        save("/home/u/b");
        save("/home/u/c");
        // b is edited on disk after its last save, c was never signed.
        let edited = dir.join(crate::sync::journal_filename("/home/u/b", "/my-files/b"));
        let mut raw = read(&edited);
        raw["id"] = json!("edited");
        std::fs::write(&edited, serde_json::to_vec(&raw).unwrap()).unwrap();
        std::fs::remove_file(sig("/home/u/c", "/my-files/c")).unwrap();
        assert_eq!(verify("/home/u/a", "/my-files/a"), Ok(true));
        assert_eq!(verify("/home/u/b", "/my-files/b"), Ok(false));

        apply_in(
            &f.dirs,
            &profile(ProtonMode::Cli, "/my-files/a"),
            ProtonMode::Native,
            &CloudConfig::default(),
            &CloudPairsConfig::default(),
            None,
            &mut Memory::default(),
        )
        .unwrap();

        assert_eq!(verify("/home/u/a", "/a"), Ok(true));
        assert_eq!(verify("/home/u/b", "/b"), Ok(false));
        assert!(verify("/home/u/c", "/c").is_err(), "still no signature");
        for local in ["/home/u/a", "/home/u/b", "/home/u/c"] {
            let old = format!("/my-files{}", &local["/home/u".len()..]);
            assert!(!sig(local, &old).exists(), "old signature of {local} left");
        }
    }

    /// A history of the profile in a section with no native equivalent is
    /// reported and stays where it is: nothing of it is removed.
    #[test]
    fn a_history_without_equivalent_stays_where_it_is() {
        let f = fixture();
        let shared = write_index(
            &f.dirs,
            "/home/u/team",
            "/shared-with-me/team",
            Some("p1"),
            "t.txt",
        );
        let mut memory = Memory::default();
        let plan = apply_in(
            &f.dirs,
            &profile(ProtonMode::Cli, ""),
            ProtonMode::Native,
            &CloudConfig::default(),
            &CloudPairsConfig::default(),
            None,
            &mut memory,
        )
        .unwrap();
        assert!(plan
            .without_equivalent()
            .any(|c| c.from == "/shared-with-me/team"));
        assert_eq!(read(&shared)["remote_path"], "/shared-with-me/team");
        assert_eq!(files_in(&f.dirs.index).len(), 1);
    }

    #[test]
    fn a_failed_profile_write_leaves_every_history_where_it_was() {
        let f = fixture();
        write_index(
            &f.dirs,
            "/home/u/docs",
            "/my-files/docs",
            Some("p1"),
            "a.txt",
        );
        write_journal(&f.dirs, "/home/u/docs", "/my-files/docs", Some("p1"), true);
        let index_before = files_in(&f.dirs.index);
        let journal_before = files_in(&f.dirs.journal);
        let mut memory = Memory {
            fail_profile: true,
            ..Memory::default()
        };
        let err = apply_in(
            &f.dirs,
            &profile(ProtonMode::Cli, ""),
            ProtonMode::Native,
            &CloudConfig::default(),
            &CloudPairsConfig::default(),
            None,
            &mut memory,
        )
        .unwrap_err();
        assert!(err.contains("nothing was changed"), "{err}");
        assert_eq!(files_in(&f.dirs.index), index_before);
        assert_eq!(files_in(&f.dirs.journal), journal_before);
        assert!(memory.profile.is_none());
    }

    #[test]
    fn a_legacy_pending_sync_under_a_saved_folder_blocks_the_switch() {
        let f = fixture();
        write_journal(&f.dirs, "/home/u/docs", "/my-files/docs/sub", None, false);
        let mut memory = Memory::default();
        let err = apply_in(
            &f.dirs,
            &profile(ProtonMode::Cli, "/my-files/docs"),
            ProtonMode::Native,
            &CloudConfig::default(),
            &CloudPairsConfig::default(),
            None,
            &mut memory,
        )
        .unwrap_err();
        assert!(err.contains("still pending"), "{err}");
        assert!(memory.profile.is_none());
        // A completed legacy journal, or one elsewhere, does not block.
        let g = fixture();
        write_journal(&g.dirs, "/home/u/docs", "/my-files/docs", None, true);
        write_journal(&g.dirs, "/home/u/x", "/my-files/x", None, false);
        let plan = plan_in(
            &g.dirs,
            &profile(ProtonMode::Cli, "/my-files/docs"),
            ProtonMode::Native,
            &CloudConfig::default(),
            &CloudPairsConfig::default(),
            None,
        )
        .unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(plan.legacy_histories, 2);
    }

    /// A native profile with a user folder `my-files`: its history of
    /// `/my-files/x` moves to `/my-files/my-files/x` while the history of `/x`
    /// moves onto the old name of the first. Both arrive, neither is lost.
    #[test]
    fn histories_that_trade_names_both_arrive() {
        let f = fixture();
        write_index(
            &f.dirs,
            "/home/u/d",
            "/my-files/x",
            Some("p1"),
            "nested.txt",
        );
        write_index(&f.dirs, "/home/u/d", "/x", Some("p1"), "top.txt");
        let mut memory = Memory::default();
        apply_in(
            &f.dirs,
            &profile(ProtonMode::Native, ""),
            ProtonMode::Cli,
            &CloudConfig::default(),
            &CloudPairsConfig::default(),
            None,
            &mut memory,
        )
        .unwrap();
        let top = read(
            &f.dirs
                .index
                .join(crate::sync::index_filename("/home/u/d", "/my-files/x")),
        );
        let nested = read(&f.dirs.index.join(crate::sync::index_filename(
            "/home/u/d",
            "/my-files/my-files/x",
        )));
        assert!(top["files"].get("top.txt").is_some());
        assert!(nested["files"].get("nested.txt").is_some());
        assert_eq!(files_in(&f.dirs.index).len(), 2);
    }

    /// Native, then the CLI, a sync on the CLI, then native again: the
    /// baseline back on native is the one the CLI sync wrote, not the one the
    /// profile had before.
    #[test]
    fn a_round_trip_brings_back_the_fresh_baseline() {
        let f = fixture();
        write_index(&f.dirs, "/home/u/d", "/docs", Some("p1"), "old.txt");
        let mut memory = Memory::default();
        apply_in(
            &f.dirs,
            &profile(ProtonMode::Native, "/docs"),
            ProtonMode::Cli,
            &CloudConfig::default(),
            &CloudPairsConfig::default(),
            None,
            &mut memory,
        )
        .unwrap();
        // The sync on the CLI rewrites the baseline under the CLI spelling.
        write_index(
            &f.dirs,
            "/home/u/d",
            "/my-files/docs",
            Some("p1"),
            "fresh.txt",
        );
        apply_in(
            &f.dirs,
            &profile(ProtonMode::Cli, "/my-files/docs"),
            ProtonMode::Native,
            &CloudConfig::default(),
            &CloudPairsConfig::default(),
            None,
            &mut memory,
        )
        .unwrap();
        let back = read(
            &f.dirs
                .index
                .join(crate::sync::index_filename("/home/u/d", "/docs")),
        );
        assert!(back["files"].get("fresh.txt").is_some());
        assert!(back["files"].get("old.txt").is_none());
        assert_eq!(files_in(&f.dirs.index).len(), 1);
    }

    #[test]
    fn aerocloud_folders_follow_the_profile_and_a_folder_without_equivalent_blocks() {
        let f = fixture();
        let cloud = CloudConfig {
            remote_folder: "/my-files/cloud".to_string(),
            server_profile: "Proton".to_string(),
            server_profile_id: Some("p1".to_string()),
            ..CloudConfig::default()
        };
        let mut pairs = CloudPairsConfig::default();
        pairs.pairs.push(crate::cloud_pairs::new_cloud_path_pair(
            "pair-1".to_string(),
            "Photos".to_string(),
            std::path::PathBuf::from("/home/u/photos"),
            "/my-files/photos".to_string(),
            "Proton".to_string(),
        ));
        // The pair's index records no profile: it moves by its key.
        write_index(
            &f.dirs,
            "/home/u/photos",
            "/my-files/photos",
            None,
            "pic.jpg",
        );
        let mut memory = Memory::default();
        let plan = apply_in(
            &f.dirs,
            &profile(ProtonMode::Cli, ""),
            ProtonMode::Native,
            &cloud,
            &pairs,
            None,
            &mut memory,
        )
        .unwrap();
        assert_eq!(plan.legacy_histories, 0);
        let (stored_cloud, stored_pairs) = memory.cloud.unwrap();
        assert_eq!(stored_cloud.remote_folder, "/cloud");
        assert_eq!(stored_pairs.pairs[0].remote_path, "/photos");
        assert!(f
            .dirs
            .index
            .join(crate::sync::index_filename("/home/u/photos", "/photos"))
            .exists());
        assert_eq!(files_in(&f.dirs.index).len(), 1);

        let shared = CloudConfig {
            remote_folder: "/shared-with-me/team".to_string(),
            ..cloud
        };
        let plan = plan_in(
            &f.dirs,
            &profile(ProtonMode::Cli, ""),
            ProtonMode::Native,
            &shared,
            &CloudPairsConfig::default(),
            None,
        )
        .unwrap();
        assert!(plan.blockers.iter().any(|b| b.contains("no equivalent")));
        let busy = plan_in(
            &f.dirs,
            &profile(ProtonMode::Cli, ""),
            ProtonMode::Native,
            &CloudConfig::default(),
            &CloudPairsConfig::default(),
            Some("AeroCloud is syncing".to_string()),
        )
        .unwrap();
        assert_eq!(busy.blockers, vec!["AeroCloud is syncing".to_string()]);
    }
}
