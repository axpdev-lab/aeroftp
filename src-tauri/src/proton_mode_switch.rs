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

/// The folder of an AeroCloud `entry` (the settings when `None`, else the
/// pair with that id) as a switch to `to` writes it: as it is when an earlier
/// failed switch to `to` wrote that entry with that folder (`held`),
/// translated otherwise. Translated again, `/my-files/docs` would become
/// `/my-files/my-files/docs`. The entry is known by its identity, never by
/// its folder: another entry's folder not yet translated can read the same.
fn held_or_translated(
    path: &str,
    entry: Option<&str>,
    from: ProtonMode,
    to: ProtonMode,
    held: &[HeldFolder],
) -> Option<String> {
    if held
        .iter()
        .any(|left| left.pair.as_deref() == entry && left.folder == path)
    {
        Some(path.to_string())
    } else {
        translate_path(path, from, to)
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
#[cfg(test)]
pub(crate) fn plan_in(
    dirs: &HistoryDirs,
    profile: &Value,
    to: ProtonMode,
    cloud: &CloudConfig,
    pairs: &CloudPairsConfig,
    aerocloud_busy: Option<String>,
) -> Result<SwitchPlan, String> {
    plan_with_held(dirs, profile, to, cloud, pairs, aerocloud_busy, &[])
}

/// [`plan_in`] with `held`, the AeroCloud entries an earlier failed switch
/// to `to` wrote: those still on the folder it wrote stay as they are.
fn plan_with_held(
    dirs: &HistoryDirs,
    profile: &Value,
    to: ProtonMode,
    cloud: &CloudConfig,
    pairs: &CloudPairsConfig,
    aerocloud_busy: Option<String>,
    held: &[HeldFolder],
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
        let to_path = held_or_translated(&cloud.remote_folder, None, from, to, held);
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
        let to_path = held_or_translated(&pair.remote_path, Some(&pair.id), from, to, held);
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
    // Each new history file (and journal signature) with the old folder it
    // comes from: two histories whose folders translate to one, such as
    // `/my-files` and `/my-files/`, would write the same file and the later
    // one would replace the other's baseline.
    let mut targets: HashMap<PathBuf, &str> = HashMap::new();
    for history in moving {
        let local = history.text("local_path");
        let remote = history.text("remote_path");
        if history.pending() {
            // The run that wrote it may still be going, and it saves the
            // journal under the old folder while the switch moves it.
            plan.blockers.push(format!(
                "a sync of {local} with {remote} is still pending: finish or discard it before switching mode"
            ));
        }
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
            let mut new_files = vec![new_path];
            if history.kind == HistoryKind::Journal {
                let dir = history.path.parent().unwrap_or(Path::new(""));
                new_files.push(dir.join(crate::sync::journal_sig_filename(local, target)));
            }
            for new_file in new_files {
                if let Some(other) = targets.get(&new_file) {
                    let blocker = format!(
                        "the sync histories of {local} with {other} and with {remote} would both become the history of {target} in {} mode: remove one of them from the sync history first",
                        to.label()
                    );
                    if !plan.blockers.contains(&blocker) {
                        plan.blockers.push(blocker);
                    }
                    break;
                }
                targets.insert(new_file, remote);
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
    /// Writes `cloud` and `pairs`, edited from `read_as` (the files as the
    /// switch read them), over the files as they are at the write, for the
    /// switch of `profile` (its id and name).
    fn store_cloud(
        &mut self,
        profile: (&str, &str),
        read_as: (&CloudConfig, &CloudPairsConfig),
        cloud: &CloudConfig,
        pairs: &CloudPairsConfig,
    ) -> Result<(), CloudStoreError>;
    /// Puts back `cloud` and `pairs`, the files before a switch that wrote
    /// `written`, over the files as they are now: the settings by
    /// [`put_back_settings_folder`], the pairs by [`put_back_pair_folders`],
    /// so an edit or a pair made since the switch wrote is kept.
    fn put_back_cloud(
        &mut self,
        profile: (&str, &str),
        written: (&CloudConfig, &CloudPairsConfig),
        cloud: &CloudConfig,
        pairs: &CloudPairsConfig,
    ) -> Result<(), CloudStoreError>;
    /// Sets the mode and, when given, the initial folder on the stored
    /// profile, leaving every other field as the store holds it now.
    fn store_profile(
        &mut self,
        id: &str,
        to: ProtonMode,
        initial_path: Option<&str>,
    ) -> Result<(), ProfileStoreError>;
    /// Holds the syncs of the folders a switch changes, before it changes
    /// them, until it is done or they are repaired: see
    /// [`refuse_an_unrepaired_folder`].
    fn hold_syncs(&mut self, left: &UnrepairedSwitch) -> Result<(), String>;
    /// Releases the hold of the profile `profile_id`.
    fn release_syncs(&mut self, profile_id: &str) -> Result<(), String>;
    /// The hold of the profile `profile_id`, if any.
    fn held_syncs(&mut self, profile_id: &str) -> Result<Option<UnrepairedSwitch>, String>;
}

/// Puts back over `stored` the settings folder the switch changed from
/// `original` to `written`, and only that, while it still holds the folder
/// the switch wrote for the same target: every other field, and a folder or
/// a target changed since, stay as stored. True when the folder the switch
/// wrote is kept on settings still bound to the switch's `profile` (its id
/// and name) by the rules the switch finds them with ([`bound_config`]):
/// another local folder, protocol or connection set since, or the binding
/// written by name only. The put back is then incomplete.
#[must_use]
fn put_back_settings_folder(
    stored: &mut CloudConfig,
    written: &CloudConfig,
    original: &CloudConfig,
    profile: (&str, &str),
) -> bool {
    let target = |config: &CloudConfig| {
        (
            config.server_profile_id.clone(),
            config.server_profile.clone(),
            config.local_folder.clone(),
            config.protocol_type.clone(),
            config.connection_params.clone(),
        )
    };
    if written.remote_folder == original.remote_folder
        || stored.remote_folder != written.remote_folder
    {
        return false;
    }
    if target(stored) == target(written) {
        stored.remote_folder = original.remote_folder.clone();
        return false;
    }
    bound_config(stored, profile.0, profile.1)
}

/// Why a settings put back kept the folder the switch wrote.
const SETTINGS_KEPT_THE_FOLDER: &str = "the AeroCloud settings changed since the switch wrote them (another local folder, protocol or connection) and keep its folder";

/// The settings put back of a switch whose pairs write was refused with
/// `refused`: [`put_back_settings_folder`], and the error that says what
/// it left.
fn settings_back_after_refused_pairs(
    stored: &mut CloudConfig,
    written: &CloudConfig,
    original: &CloudConfig,
    profile: (&str, &str),
    refused: String,
) -> CloudStoreError {
    if put_back_settings_folder(stored, written, original, profile) {
        CloudStoreError::SettingsOnly(format!("{refused}; {SETTINGS_KEPT_THE_FOLDER}"))
    } else {
        CloudStoreError::NotSaved(refused)
    }
}

/// The AeroCloud folders a failed switch left spelled for `to` while the
/// profile stays in the other mode. No sync runs on them until the profile
/// is in `to` mode or the folder is set again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct UnrepairedSwitch {
    pub profile_id: String,
    pub profile_name: String,
    pub to: ProtonMode,
    /// The settings folder and the pair folders as the switch wrote them.
    pub folders: Vec<HeldFolder>,
}

/// A folder a switch wrote, with its entry: the settings when `pair` is
/// `None`, else the pair with that id. A retry knows what is already
/// spelled for its mode by the entry, never by the folder text alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HeldFolder {
    pub pair: Option<String>,
    pub folder: String,
}

/// Where the held switches are kept, next to the AeroCloud settings: a
/// restart, the app or the CLI, holds them the same.
pub(crate) fn unrepaired_path() -> PathBuf {
    crate::cloud_config::get_config_path().with_file_name("proton_switch_unrepaired.json")
}

static UNREPAIRED_WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Reads, edits and writes the held switches under the lock of their file;
/// an empty list removes the file. A file that cannot be read is an error,
/// so a sync is never let through on a record it could not read.
fn with_unrepaired_at<T>(
    path: &Path,
    edit: impl FnOnce(&mut Vec<UnrepairedSwitch>) -> T,
) -> Result<T, String> {
    let _lock = crate::cloud_config::SettingsFileLock::acquire_with(&UNREPAIRED_WRITE_LOCK, path)?;
    let mut held: Vec<UnrepairedSwitch> = match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| format!("the Proton Drive switch record is unreadable: {e}"))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(format!("the Proton Drive switch record is unreadable: {e}")),
    };
    let before = held.clone();
    let out = edit(&mut held);
    if held != before {
        let written = if held.is_empty() {
            std::fs::remove_file(path).map_err(|e| e.to_string())
        } else {
            serde_json::to_vec_pretty(&held)
                .map_err(|e| e.to_string())
                .and_then(|bytes| crate::sync::atomic_write(path, &bytes))
        };
        // The cause names the file's path: logged, never put in a message.
        written.map_err(|e| {
            tracing::warn!("Proton Drive switch record not written: {e}");
            "the Proton Drive switch record could not be written".to_string()
        })?;
    }
    Ok(out)
}

/// The hold of the profile `profile_id` on disk, if any.
fn read_held_syncs(profile_id: &str) -> Result<Option<UnrepairedSwitch>, String> {
    let path = unrepaired_path();
    if !path.exists() {
        return Ok(None);
    }
    with_unrepaired_at(&path, |held| {
        held.iter()
            .find(|left| left.profile_id == profile_id)
            .cloned()
    })
}

/// `held` when it is an earlier switch to `to` not repaired yet; one to the
/// other mode has nothing to do with this switch (the profile is in its
/// mode, which drops it at the next sync).
fn earlier_hold(held: &Option<UnrepairedSwitch>, to: ProtonMode) -> Option<UnrepairedSwitch> {
    held.clone().filter(|left| left.to == to)
}

/// Drops from `held` the switches whose profile is now in their mode
/// (`mode_of` answers the saved profile's mode), and answers why `config`
/// may not sync: its folder is one a held switch left behind.
fn unrepaired_refusal(
    held: &mut Vec<UnrepairedSwitch>,
    config: &CloudConfig,
    mode_of: impl Fn(&str) -> Option<ProtonMode>,
) -> Option<String> {
    held.retain(|left| mode_of(&left.profile_id) != Some(left.to));
    held.iter()
        .find(|left| {
            bound_config(config, &left.profile_id, &left.profile_name)
                && left
                    .folders
                    .iter()
                    .any(|held| held.folder == config.remote_folder)
        })
        .map(|left| {
            let from = match left.to {
                ProtonMode::Native => ProtonMode::Cli,
                ProtonMode::Cli => ProtonMode::Native,
            };
            format!(
                "AeroCloud sync held: a Proton Drive mode switch of {} could not put this folder back, which {} mode spells while the profile stays in {} mode; switch the profile to {} mode or set the AeroCloud folder again",
                left.profile_name,
                left.to.label(),
                from.label(),
                left.to.label()
            )
        })
}

/// Refuses a sync of `config` while its folder is one a failed switch left
/// spelled for the other mode, and forgets the switches already repaired.
/// Every AeroCloud connection asks it before connecting.
pub(crate) fn refuse_an_unrepaired_folder(
    config: &CloudConfig,
    store: &crate::credential_store::CredentialStore,
) -> Result<(), String> {
    let path = unrepaired_path();
    if !path.exists() {
        return Ok(());
    }
    // The profiles the connection selects from; a list that cannot be read
    // repairs nothing.
    let profiles =
        crate::user_partitions::mcp_list_active_server_profiles(store).unwrap_or_default();
    let mode_of = |id: &str| {
        profiles
            .iter()
            .find(|profile| field(profile, "id") == id)
            .map(ProtonMode::of_profile)
    };
    match with_unrepaired_at(&path, |held| unrepaired_refusal(held, config, mode_of))? {
        Some(refusal) => Err(refusal),
        None => Ok(()),
    }
}

/// Puts back over `stored` the remote folder of each pair the switch changed
/// from `original` to `written`, and only that: a pair added since, a
/// folder changed since and every other field stay as stored.
fn put_back_pair_folders(
    stored: &mut CloudPairsConfig,
    written: &CloudPairsConfig,
    original: &CloudPairsConfig,
) {
    for pair in &mut stored.pairs {
        let (Some(wrote), Some(was)) = (
            written.pairs.iter().find(|p| p.id == pair.id),
            original.pairs.iter().find(|p| p.id == pair.id),
        ) else {
            continue;
        };
        if wrote.remote_path != was.remote_path && pair.remote_path == wrote.remote_path {
            pair.remote_path = was.remote_path.clone();
        }
    }
}

/// [`SwitchTargets::store_cloud`] on the AeroCloud files.
fn store_cloud_files(
    profile: (&str, &str),
    read_as: (&CloudConfig, &CloudPairsConfig),
    cloud: &CloudConfig,
    pairs: &CloudPairsConfig,
) -> Result<(), CloudStoreError> {
    // Each file is merged with the file as it is at the write, under its
    // write lock, by the rule of the settings and the pair editor: what
    // it gained since the switch read it and the switch did not change
    // (a folder a sync placed meanwhile) stays as stored. A file that
    // cannot be read is not written at all, so the rollback can never
    // save a default over the user's file.
    crate::cloud_config::with_cloud_config_mut(|stored| {
        *stored = crate::cloud_config::settings_save(stored, read_as.0, cloud.clone())?;
        Ok(())
    })
    .map_err(CloudStoreError::NotSaved)?;
    if let Err(e) = crate::cloud_pairs::with_cloud_pairs_mut(|stored| {
        *stored = crate::cloud_pairs::pairs_save(stored, read_as.1, pairs.clone())?;
        Ok(())
    }) {
        // The settings folder goes back while it is the one this write
        // made: what changed in them since is kept, and a put back that
        // cannot be made is said, never taken for one that was.
        return Err(
            match crate::cloud_config::with_cloud_config_mut(|stored| {
                Ok(settings_back_after_refused_pairs(
                    stored,
                    cloud,
                    read_as.0,
                    profile,
                    e.clone(),
                ))
            }) {
                Ok(left) => left,
                Err(back) => CloudStoreError::SettingsOnly(format!(
                    "{e}; the settings could not be put back: {back}"
                )),
            },
        );
    }
    Ok(())
}

/// [`SwitchTargets::put_back_cloud`] on the AeroCloud files. The pairs go
/// first: a failure there leaves both files as the switch wrote them, which
/// name the same folders.
fn put_back_cloud_files(
    profile: (&str, &str),
    written: (&CloudConfig, &CloudPairsConfig),
    cloud: &CloudConfig,
    pairs: &CloudPairsConfig,
) -> Result<(), CloudStoreError> {
    crate::cloud_pairs::with_cloud_pairs_mut(|stored| {
        put_back_pair_folders(stored, written.1, pairs);
        Ok(())
    })
    .map_err(CloudStoreError::NotSaved)?;
    let kept = crate::cloud_config::with_cloud_config_mut(|stored| {
        Ok(put_back_settings_folder(stored, written.0, cloud, profile))
    })
    .map_err(|e| {
        CloudStoreError::SettingsOnly(format!(
            "the pairs were put back and the settings were not: {e}"
        ))
    })?;
    if kept {
        return Err(CloudStoreError::SettingsOnly(
            SETTINGS_KEPT_THE_FOLDER.to_string(),
        ));
    }
    Ok(())
}

/// Why [`SwitchTargets::store_cloud`] did not finish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CloudStoreError {
    /// Neither file was changed: nothing was written, or what was written
    /// has been put back.
    NotSaved(String),
    /// The settings were written and the pairs were not, and the settings
    /// could not be put back: the two files name different folders.
    SettingsOnly(String),
}

impl std::fmt::Display for CloudStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotSaved(e) | Self::SettingsOnly(e) => f.write_str(e),
        }
    }
}

/// What a failed switch left behind, said after its error: "nothing was
/// changed" only when every write was put back.
fn left_behind(history_not_put_back: usize, cloud: Option<String>) -> String {
    let mut left = Vec::new();
    if let Some(cloud) = cloud {
        left.push(cloud);
    }
    if history_not_put_back > 0 {
        left.push(format!(
            "{history_not_put_back} sync history file(s) could not be put back"
        ));
    }
    if left.is_empty() {
        "nothing was changed".to_string()
    } else {
        format!("NOT everything was put back: {}", left.join("; "))
    }
}

/// Why [`SwitchTargets::store_profile`] did not finish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProfileStoreError {
    /// The stored profile is as it was: nothing was written, or what was
    /// written has been put back.
    NotSaved(String),
    /// The profile was saved in the new mode and could not be put back.
    Switched(String),
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
    // An earlier failed switch to `to` that is not repaired yet: its folders
    // are already spelled for `to`, and its hold is never dropped by this one.
    let earlier = earlier_hold(&targets.held_syncs(field(profile, "id"))?, to);
    let held_folders = earlier
        .as_ref()
        .map(|left| left.folders.clone())
        .unwrap_or_default();
    let plan = plan_with_held(
        dirs,
        profile,
        to,
        cloud,
        pairs,
        aerocloud_busy,
        &held_folders,
    )?;
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

    // Puts the written history files back; answers how many could not be.
    let roll_back = |written: &[PathBuf]| {
        written
            .iter()
            .filter(|path| {
                let put_back = match originals.get(*path) {
                    Some(bytes) => {
                        crate::sync::atomic_write(path, bytes).map_err(|e| e.to_string())
                    }
                    None => std::fs::remove_file(path).map_err(|e| e.to_string()),
                };
                if let Err(e) = &put_back {
                    tracing::warn!(
                        "proton mode switch: sync history {} not put back: {e}",
                        path.display()
                    );
                }
                put_back.is_err()
            })
            .count()
    };

    let mut new_cloud = cloud.clone();
    let mut new_pairs = pairs.clone();
    let name = field(profile, "name").to_string();
    let mut cloud_changed = false;
    if bound_config(cloud, &id, &name) {
        if let Some(target) =
            held_or_translated(&cloud.remote_folder, None, from, to, &held_folders)
        {
            cloud_changed |= target != new_cloud.remote_folder;
            new_cloud.remote_folder = target;
        }
    }
    for pair in new_pairs
        .pairs
        .iter_mut()
        .filter(|pair| !name.is_empty() && pair.server_profile == name)
    {
        if let Some(target) =
            held_or_translated(&pair.remote_path, Some(&pair.id), from, to, &held_folders)
        {
            cloud_changed |= target != pair.remote_path;
            pair.remote_path = target;
        }
    }
    // The AeroCloud state a failed switch leaves, for its error.
    let cloud_left = |what: &str| {
        format!(
            "{what}: AeroCloud names its folders as {} mode spells them while the profile stays in {} mode; switch the profile to {} mode or set the AeroCloud folder again before the next sync",
            to.label(),
            from.label(),
            to.label()
        )
    };
    // What a failed put back leaves, held from syncing until repaired.
    let unrepaired = UnrepairedSwitch {
        profile_id: id.clone(),
        profile_name: name.clone(),
        to,
        // Each entry this switch changes with the folder it writes, then the
        // entries of the earlier hold it does not write again.
        folders: std::iter::once((None, &cloud.remote_folder, &new_cloud.remote_folder))
            .chain(
                pairs
                    .pairs
                    .iter()
                    .zip(&new_pairs.pairs)
                    .map(|(was, wrote)| (Some(&wrote.id), &was.remote_path, &wrote.remote_path)),
            )
            .filter(|(_, was, wrote)| was != wrote)
            .map(|(pair, _, wrote)| HeldFolder {
                pair: pair.cloned(),
                folder: wrote.clone(),
            })
            .chain(held_folders.iter().cloned())
            .fold(Vec::new(), |mut folders: Vec<HeldFolder>, held| {
                if !folders.iter().any(|folder| folder.pair == held.pair) {
                    folders.push(held);
                }
                folders
            }),
    };
    // The syncs of the folders the switch changes are held before any
    // change, and released only when the switch is done or everything it
    // changed in AeroCloud is back: a failure at any step, of a put back
    // too, can never leave a sync free to run on a folder spelled for the
    // other mode. A hold that cannot be written refuses the switch.
    let holds = cloud_changed || earlier.is_some();
    if holds {
        if let Err(e) = targets.hold_syncs(&unrepaired) {
            return Err(format!(
                "AeroCloud syncs could not be held, nothing was changed: {e}"
            ));
        }
    }
    let (applied, outcome) = write_switch(
        targets,
        (&id, &name),
        to,
        &plan,
        (cloud, pairs),
        (&new_cloud, &new_pairs),
        cloud_changed,
        &writes,
        &moved,
        &new_paths,
        &roll_back,
        &cloud_left,
    );
    let released = match (holds, outcome, &earlier) {
        (false, _, _) | (true, SwitchOutcome::FoldersLeft, _) => Ok(()),
        // Everything this switch changed is back: the earlier hold stands.
        (true, SwitchOutcome::PutBack, Some(earlier)) => targets.hold_syncs(earlier),
        (true, SwitchOutcome::PutBack, None) | (true, SwitchOutcome::Done, _) => {
            targets.release_syncs(&id)
        }
    };
    if let Err(e) = released {
        // Left as it is, the record holds a superset of what it should:
        // folders no sync names any more, or a profile already in its mode,
        // which the next sync drops. It never frees a sync it should hold.
        tracing::warn!("proton mode switch: sync hold not narrowed: {e}");
    }
    applied.map(|()| plan)
}

/// How the writes of a switch ended, for its hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SwitchOutcome {
    /// The profile is in the new mode, with its folders.
    Done,
    /// The switch failed and everything it changed in AeroCloud is back.
    PutBack,
    /// AeroCloud keeps folders spelled for the new mode while the profile
    /// stays in the old one: the hold is kept.
    FoldersLeft,
}

/// The writes of [`apply_in`] once the syncs are held, and how they ended.
#[allow(clippy::too_many_arguments)]
fn write_switch(
    targets: &mut dyn SwitchTargets,
    profile: (&str, &str),
    to: ProtonMode,
    plan: &SwitchPlan,
    (cloud, pairs): (&CloudConfig, &CloudPairsConfig),
    (new_cloud, new_pairs): (&CloudConfig, &CloudPairsConfig),
    cloud_changed: bool,
    writes: &[(PathBuf, Vec<u8>)],
    moved: &[PathBuf],
    new_paths: &HashSet<PathBuf>,
    roll_back: &dyn Fn(&[PathBuf]) -> usize,
    cloud_left: &dyn Fn(&str) -> String,
) -> (Result<(), String>, SwitchOutcome) {
    let held = |what: String| format!("{what}; its syncs are held until then");
    let mut written = Vec::new();
    for (path, bytes) in writes {
        if let Err(e) = crate::sync::atomic_write(path, bytes) {
            let left = left_behind(roll_back(&written), None);
            return (
                Err(format!("sync history not moved, {left}: {e}")),
                SwitchOutcome::PutBack,
            );
        }
        written.push(path.clone());
    }

    if cloud_changed {
        if let Err(e) = targets.store_cloud(profile, (cloud, pairs), new_cloud, new_pairs) {
            let settings_only = matches!(e, CloudStoreError::SettingsOnly(_));
            let cloud = settings_only.then(|| {
                held(cloud_left(
                    "the AeroCloud settings were updated and its pairs were not",
                ))
            });
            let left = left_behind(roll_back(&written), cloud);
            return (
                Err(format!("AeroCloud not updated, {left}: {e}")),
                if settings_only {
                    SwitchOutcome::FoldersLeft
                } else {
                    SwitchOutcome::PutBack
                },
            );
        }
    }

    let initial = plan
        .changes
        .iter()
        .find(|change| change.kind == "initialPath")
        .and_then(|change| change.to.clone());
    // A profile left in the new mode keeps the moved folders and histories:
    // putting those back would point them at folders the profile no longer
    // spells that way. The error says what happened.
    let mut switched_anyway = None;
    match targets.store_profile(profile.0, to, initial.as_deref()) {
        Ok(()) => {}
        Err(ProfileStoreError::NotSaved(e)) => {
            // AeroCloud is put back over the files as they are now: a pair
            // added since is kept, and a put back that fails is said.
            let cloud = if cloud_changed {
                match targets.put_back_cloud(profile, (new_cloud, new_pairs), cloud, pairs) {
                    Ok(()) => None,
                    Err(back) => Some(held(cloud_left(&format!(
                        "AeroCloud could not be put back ({back})"
                    )))),
                }
            } else {
                None
            };
            let folders_left = cloud.is_some();
            let left = left_behind(roll_back(&written), cloud);
            return (
                Err(format!("profile not saved, {left}: {e}")),
                if folders_left {
                    SwitchOutcome::FoldersLeft
                } else {
                    SwitchOutcome::PutBack
                },
            );
        }
        Err(ProfileStoreError::Switched(e)) => switched_anyway = Some(e),
    }

    for old in moved {
        if !new_paths.contains(old) {
            if let Err(e) = std::fs::remove_file(old) {
                tracing::warn!(
                    "proton mode switch: old sync history {} not removed: {e}",
                    old.display()
                );
            }
        }
    }
    if let Some(e) = switched_anyway {
        return (
            Err(format!(
                "the profile is now in {} mode, but {e}; its folders and sync histories were moved with it",
                to.label()
            )),
            SwitchOutcome::Done,
        );
    }
    (Ok(()), SwitchOutcome::Done)
}

/// One edit of a profile list: true when it changed the list.
type ProfileEdit<'a> = dyn FnMut(&mut Vec<Value>) -> bool + 'a;
/// Runs an edit in the vault and answers the stored list and whether the edit
/// was written.
type ProfileUpdate<'a> = dyn FnMut(&mut ProfileEdit<'_>) -> Result<(Vec<Value>, bool), String> + 'a;
/// Copies the stored profile list into the legacy blob.
type ProfileMirror<'a> = dyn FnMut(&[Value]) -> Result<(), String> + 'a;

/// Writes the mode on the profile in a partitioned vault, then mirrors the
/// partition into the legacy profile list when `mirror` is given (the active
/// account). The two writes are one change: when the mirror fails, the
/// profile is put back as it was in the partition, and only a failure of
/// that too leaves it switched. `update` is
/// [`crate::user_partitions::cli_update_server_profiles_for_user`] for the
/// account; tests pass an in-memory list.
fn store_partitioned_profile(
    id: &str,
    to: ProtonMode,
    initial_path: Option<&str>,
    update: &mut ProfileUpdate<'_>,
    mirror: Option<&mut ProfileMirror<'_>>,
) -> Result<(), ProfileStoreError> {
    let mut previous: Option<Value> = None;
    let (stored, written) = update(&mut |profiles: &mut Vec<Value>| {
        let Some(profile) = profiles.iter_mut().find(|p| field(p, "id") == id) else {
            return false;
        };
        previous = Some(profile.clone());
        set_mode(profile, to, initial_path);
        true
    })
    .map_err(ProfileStoreError::NotSaved)?;
    let Some(previous) = previous.filter(|_| written) else {
        return Err(ProfileStoreError::NotSaved(
            "the profile was removed in the meantime".to_string(),
        ));
    };
    let Some(mirror) = mirror else {
        return Ok(());
    };
    let Err(mirror_error) = mirror(&stored) else {
        return Ok(());
    };
    let restored = update(&mut |profiles: &mut Vec<Value>| {
        let Some(profile) = profiles.iter_mut().find(|p| field(p, "id") == id) else {
            return false;
        };
        *profile = previous.clone();
        true
    });
    match restored {
        Ok((_, true)) => Err(ProfileStoreError::NotSaved(format!(
            "the legacy profile list was not updated ({mirror_error}), and the profile was put back"
        ))),
        Ok((_, false)) => Err(ProfileStoreError::Switched(format!(
            "the legacy profile list was not updated ({mirror_error}) and the profile could not be put back: it was removed in the meantime"
        ))),
        Err(restore_error) => Err(ProfileStoreError::Switched(format!(
            "the legacy profile list was not updated ({mirror_error}) and putting the profile back failed ({restore_error})"
        ))),
    }
}

/// The app's and the CLI's stores: AeroCloud's config files, and the vault's
/// profile list (the active account's partition, mirrored into the legacy
/// blob the way the CLI keeps it, or the blob alone before partitions).
pub struct DiskTargets<'a> {
    pub store: &'a crate::credential_store::CredentialStore,
    pub user_id: Option<i64>,
}

impl SwitchTargets for DiskTargets<'_> {
    fn store_cloud(
        &mut self,
        profile: (&str, &str),
        read_as: (&CloudConfig, &CloudPairsConfig),
        cloud: &CloudConfig,
        pairs: &CloudPairsConfig,
    ) -> Result<(), CloudStoreError> {
        store_cloud_files(profile, read_as, cloud, pairs)
    }

    fn put_back_cloud(
        &mut self,
        profile: (&str, &str),
        written: (&CloudConfig, &CloudPairsConfig),
        cloud: &CloudConfig,
        pairs: &CloudPairsConfig,
    ) -> Result<(), CloudStoreError> {
        put_back_cloud_files(profile, written, cloud, pairs)
    }

    fn hold_syncs(&mut self, left: &UnrepairedSwitch) -> Result<(), String> {
        with_unrepaired_at(&unrepaired_path(), |held| {
            held.retain(|other| other.profile_id != left.profile_id);
            held.push(left.clone());
        })
    }

    fn release_syncs(&mut self, profile_id: &str) -> Result<(), String> {
        with_unrepaired_at(&unrepaired_path(), |held| {
            held.retain(|other| other.profile_id != profile_id);
        })
    }

    fn held_syncs(&mut self, profile_id: &str) -> Result<Option<UnrepairedSwitch>, String> {
        read_held_syncs(profile_id)
    }

    fn store_profile(
        &mut self,
        id: &str,
        to: ProtonMode,
        initial_path: Option<&str>,
    ) -> Result<(), ProfileStoreError> {
        let store = self.store;
        match self.user_id {
            Some(user) => {
                let active = crate::user_partitions::cli_get_active_user(store)
                    .ok()
                    .flatten()
                    .map(|user| user.id);
                let mut update = |apply: &mut ProfileEdit<'_>| {
                    crate::user_partitions::cli_update_server_profiles_for_user(store, user, apply)
                };
                let mut mirror = |profiles: &[Value]| {
                    crate::user_partitions::mirror_active_profiles_to_legacy_blob(store, profiles)
                        .map(|_| ())
                };
                let mirror: Option<&mut ProfileMirror<'_>> = if active == Some(user) {
                    Some(&mut mirror)
                } else {
                    None
                };
                store_partitioned_profile(id, to, initial_path, &mut update, mirror)
            }
            None => {
                let not_saved = ProfileStoreError::NotSaved;
                // Read and written under the lock of the list it stands in for.
                let _lock = crate::user_partitions::cli_db_path()
                    .and_then(|db| crate::user_partitions::ProfileListLock::acquire_for_path(&db))
                    .map_err(not_saved)?;
                let raw = store
                    .get("config_server_profiles")
                    .map_err(|e| not_saved(e.to_string()))?;
                let mut profiles: Vec<Value> = serde_json::from_str(&raw)
                    .map_err(|_| not_saved("invalid saved profiles".to_string()))?;
                let Some(profile) = profiles.iter_mut().find(|p| field(p, "id") == id) else {
                    return Err(not_saved(
                        "the profile was removed in the meantime".to_string(),
                    ));
                };
                set_mode(profile, to, initial_path);
                let serialized =
                    serde_json::to_string(&profiles).map_err(|e| not_saved(e.to_string()))?;
                store
                    .store("config_server_profiles", &serialized)
                    .map_err(|e| not_saved(e.to_string()))
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
    let earlier = earlier_hold(&read_held_syncs(field(profile, "id"))?, to);
    plan_with_held(
        &HistoryDirs::on_disk()?,
        profile,
        to,
        &crate::cloud_config::load_cloud_config_strict()?,
        &crate::cloud_pairs::load_cloud_pairs_config_strict()?,
        aerocloud_busy,
        &earlier.map(|left| left.folders).unwrap_or_default(),
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
        &crate::cloud_config::load_cloud_config_strict()?,
        &crate::cloud_pairs::load_cloud_pairs_config_strict()?,
        aerocloud_busy,
        targets,
    )
}

/// Why the CLI refuses a switch while AeroCloud uses the profile: the CLI
/// cannot see whether the app is running AeroCloud with the old folders.
pub fn cli_aerocloud_refusal(profile: &Value) -> Option<String> {
    let id = field(profile, "id");
    let name = field(profile, "name");
    aerocloud_refusal_from(
        crate::cloud_config::load_cloud_config_strict(),
        crate::cloud_pairs::load_cloud_pairs_config_strict(),
        id,
        name,
    )
}

/// The refusal decision on already-loaded stores, pure so the
/// unreadable-file rule is testable without touching disk.
fn aerocloud_refusal_from(
    cloud: Result<crate::cloud_config::CloudConfig, String>,
    pairs: Result<crate::cloud_pairs::CloudPairsConfig, String>,
    id: &str,
    name: &str,
) -> Option<String> {
    // Strict loads: an unreadable config cannot prove the profile is NOT
    // bound, so the switch is refused until the file can be read again.
    let cloud = match cloud {
        Ok(cloud) => cloud,
        Err(e) => {
            return Some(format!(
                "the AeroCloud config cannot be read ({e}), so a binding to this profile cannot be ruled out: fix or remove the file, then switch"
            ));
        }
    };
    if cloud.enabled && bound_config(&cloud, id, name) {
        return Some(format!(
            "AeroCloud syncs {} with this profile: run `aeroftp-cli aerocloud disable`, switch, then enable it again (or switch from the app, which updates AeroCloud itself)",
            cloud.remote_folder
        ));
    }
    let pairs = match pairs {
        Ok(pairs) => pairs,
        Err(e) => {
            return Some(format!(
                "the AeroCloud pairs file cannot be read ({e}), so a binding to this profile cannot be ruled out: fix or remove the file, then switch"
            ));
        }
    };
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

    /// An unreadable AeroCloud store cannot prove the profile is unbound,
    /// so the CLI switch is refused instead of failing open.
    #[test]
    fn an_unreadable_aerocloud_store_refuses_the_switch() {
        let cloud_err = Err("Failed to parse cloud config".to_string());
        let pairs_ok = Ok(crate::cloud_pairs::CloudPairsConfig::default());
        let refusal =
            aerocloud_refusal_from(cloud_err, pairs_ok, "srv_1", "Prod").expect("must refuse");
        assert!(refusal.contains("cannot be read"), "{refusal}");

        let cloud_ok = Ok(crate::cloud_config::CloudConfig::default());
        let pairs_err = Err("Failed to parse cloud pairs".to_string());
        let refusal =
            aerocloud_refusal_from(cloud_ok, pairs_err, "srv_1", "Prod").expect("must refuse");
        assert!(refusal.contains("cannot be read"), "{refusal}");

        // Readable and unbound: no refusal.
        assert!(aerocloud_refusal_from(
            Ok(crate::cloud_config::CloudConfig::default()),
            Ok(crate::cloud_pairs::CloudPairsConfig::default()),
            "srv_1",
            "Prod"
        )
        .is_none());
    }

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

    /// A held folder of the settings (`pair` None) or of a pair.
    fn held(pair: Option<&str>, folder: &str) -> HeldFolder {
        HeldFolder {
            pair: pair.map(str::to_string),
            folder: folder.to_string(),
        }
    }

    #[derive(Default)]
    struct Memory {
        cloud: Option<(CloudConfig, CloudPairsConfig)>,
        profile: Option<(String, ProtonMode, Option<String>)>,
        fail_profile: bool,
        /// A partitioned vault's profile list: the profile is written there
        /// through [`store_partitioned_profile`], as the app does.
        partition: Option<Vec<Value>>,
        /// The legacy blob mirror fails after the partition write.
        fail_mirror: bool,
        /// Every partition write after the first fails.
        fail_restore: bool,
        cloud_writes: usize,
        /// The AeroCloud put back fails (a disk that filled up meanwhile).
        refuse_cloud_restore: bool,
        /// Added to the stored pairs right after the forward AeroCloud write,
        /// as the app or the CLI adds a pair while the switch runs.
        pair_added_meanwhile: Option<crate::cloud_pairs::CloudPathPair>,
        /// Every AeroCloud write fails so.
        cloud_refusal: Option<CloudStoreError>,
        /// Made on the stored settings right after the forward AeroCloud
        /// write, as the settings view saves while the switch runs.
        settings_edited_meanwhile: Option<fn(&mut CloudConfig)>,
        /// The syncs held by [`SwitchTargets::hold_syncs`].
        held: Vec<UnrepairedSwitch>,
        /// [`SwitchTargets::hold_syncs`] fails.
        refuse_hold: bool,
        /// [`SwitchTargets::release_syncs`] fails.
        refuse_release: bool,
        /// The pairs write fails after the settings write, as a pairs file
        /// changed since the switch read it refuses it.
        refuse_pairs: bool,
        /// Made read-only at the AeroCloud write: a history file written
        /// there before cannot be removed again (Unix permissions).
        #[cfg_attr(not(unix), allow(dead_code))]
        lock_dir_on_cloud: Option<PathBuf>,
    }

    impl SwitchTargets for Memory {
        fn store_cloud(
            &mut self,
            profile: (&str, &str),
            read_as: (&CloudConfig, &CloudPairsConfig),
            cloud: &CloudConfig,
            pairs: &CloudPairsConfig,
        ) -> Result<(), CloudStoreError> {
            self.cloud_writes += 1;
            #[cfg(unix)]
            if let Some(dir) = &self.lock_dir_on_cloud {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o555)).unwrap();
            }
            if let Some(refused) = self.cloud_refusal.clone() {
                return Err(refused);
            }
            let mut pairs = pairs.clone();
            pairs.pairs.extend(self.pair_added_meanwhile.take());
            let mut settings = cloud.clone();
            if let Some(edit) = self.settings_edited_meanwhile.take() {
                edit(&mut settings);
            }
            if self.refuse_pairs {
                let left = settings_back_after_refused_pairs(
                    &mut settings,
                    cloud,
                    read_as.0,
                    profile,
                    "pairs refused".to_string(),
                );
                self.cloud = Some((settings, read_as.1.clone()));
                return Err(left);
            }
            self.cloud = Some((settings, pairs));
            Ok(())
        }
        fn put_back_cloud(
            &mut self,
            profile: (&str, &str),
            written: (&CloudConfig, &CloudPairsConfig),
            cloud: &CloudConfig,
            pairs: &CloudPairsConfig,
        ) -> Result<(), CloudStoreError> {
            self.cloud_writes += 1;
            if self.refuse_cloud_restore {
                return Err(CloudStoreError::NotSaved("disk full".to_string()));
            }
            let (mut settings, mut stored) = self.cloud.clone().unwrap_or_default();
            put_back_pair_folders(&mut stored, written.1, pairs);
            let kept = put_back_settings_folder(&mut settings, written.0, cloud, profile);
            self.cloud = Some((settings, stored));
            if kept {
                return Err(CloudStoreError::SettingsOnly(
                    SETTINGS_KEPT_THE_FOLDER.to_string(),
                ));
            }
            Ok(())
        }
        fn hold_syncs(&mut self, left: &UnrepairedSwitch) -> Result<(), String> {
            if self.refuse_hold {
                return Err("disk full".to_string());
            }
            self.held
                .retain(|other| other.profile_id != left.profile_id);
            self.held.push(left.clone());
            Ok(())
        }
        fn release_syncs(&mut self, profile_id: &str) -> Result<(), String> {
            if self.refuse_release {
                return Err("disk full".to_string());
            }
            self.held.retain(|other| other.profile_id != profile_id);
            Ok(())
        }
        fn held_syncs(&mut self, profile_id: &str) -> Result<Option<UnrepairedSwitch>, String> {
            Ok(self
                .held
                .iter()
                .find(|left| left.profile_id == profile_id)
                .cloned())
        }
        fn store_profile(
            &mut self,
            id: &str,
            to: ProtonMode,
            initial: Option<&str>,
        ) -> Result<(), ProfileStoreError> {
            if self.fail_profile {
                return Err(ProfileStoreError::NotSaved("vault locked".to_string()));
            }
            if let Some(partition) = self.partition.as_mut() {
                let fail_restore = self.fail_restore;
                let mut writes = 0;
                let mut update = |apply: &mut ProfileEdit<'_>| {
                    writes += 1;
                    if writes > 1 && fail_restore {
                        return Err("partition write failed".to_string());
                    }
                    let mut next = partition.clone();
                    let written = apply(&mut next);
                    if written {
                        *partition = next.clone();
                    }
                    Ok((next, written))
                };
                let mut mirror = |_: &[Value]| -> Result<(), String> {
                    Err("legacy blob unreadable".to_string())
                };
                let mirror: Option<&mut ProfileMirror<'_>> = if self.fail_mirror {
                    Some(&mut mirror)
                } else {
                    None
                };
                return store_partitioned_profile(id, to, initial, &mut update, mirror);
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
        let journal = write_journal(&f.dirs, "/home/u/docs", "/my-files/docs", Some("p1"), true);
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

    /// Every file of the history folders with its bytes.
    fn snapshot(dirs: &HistoryDirs) -> Vec<(String, Vec<u8>)> {
        let mut files = Vec::new();
        for dir in [&dirs.index, &dirs.journal] {
            for name in files_in(dir) {
                let bytes = std::fs::read(dir.join(&name)).unwrap();
                files.push((name, bytes));
            }
        }
        files
    }

    /// Audit 4.2.5 B02: a pending journal that records the profile (what a
    /// running AeroSync writes before its transfers) blocks the switch, and
    /// nothing is written.
    #[test]
    fn a_pending_sync_of_the_profile_blocks_the_switch_before_any_write() {
        let f = fixture();
        write_index(
            &f.dirs,
            "/home/u/docs",
            "/my-files/docs",
            Some("p1"),
            "a.txt",
        );
        write_journal(&f.dirs, "/home/u/docs", "/my-files/docs", Some("p1"), false);
        let before = snapshot(&f.dirs);
        let plan = plan_in(
            &f.dirs,
            &profile(ProtonMode::Cli, ""),
            ProtonMode::Native,
            &CloudConfig::default(),
            &CloudPairsConfig::default(),
            None,
        )
        .unwrap();
        assert!(
            plan.blockers.iter().any(|b| b.contains("still pending")),
            "{:?}",
            plan.blockers
        );
        let mut memory = Memory::default();
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
        assert!(err.contains("still pending"), "{err}");
        assert_eq!(snapshot(&f.dirs), before);
        assert!(memory.profile.is_none() && memory.cloud.is_none());
    }

    /// Audit 4.2.5 B03: two histories of one local folder whose remote
    /// folders translate to the same spelling (`/my-files` and `/my-files/`
    /// both become `/`) would write the same new file, and the later one
    /// would replace the other's baseline. The switch is refused and both
    /// stay as they were, for indexes and for signed journals.
    #[test]
    fn histories_that_translate_to_one_folder_refuse_the_switch() {
        {
            let (a, b) = ("/my-files", "/my-files/");
            // Indexes.
            let f = fixture();
            write_index(&f.dirs, "/home/u/docs", a, Some("p1"), "first.txt");
            write_index(&f.dirs, "/home/u/docs", b, Some("p1"), "second.txt");
            let before = snapshot(&f.dirs);
            let mut memory = Memory::default();
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
            assert!(err.contains("would both become"), "{a} {b}: {err}");
            assert_eq!(snapshot(&f.dirs), before, "{a} {b}");
            assert!(memory.profile.is_none());

            // Signed journals.
            let g = fixture();
            for remote in [a, b] {
                let mut journal = crate::sync::SyncJournal::new(
                    "/home/u/docs".to_string(),
                    remote.to_string(),
                    crate::sync::CompareDirection::Bidirectional,
                    crate::sync::RetryPolicy::default(),
                    crate::sync::VerifyPolicy::default(),
                );
                journal.profile_id = Some("p1".to_string());
                journal.completed = true;
                crate::sync::save_sync_journal_in(&g.dirs.journal, &journal).unwrap();
            }
            let before = snapshot(&g.dirs);
            let err = apply_in(
                &g.dirs,
                &profile(ProtonMode::Cli, ""),
                ProtonMode::Native,
                &CloudConfig::default(),
                &CloudPairsConfig::default(),
                None,
                &mut Memory::default(),
            )
            .unwrap_err();
            assert!(err.contains("would both become"), "journals {a} {b}: {err}");
            assert_eq!(snapshot(&g.dirs), before, "journals {a} {b}");
        }

        // Below My Files the spellings stay apart: `/my-files/docs/` becomes
        // `/docs/` and `/my-files//docs` becomes `//docs`, other files than
        // `/docs`, so all three arrive, as they lived side by side before.
        let f = fixture();
        write_index(
            &f.dirs,
            "/home/u/docs",
            "/my-files/docs",
            Some("p1"),
            "one.txt",
        );
        write_index(
            &f.dirs,
            "/home/u/docs",
            "/my-files//docs",
            Some("p1"),
            "two.txt",
        );
        write_index(
            &f.dirs,
            "/home/u/docs",
            "/my-files/docs/",
            Some("p1"),
            "three.txt",
        );
        apply_in(
            &f.dirs,
            &profile(ProtonMode::Cli, ""),
            ProtonMode::Native,
            &CloudConfig::default(),
            &CloudPairsConfig::default(),
            None,
            &mut Memory::default(),
        )
        .unwrap();
        let one = read(
            &f.dirs
                .index
                .join(crate::sync::index_filename("/home/u/docs", "/docs")),
        );
        let two = read(
            &f.dirs
                .index
                .join(crate::sync::index_filename("/home/u/docs", "//docs")),
        );
        let three = read(
            &f.dirs
                .index
                .join(crate::sync::index_filename("/home/u/docs", "/docs/")),
        );
        assert_eq!(one["files"]["one.txt"]["size"], 1);
        assert_eq!(two["files"]["two.txt"]["size"], 1);
        assert_eq!(three["files"]["three.txt"]["size"], 1);
        assert_eq!(files_in(&f.dirs.index).len(), 3);
    }

    /// Audit 4.2.5 B04: in a partitioned vault the profile is written, then
    /// mirrored into the legacy list. When the mirror fails the profile is
    /// put back, so profile, AeroCloud, index, journal and signature all
    /// match what they were.
    #[test]
    fn a_failed_mirror_after_the_profile_write_puts_everything_back() {
        let f = fixture();
        write_index(
            &f.dirs,
            "/home/u/docs",
            "/my-files/docs",
            Some("p1"),
            "a.txt",
        );
        let mut journal = crate::sync::SyncJournal::new(
            "/home/u/docs".to_string(),
            "/my-files/docs".to_string(),
            crate::sync::CompareDirection::Bidirectional,
            crate::sync::RetryPolicy::default(),
            crate::sync::VerifyPolicy::default(),
        );
        journal.profile_id = Some("p1".to_string());
        journal.completed = true;
        crate::sync::save_sync_journal_in(&f.dirs.journal, &journal).unwrap();
        let cloud = CloudConfig {
            remote_folder: "/my-files/cloud".to_string(),
            server_profile: "Proton".to_string(),
            server_profile_id: Some("p1".to_string()),
            ..CloudConfig::default()
        };
        let original = profile(ProtonMode::Cli, "/my-files/docs");
        let other = json!({"id": "p2", "name": "Other", "protocol": "sftp"});
        let before = snapshot(&f.dirs);
        let mut memory = Memory {
            partition: Some(vec![original.clone(), other.clone()]),
            fail_mirror: true,
            ..Memory::default()
        };
        let err = apply_in(
            &f.dirs,
            &original,
            ProtonMode::Native,
            &cloud,
            &CloudPairsConfig::default(),
            None,
            &mut memory,
        )
        .unwrap_err();
        assert!(err.contains("nothing was changed"), "{err}");
        assert_eq!(
            memory.partition,
            Some(vec![original.clone(), other.clone()])
        );
        assert_eq!(memory.cloud.unwrap().0.remote_folder, "/my-files/cloud");
        assert_eq!(snapshot(&f.dirs), before);

        // Putting the profile back fails too: it stays switched, and so do
        // its folders and histories, and the error says so.
        let mut memory = Memory {
            partition: Some(vec![original.clone(), other]),
            fail_mirror: true,
            fail_restore: true,
            ..Memory::default()
        };
        let err = apply_in(
            &f.dirs,
            &original,
            ProtonMode::Native,
            &cloud,
            &CloudPairsConfig::default(),
            None,
            &mut memory,
        )
        .unwrap_err();
        assert!(err.contains("now in native mode"), "{err}");
        assert!(!err.contains("nothing was changed"), "{err}");
        let partition = memory.partition.unwrap();
        assert_eq!(ProtonMode::of_profile(&partition[0]), ProtonMode::Native);
        assert_eq!(partition[0]["initialPath"], "/docs");
        assert_eq!(memory.cloud.unwrap().0.remote_folder, "/cloud");
        assert!(f
            .dirs
            .index
            .join(crate::sync::index_filename("/home/u/docs", "/docs"))
            .exists());
        assert!(f
            .dirs
            .journal
            .join(crate::sync::journal_filename("/home/u/docs", "/docs"))
            .exists());
        assert!(!f
            .dirs
            .index
            .join(crate::sync::index_filename(
                "/home/u/docs",
                "/my-files/docs"
            ))
            .exists());
    }

    /// A switch that fails after AeroCloud was written says what is left
    /// when it cannot put everything back: never "nothing was changed".
    /// The AeroCloud put back failed, the settings written without the
    /// pairs, a history file that
    /// cannot be removed again: each is said, and a put back that worked
    /// still says nothing was changed.
    /// A pair added between the switch's AeroCloud write and a rollback
    /// (the profile not saved) is kept, and the switch's own folders are put
    /// back: the settings' and the pair's it moved.
    #[test]
    fn a_pair_added_while_a_switch_runs_is_kept_by_its_rollback() {
        let cloud = CloudConfig {
            remote_folder: "/my-files/cloud".to_string(),
            server_profile: "Proton".to_string(),
            server_profile_id: Some("p1".to_string()),
            ..CloudConfig::default()
        };
        let pair = |id: &str, profile: &str, remote: &str| crate::cloud_pairs::CloudPathPair {
            id: id.to_string(),
            remote_path: remote.to_string(),
            ..crate::cloud_pairs::CloudPathPair::from_legacy(&CloudConfig {
                server_profile: profile.to_string(),
                ..CloudConfig::default()
            })
        };
        let pairs = CloudPairsConfig {
            pairs: vec![pair("moved", "Proton", "/my-files/docs")],
            ..CloudPairsConfig::default()
        };
        let f = fixture();
        let mut memory = Memory {
            fail_profile: true,
            pair_added_meanwhile: Some(pair("added", "nas", "/srv/new")),
            ..Memory::default()
        };
        let err = apply_in(
            &f.dirs,
            &profile(ProtonMode::Cli, "/my-files/docs"),
            ProtonMode::Native,
            &cloud,
            &pairs,
            None,
            &mut memory,
        )
        .unwrap_err();
        assert!(err.contains("profile not saved"), "{err}");
        assert!(err.contains("nothing was changed"), "{err}");
        let (settings, stored) = memory.cloud.unwrap();
        assert_eq!(settings.remote_folder, "/my-files/cloud");
        let folders: Vec<_> = stored
            .pairs
            .iter()
            .map(|p| (p.id.as_str(), p.remote_path.as_str()))
            .collect();
        assert_eq!(
            folders,
            [("moved", "/my-files/docs"), ("added", "/srv/new")]
        );
        assert_eq!(memory.cloud_writes, 2);
    }

    /// A settings edit saved while the switch runs is kept by its rollback:
    /// the folder goes back only while it is the one the switch wrote, and
    /// every other field stays as stored.
    #[test]
    fn a_rollback_keeps_a_settings_edit_made_while_the_switch_runs() {
        let cloud = CloudConfig {
            remote_folder: "/my-files/cloud".to_string(),
            server_profile: "Proton".to_string(),
            server_profile_id: Some("p1".to_string()),
            ..CloudConfig::default()
        };
        type Edit = fn(&mut CloudConfig);
        let edits: [(Edit, &str); 2] = [
            (
                |c| {
                    c.remote_folder = "/elsewhere".to_string();
                    c.sync_interval_secs = 60;
                },
                "/elsewhere",
            ),
            (|c| c.sync_interval_secs = 60, "/my-files/cloud"),
        ];
        for (edit, folder) in edits {
            let f = fixture();
            let mut memory = Memory {
                fail_profile: true,
                settings_edited_meanwhile: Some(edit),
                ..Memory::default()
            };
            let err = apply_in(
                &f.dirs,
                &profile(ProtonMode::Cli, "/my-files/docs"),
                ProtonMode::Native,
                &cloud,
                &CloudPairsConfig::default(),
                None,
                &mut memory,
            )
            .unwrap_err();
            assert!(err.contains("nothing was changed"), "{err}");
            let (settings, _) = memory.cloud.unwrap();
            assert_eq!(settings.remote_folder, folder);
            assert_eq!(settings.sync_interval_secs, 60);
        }
    }

    /// The same on the AeroCloud files, which the app and the CLI put back.
    /// Linux only: the settings folder follows `XDG_CONFIG_HOME` there.
    #[test]
    #[cfg(target_os = "linux")]
    fn the_files_put_back_keep_a_settings_edit_made_while_the_switch_runs() {
        let _env = crate::test_env::lock();
        let data = tempfile::tempdir().unwrap();
        let prev = std::env::var_os("XDG_CONFIG_HOME");
        std::env::set_var("XDG_CONFIG_HOME", data.path());
        let original = CloudConfig {
            remote_folder: "/my-files/cloud".to_string(),
            server_profile: "Proton".to_string(),
            ..CloudConfig::default()
        };
        let written = CloudConfig {
            remote_folder: "/cloud".to_string(),
            ..original.clone()
        };
        let mut kept = Vec::new();
        for edit in ["/elsewhere", "/cloud"] {
            crate::cloud_config::with_cloud_config_mut(|stored| {
                *stored = CloudConfig {
                    remote_folder: edit.to_string(),
                    sync_interval_secs: 60,
                    ..written.clone()
                };
                Ok(())
            })
            .unwrap();
            put_back_cloud_files(
                ("p1", "Proton"),
                (&written, &CloudPairsConfig::default()),
                &original,
                &CloudPairsConfig::default(),
            )
            .unwrap();
            let stored = crate::cloud_config::load_cloud_config();
            kept.push((stored.remote_folder, stored.sync_interval_secs));
        }
        // A local folder set since the switch wrote: the folder is kept with
        // it, and the put back says it is incomplete.
        crate::cloud_config::with_cloud_config_mut(|stored| {
            *stored = CloudConfig {
                local_folder: "/home/u/elsewhere".into(),
                ..written.clone()
            };
            Ok(())
        })
        .unwrap();
        let incomplete = put_back_cloud_files(
            ("p1", "Proton"),
            (&written, &CloudPairsConfig::default()),
            &original,
            &CloudPairsConfig::default(),
        );
        let after_local_edit = crate::cloud_config::load_cloud_config();

        // A pairs file changed since the switch read it refuses the pairs
        // write; the settings folder this write made goes back.
        crate::cloud_config::with_cloud_config_mut(|stored| {
            *stored = original.clone();
            Ok(())
        })
        .unwrap();
        let changed = CloudPairsConfig {
            pairs: vec![crate::cloud_pairs::CloudPathPair::from_legacy(&original)],
            ..CloudPairsConfig::default()
        };
        crate::cloud_pairs::with_cloud_pairs_mut(|stored| {
            *stored = changed.clone();
            Ok(())
        })
        .unwrap();
        let refused = store_cloud_files(
            ("p1", "Proton"),
            (&original, &CloudPairsConfig::default()),
            &written,
            &CloudPairsConfig::default(),
        );
        let back = crate::cloud_config::load_cloud_config().remote_folder;
        match prev {
            Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        assert!(
            matches!(refused, Err(CloudStoreError::NotSaved(_))),
            "{refused:?}"
        );
        assert_eq!(back, "/my-files/cloud");
        assert!(
            matches!(&incomplete, Err(CloudStoreError::SettingsOnly(e)) if e.contains("keep its folder")),
            "{incomplete:?}"
        );
        assert_eq!(after_local_edit.remote_folder, "/cloud");
        assert_eq!(
            after_local_edit.local_folder,
            PathBuf::from("/home/u/elsewhere")
        );
        assert_eq!(
            kept,
            [
                ("/elsewhere".to_string(), 60),
                ("/my-files/cloud".to_string(), 60)
            ]
        );
    }

    /// A switch whose rollback fails holds the syncs of the folders it left
    /// spelled for the new mode, and says so; they sync again once the
    /// profile is in that mode or the folder is set again.
    #[test]
    fn a_failed_rollback_holds_its_syncs_until_repaired() {
        let cloud = CloudConfig {
            remote_folder: "/my-files/cloud".to_string(),
            server_profile: "Proton".to_string(),
            server_profile_id: Some("p1".to_string()),
            ..CloudConfig::default()
        };
        let moved = crate::cloud_pairs::CloudPathPair {
            id: "moved".to_string(),
            remote_path: "/my-files/docs".to_string(),
            ..crate::cloud_pairs::CloudPathPair::from_legacy(&CloudConfig {
                server_profile: "Proton".to_string(),
                ..CloudConfig::default()
            })
        };
        let pairs = CloudPairsConfig {
            pairs: vec![moved.clone()],
            ..CloudPairsConfig::default()
        };
        let run = |memory: &mut Memory| {
            apply_in(
                &fixture().dirs,
                &profile(ProtonMode::Cli, "/my-files/docs"),
                ProtonMode::Native,
                &cloud,
                &pairs,
                None,
                memory,
            )
            .unwrap_err()
        };
        let mut memory = Memory {
            fail_profile: true,
            refuse_cloud_restore: true,
            ..Memory::default()
        };
        let err = run(&mut memory);
        assert!(err.contains("its syncs are held until then"), "{err}");
        let held = UnrepairedSwitch {
            profile_id: "p1".to_string(),
            profile_name: "Proton".to_string(),
            to: ProtonMode::Native,
            folders: vec![held(None, "/cloud"), held(Some("moved"), "/docs")],
        };
        assert_eq!(memory.held, std::slice::from_ref(&held));

        let pair_config = crate::cloud_pairs::CloudPathPair {
            remote_path: "/docs".to_string(),
            ..moved.clone()
        }
        .to_cloud_config();
        let settings = CloudConfig {
            remote_folder: "/cloud".to_string(),
            ..cloud.clone()
        };
        let still_cli = |_: &str| Some(ProtonMode::Cli);
        for config in [&settings, &pair_config] {
            let mut list = vec![held.clone()];
            let refusal = unrepaired_refusal(&mut list, config, still_cli).unwrap();
            assert!(
                refusal.contains("switch the profile to native mode"),
                "{refusal}"
            );
            // A refusal names the problem, never the folder.
            assert!(!refusal.contains(&config.remote_folder), "{refusal}");
            assert_eq!(list, std::slice::from_ref(&held));
        }
        // The folder set again, another profile: not held.
        for config in [
            CloudConfig {
                remote_folder: "/my-files/cloud".to_string(),
                ..settings.clone()
            },
            CloudConfig {
                server_profile_id: Some("p2".to_string()),
                ..settings.clone()
            },
        ] {
            let mut list = vec![held.clone()];
            assert_eq!(unrepaired_refusal(&mut list, &config, still_cli), None);
        }
        // The profile now in native mode: repaired, and forgotten.
        let mut list = vec![held.clone()];
        let native = |_: &str| Some(ProtonMode::Native);
        assert_eq!(unrepaired_refusal(&mut list, &settings, native), None);
        assert!(list.is_empty());
    }

    /// Every step of a switch, failing alone and with the put back that
    /// follows it: the syncs of the folders it changes are held before any
    /// change, kept while a folder stays spelled for native mode under a
    /// profile in CLI mode, and released otherwise. No row leaves a sync free
    /// to run on such a folder.
    #[test]
    fn no_step_of_a_switch_leaves_a_sync_free_on_a_mismatched_folder() {
        let cloud = CloudConfig {
            remote_folder: "/my-files/cloud".to_string(),
            server_profile: "Proton".to_string(),
            server_profile_id: Some("p1".to_string()),
            ..CloudConfig::default()
        };
        let moved = crate::cloud_pairs::CloudPathPair {
            id: "moved".to_string(),
            remote_path: "/my-files/docs".to_string(),
            ..crate::cloud_pairs::CloudPathPair::from_legacy(&CloudConfig {
                server_profile: "Proton".to_string(),
                ..CloudConfig::default()
            })
        };
        let pairs = CloudPairsConfig {
            pairs: vec![moved.clone()],
            ..CloudPairsConfig::default()
        };
        let original = profile(ProtonMode::Cli, "/my-files/docs");
        let record = UnrepairedSwitch {
            profile_id: "p1".to_string(),
            profile_name: "Proton".to_string(),
            to: ProtonMode::Native,
            folders: vec![held(None, "/cloud"), held(Some("moved"), "/docs")],
        };
        // Whether a sync of what `memory` stores can run on a folder spelled
        // for native mode while the profile stays in CLI mode.
        let a_sync_runs_mismatched = |memory: &Memory| {
            let native = memory
                .profile
                .as_ref()
                .is_some_and(|(_, mode, _)| *mode == ProtonMode::Native)
                || memory.partition.as_ref().is_some_and(|partition| {
                    ProtonMode::of_profile(&partition[0]) == ProtonMode::Native
                });
            let Some((settings, stored)) = &memory.cloud else {
                return false;
            };
            let mode_of = |_: &str| {
                Some(if native {
                    ProtonMode::Native
                } else {
                    ProtonMode::Cli
                })
            };
            std::iter::once(settings.clone())
                .chain(stored.pairs.iter().map(|pair| pair.to_cloud_config()))
                .filter(|config| {
                    !native
                        && record
                            .folders
                            .iter()
                            .any(|held| held.folder == config.remote_folder)
                })
                .any(|config| {
                    unrepaired_refusal(&mut memory.held.clone(), &config, mode_of).is_none()
                })
        };
        let rows: Vec<(&str, Memory, Option<&str>, bool)> = vec![
            ("done", Memory::default(), None, false),
            (
                "hold not written",
                Memory {
                    refuse_hold: true,
                    ..Memory::default()
                },
                Some("AeroCloud syncs could not be held, nothing was changed"),
                false,
            ),
            (
                "settings not written",
                Memory {
                    cloud_refusal: Some(CloudStoreError::NotSaved("disk full".to_string())),
                    ..Memory::default()
                },
                Some("nothing was changed"),
                false,
            ),
            (
                "pairs not written, settings not put back",
                Memory {
                    cloud_refusal: Some(CloudStoreError::SettingsOnly("refused".to_string())),
                    ..Memory::default()
                },
                Some("its syncs are held until then"),
                true,
            ),
            (
                "profile not saved, AeroCloud put back",
                Memory {
                    fail_profile: true,
                    ..Memory::default()
                },
                Some("nothing was changed"),
                false,
            ),
            (
                "profile not saved, AeroCloud not put back",
                Memory {
                    fail_profile: true,
                    refuse_cloud_restore: true,
                    ..Memory::default()
                },
                Some("its syncs are held until then"),
                true,
            ),
            (
                "profile saved and not put back",
                Memory {
                    partition: Some(vec![original.clone()]),
                    fail_mirror: true,
                    fail_restore: true,
                    ..Memory::default()
                },
                Some("now in native mode"),
                false,
            ),
            (
                "done, hold not released",
                Memory {
                    refuse_release: true,
                    ..Memory::default()
                },
                None,
                true,
            ),
        ];
        for (row, mut memory, refused, kept) in rows {
            let f = fixture();
            let before = snapshot(&f.dirs);
            let applied = apply_in(
                &f.dirs,
                &original,
                ProtonMode::Native,
                &cloud,
                &pairs,
                None,
                &mut memory,
            );
            match refused {
                None => assert!(applied.is_ok(), "{row}: {:?}", applied.err()),
                Some(said) => {
                    let err = applied.expect_err(row);
                    assert!(err.contains(said), "{row}: {err}");
                }
            }
            assert_eq!(memory.held.is_empty(), !kept, "{row}: {:?}", memory.held);
            if kept {
                assert_eq!(memory.held, std::slice::from_ref(&record), "{row}");
            }
            assert!(!a_sync_runs_mismatched(&memory), "{row}");
            if row == "hold not written" {
                assert_eq!(memory.cloud_writes, 0, "{row}");
                assert!(memory.cloud.is_none() && memory.profile.is_none(), "{row}");
                assert_eq!(snapshot(&f.dirs), before, "{row}");
            }
            if row == "done, hold not released" {
                // The profile is in native mode: the next sync drops it.
                let mut held = memory.held.clone();
                let native = |_: &str| Some(ProtonMode::Native);
                let settings = &memory.cloud.as_ref().unwrap().0;
                assert_eq!(unrepaired_refusal(&mut held, settings, native), None);
                assert!(held.is_empty(), "{row}");
            }
        }
    }

    /// A local folder (or protocol, or connection) set on the settings after
    /// the switch wrote them, or their binding written by the profile's name
    /// only, is kept by the rollback, and so is the folder the switch wrote
    /// with it: the settings are still bound to the profile by the rules the
    /// switch finds them with, so the put back is incomplete, the hold stays
    /// and the error says so. Settings moved to another profile are not held
    /// for this one.
    #[test]
    fn a_settings_edit_made_while_the_switch_runs_keeps_its_hold_while_bound_to_the_profile() {
        let cloud = CloudConfig {
            remote_folder: "/my-files/cloud".to_string(),
            server_profile: "Proton".to_string(),
            server_profile_id: Some("p1".to_string()),
            ..CloudConfig::default()
        };
        type Edit = fn(&mut CloudConfig);
        type Row = (&'static str, fn() -> Memory, Edit, Option<&'static str>);
        let local: Edit = |c| c.local_folder = "/home/u/elsewhere".into();
        let other_profile: Edit = |c| {
            c.local_folder = "/home/u/elsewhere".into();
            c.server_profile_id = Some("p2".to_string());
        };
        // `aerocloud set --profile Proton` that could not open the vault:
        // bound by the name only.
        let name_only: Edit = |c| c.server_profile_id = None;
        let refused_profile = || Memory {
            fail_profile: true,
            ..Memory::default()
        };
        let refused_pairs = || Memory {
            refuse_pairs: true,
            ..Memory::default()
        };
        let rows: [Row; 6] = [
            (
                "profile refused",
                refused_profile,
                local,
                Some("could not be put back"),
            ),
            (
                "pairs refused",
                refused_pairs,
                local,
                Some("settings were updated and its pairs were not"),
            ),
            (
                "profile refused, bound by name only",
                refused_profile,
                name_only,
                Some("could not be put back"),
            ),
            (
                "pairs refused, bound by name only",
                refused_pairs,
                name_only,
                Some("settings were updated and its pairs were not"),
            ),
            (
                "profile refused, other profile",
                refused_profile,
                other_profile,
                None,
            ),
            (
                "pairs refused, other profile",
                refused_pairs,
                other_profile,
                None,
            ),
        ];
        for (row, memory, edit, left) in rows {
            let f = fixture();
            let mut memory = Memory {
                settings_edited_meanwhile: Some(edit),
                ..memory()
            };
            let err = apply_in(
                &f.dirs,
                &profile(ProtonMode::Cli, "/my-files/docs"),
                ProtonMode::Native,
                &cloud,
                &CloudPairsConfig::default(),
                None,
                &mut memory,
            )
            .unwrap_err();
            let (settings, _) = memory.cloud.clone().unwrap();
            // The concurrent edit is kept, and with it the switched folder.
            let mut edited = cloud.clone();
            edit(&mut edited);
            assert_eq!(settings.local_folder, edited.local_folder, "{row}");
            assert_eq!(
                settings.server_profile_id, edited.server_profile_id,
                "{row}"
            );
            assert_eq!(settings.remote_folder, "/cloud", "{row}");
            match left {
                Some(said) => {
                    assert!(err.contains(said), "{row}: {err}");
                    assert!(err.contains("keep its folder"), "{row}: {err}");
                    assert!(
                        err.contains("its syncs are held until then"),
                        "{row}: {err}"
                    );
                    assert!(!err.contains("nothing was changed"), "{row}: {err}");
                    let mut held = memory.held.clone();
                    let cli = |_: &str| Some(ProtonMode::Cli);
                    assert!(
                        unrepaired_refusal(&mut held, &settings, cli).is_some(),
                        "{row}"
                    );
                }
                None => {
                    assert!(err.contains("nothing was changed"), "{row}: {err}");
                    assert!(memory.held.is_empty(), "{row}: {:?}", memory.held);
                }
            }
        }
    }

    /// A retry of a failed switch does not translate twice the folders the
    /// earlier one left spelled for the new mode, and never drops its hold:
    /// a second failed attempt leaves the earlier hold as it was, and a
    /// successful one releases it with the folders written once.
    #[test]
    fn a_retry_after_a_failed_switch_keeps_its_hold_and_translates_nothing_twice() {
        let cloud = CloudConfig {
            remote_folder: "/docs".to_string(),
            server_profile: "Proton".to_string(),
            server_profile_id: Some("p1".to_string()),
            ..CloudConfig::default()
        };
        let native = profile(ProtonMode::Native, "/docs");
        let f = fixture();
        let attempt = |memory: &mut Memory, cloud: &CloudConfig, pairs: &CloudPairsConfig| {
            apply_in(
                &f.dirs,
                &native,
                ProtonMode::Cli,
                cloud,
                pairs,
                None,
                memory,
            )
        };

        // First attempt: the profile is not saved and AeroCloud not put back.
        let mut memory = Memory {
            fail_profile: true,
            refuse_cloud_restore: true,
            ..Memory::default()
        };
        attempt(&mut memory, &cloud, &CloudPairsConfig::default()).unwrap_err();
        let (left, _) = memory.cloud.clone().unwrap();
        assert_eq!(left.remote_folder, "/my-files/docs");
        let first = memory.held.clone();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].folders, [held(None, "/my-files/docs")]);

        // The plan of a retry keeps the held folder as it is.
        let plan = plan_with_held(
            &f.dirs,
            &native,
            ProtonMode::Cli,
            &left,
            &CloudPairsConfig::default(),
            None,
            &first[0].folders,
        )
        .unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        let change = plan.changes.iter().find(|c| c.kind == "aerocloud").unwrap();
        assert_eq!(change.to.as_deref(), Some("/my-files/docs"));

        // Second attempt, with a pair added since, fails at the settings
        // write: everything it changed is back, and the earlier hold stands.
        let added = CloudPairsConfig {
            pairs: vec![crate::cloud_pairs::CloudPathPair {
                id: "added".to_string(),
                remote_path: "/photos".to_string(),
                ..crate::cloud_pairs::CloudPathPair::from_legacy(&CloudConfig {
                    server_profile: "Proton".to_string(),
                    ..CloudConfig::default()
                })
            }],
            ..CloudPairsConfig::default()
        };
        memory.fail_profile = false;
        memory.refuse_cloud_restore = false;
        memory.cloud_refusal = Some(CloudStoreError::NotSaved("disk full".to_string()));
        let err = attempt(&mut memory, &left, &added).unwrap_err();
        assert!(err.contains("nothing was changed"), "{err}");
        assert_eq!(memory.held, first);

        // A second attempt that leaves folders too holds them with the
        // earlier ones, never instead of them.
        let mut again = Memory {
            held: first.clone(),
            fail_profile: true,
            refuse_cloud_restore: true,
            ..Memory::default()
        };
        attempt(&mut again, &left, &added).unwrap_err();
        assert_eq!(again.held.len(), 1);
        assert_eq!(
            again.held[0].folders,
            [
                held(Some("added"), "/my-files/photos"),
                held(None, "/my-files/docs")
            ]
        );

        // A retry with nothing new to change, done: released.
        let mut only_held = Memory {
            held: first.clone(),
            ..Memory::default()
        };
        attempt(&mut only_held, &left, &CloudPairsConfig::default()).unwrap();
        assert!(only_held.held.is_empty(), "{:?}", only_held.held);

        // A third attempt succeeds: written once, and released.
        memory.cloud_refusal = None;
        attempt(&mut memory, &left, &added).unwrap();
        let (settings, pairs) = memory.cloud.unwrap();
        assert_eq!(settings.remote_folder, "/my-files/docs");
        assert_eq!(pairs.pairs[0].remote_path, "/my-files/photos");
        assert!(memory.held.is_empty(), "{:?}", memory.held);
    }

    /// A retry knows what an earlier failed switch wrote by its entry, never
    /// by a folder another entry shares: a pair inside a user folder named
    /// `my-files`, not written by the failed switch, is translated by the
    /// retry although its folder reads as the settings folder that switch
    /// left spelled for CLI mode.
    #[test]
    fn a_retry_knows_a_held_folder_by_its_entry_not_by_a_folder_another_pair_shares() {
        let cloud = CloudConfig {
            remote_folder: "/docs".to_string(),
            server_profile: "Proton".to_string(),
            server_profile_id: Some("p1".to_string()),
            ..CloudConfig::default()
        };
        let pairs = CloudPairsConfig {
            pairs: vec![crate::cloud_pairs::CloudPathPair {
                id: "user".to_string(),
                remote_path: "/my-files/docs".to_string(),
                ..crate::cloud_pairs::CloudPathPair::from_legacy(&CloudConfig {
                    server_profile: "Proton".to_string(),
                    ..CloudConfig::default()
                })
            }],
            ..CloudPairsConfig::default()
        };
        let native = profile(ProtonMode::Native, "/docs");
        let f = fixture();

        // The settings are written, the pairs write is refused, and the
        // settings keep the folder the switch wrote (a local folder set
        // meanwhile): only the settings are left spelled for CLI mode.
        let mut memory = Memory {
            refuse_pairs: true,
            settings_edited_meanwhile: Some(|c| c.local_folder = "/home/u/elsewhere".into()),
            ..Memory::default()
        };
        let err = apply_in(
            &f.dirs,
            &native,
            ProtonMode::Cli,
            &cloud,
            &pairs,
            None,
            &mut memory,
        )
        .unwrap_err();
        assert!(err.contains("its syncs are held until then"), "{err}");
        let (left, stored) = memory.cloud.clone().unwrap();
        assert_eq!(left.remote_folder, "/my-files/docs");
        assert_eq!(stored.pairs[0].remote_path, "/my-files/docs");
        assert_eq!(
            memory.held[0].folders,
            [
                held(None, "/my-files/docs"),
                held(Some("user"), "/my-files/my-files/docs")
            ]
        );

        // The retry keeps the settings folder and translates the pair.
        let plan = plan_with_held(
            &f.dirs,
            &native,
            ProtonMode::Cli,
            &left,
            &stored,
            None,
            &memory.held[0].folders,
        )
        .unwrap();
        let to = |kind: &str| {
            plan.changes
                .iter()
                .find(|c| c.kind == kind)
                .and_then(|c| c.to.clone())
        };
        assert_eq!(to("aerocloud").as_deref(), Some("/my-files/docs"));
        assert_eq!(
            to("aerocloudPair").as_deref(),
            Some("/my-files/my-files/docs")
        );
        let mut retry = Memory {
            held: memory.held.clone(),
            ..Memory::default()
        };
        apply_in(
            &f.dirs,
            &native,
            ProtonMode::Cli,
            &left,
            &stored,
            None,
            &mut retry,
        )
        .unwrap();
        let (settings, pairs) = retry.cloud.unwrap();
        assert_eq!(settings.remote_folder, "/my-files/docs");
        assert_eq!(pairs.pairs[0].remote_path, "/my-files/my-files/docs");
        assert!(retry.held.is_empty(), "{:?}", retry.held);
    }

    /// A history that cannot be written refuses the switch before AeroCloud
    /// is touched, and releases the hold it took.
    #[test]
    #[cfg(unix)]
    fn a_history_not_written_releases_the_hold_and_changes_nothing() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 {
            eprintln!("running as root: chmod cannot block the write, skipping");
            return;
        }
        let f = fixture();
        write_index(
            &f.dirs,
            "/home/u/docs",
            "/my-files/docs",
            Some("p1"),
            "a.txt",
        );
        let before = snapshot(&f.dirs);
        std::fs::set_permissions(&f.dirs.index, std::fs::Permissions::from_mode(0o555)).unwrap();
        let cloud = CloudConfig {
            remote_folder: "/my-files/cloud".to_string(),
            server_profile: "Proton".to_string(),
            server_profile_id: Some("p1".to_string()),
            ..CloudConfig::default()
        };
        let mut memory = Memory::default();
        let err = apply_in(
            &f.dirs,
            &profile(ProtonMode::Cli, "/my-files/docs"),
            ProtonMode::Native,
            &cloud,
            &CloudPairsConfig::default(),
            None,
            &mut memory,
        )
        .unwrap_err();
        std::fs::set_permissions(&f.dirs.index, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(err.contains("sync history not moved"), "{err}");
        assert!(err.contains("nothing was changed"), "{err}");
        assert_eq!(memory.cloud_writes, 0);
        assert!(memory.held.is_empty(), "{:?}", memory.held);
        assert_eq!(snapshot(&f.dirs), before);
    }

    #[test]
    fn a_switch_that_cannot_put_everything_back_says_what_is_left() {
        let cloud = CloudConfig {
            remote_folder: "/my-files/cloud".to_string(),
            server_profile: "Proton".to_string(),
            server_profile_id: Some("p1".to_string()),
            ..CloudConfig::default()
        };
        let original = profile(ProtonMode::Cli, "/my-files/docs");
        let run = |memory: &mut Memory, f: &Fixture| {
            apply_in(
                &f.dirs,
                &original,
                ProtonMode::Native,
                &cloud,
                &CloudPairsConfig::default(),
                None,
                memory,
            )
            .unwrap_err()
        };

        // Put back as it was: nothing was changed.
        let f = fixture();
        let mut memory = Memory {
            fail_profile: true,
            ..Memory::default()
        };
        let err = run(&mut memory, &f);
        assert!(err.contains("nothing was changed"), "{err}");

        // The profile not saved and the AeroCloud put back refused.
        let f = fixture();
        let mut memory = Memory {
            fail_profile: true,
            refuse_cloud_restore: true,
            ..Memory::default()
        };
        let err = run(&mut memory, &f);
        assert!(!err.contains("nothing was changed"), "{err}");
        assert!(err.contains("AeroCloud could not be put back"), "{err}");
        assert!(err.contains("switch the profile to native mode"), "{err}");
        assert_eq!(memory.cloud.unwrap().0.remote_folder, "/cloud");

        // The settings written and the pairs not, the settings not put back.
        let f = fixture();
        let mut memory = Memory {
            cloud_refusal: Some(CloudStoreError::SettingsOnly("pairs refused".to_string())),
            ..Memory::default()
        };
        let err = run(&mut memory, &f);
        assert!(!err.contains("nothing was changed"), "{err}");
        assert!(
            err.contains("settings were updated and its pairs were not"),
            "{err}"
        );
        assert!(memory.profile.is_none());

        // A history file written before the AeroCloud write cannot be
        // removed again (not as root, which chmod does not stop).
        #[cfg(unix)]
        if unsafe { libc::geteuid() } != 0 {
            let f = fixture();
            write_index(
                &f.dirs,
                "/home/u/docs",
                "/my-files/docs",
                Some("p1"),
                "a.txt",
            );
            let mut memory = Memory {
                cloud_refusal: Some(CloudStoreError::NotSaved("disk full".to_string())),
                lock_dir_on_cloud: Some(f.dirs.index.clone()),
                ..Memory::default()
            };
            let err = run(&mut memory, &f);
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&f.dirs.index, std::fs::Permissions::from_mode(0o755))
                    .unwrap();
            }
            assert!(!err.contains("nothing was changed"), "{err}");
            assert!(
                err.contains("1 sync history file(s) could not be put back"),
                "{err}"
            );
        }
    }
}
