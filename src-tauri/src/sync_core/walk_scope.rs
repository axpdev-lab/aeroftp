//! The rule every recursive remote walk applies before it lists a folder that a
//! listing named: the folder must sit strictly under the walk root, must not
//! point back at the folder being listed or above it, and must not have been
//! listed already.
//!
//! A walk lists the path a provider's listing gives for each child folder
//! (`RemoteEntry::path`), and nothing in the provider contract stops a
//! defective listing from naming a path that is not below the folder listed.
//! Two did. ImageKit gave every folder the path `/`, so each child sent the
//! walk back to the account root: the shared walker never finished, and a
//! recursive delete that followed it deleted root files. Swift listed every
//! folder inside itself (its own `prefix/` marker came back as a child with the
//! folder's own path), so a walk went down the same folder until its depth cap,
//! collecting the same files once per level; a dedupe over that list found
//! each file to be a duplicate of itself and deleted the only copy.
//!
//! [`WalkScope`] is the one implementation of that rule. The shared scanner,
//! the AeroAgent and MCP tools and the CLI walkers all ask it before they queue
//! a folder, and a provider whose recursive delete lists a folder and deletes
//! what the listing names asks [`listing_to_remove`] before it deletes any of
//! it.
//!
//! # How paths are compared
//!
//! Providers do not spell paths alike, so two forms are used:
//!
//! - the *identity* of a path ([`walk_path_key`]) decides whether a folder was
//!   already listed and whether two listed files are the same file: a leading
//!   `/` added (GitHub returns repo-relative paths without one), repeated
//!   slashes collapsed, the trailing slash dropped, `.` and `..` resolved.
//!   Case is kept: `Docs` and `docs` are two folders on SFTP.
//! - the *position* of a path, used for "under the root" and "the folder being
//!   listed or above it", additionally reads `\` as `/` and ignores case, so a
//!   case-insensitive provider (Dropbox only promises the case of the last
//!   component) or a provider that rewrites `\` is not refused for spelling
//!   the root differently from the caller. Ignoring case there only ever lets a
//!   folder through that a strict comparison would have refused.
//!
//! Collapsing slashes and resolving dots can make two distinct keys of an
//! opaque-key store (S3, Swift: `a//b` and `a/b`, `x/../y` and `y`) read as one
//! folder. That errs on the side that loses nothing: the second one is not
//! listed (and a scan reports it), and no file is ever counted twice.
//!
//! # The root as the provider spells it
//!
//! A caller may name the root in a form the provider resolves against a base
//! the walk does not know: `foo`, `~/foo` or `""` on SFTP become
//! `/home/u/foo`; GitLab joins even an absolute path to its current path. The
//! children of such a root are then spelled under that base, not under the
//! caller's string. The root's own listing says how the provider spells it:
//! every entry whose path ends in its own name names its parent, which is the
//! root. [`WalkScope::learn_root_listing`] adopts such a parent as another
//! spelling of the root when it agrees with what the caller asked for (it is
//! that path, or ends with it, case aside, a leading namespace such as
//! Yandex's `disk:` set aside; any parent for `/`, `~` or `disk:/`). A
//! folder that listed itself does not teach anything: its parent is the
//! root's parent, which never ends with the root.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use crate::providers::{ProviderError, RemoteEntry};
use std::collections::HashSet;

/// How many spellings of the root a walk adopts from the root's listing. A
/// tree listing names one parent for all its entries; a listing that names
/// more than a handful is inconsistent, and adopting all of them would only
/// make every check slower.
pub(crate) const MAX_ROOT_SPELLINGS: usize = 8;

/// What a walk does with a folder a listing named.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalkStep {
    /// Strictly under the walk root and not listed yet: list it.
    Follow,
    /// Strictly under the walk root and already listed (or queued): it is not
    /// listed again, so nothing under it is collected twice.
    AlreadyListed,
    /// The folder being listed, one of its ancestors, the walk root or one of
    /// the root's ancestors: following it would walk the same tree again, or
    /// a bigger one, without end.
    LoopsBack,
    /// Neither under the walk root nor above it: following it would walk a
    /// part of the account the caller did not ask for.
    OutsideRoot,
}

impl WalkStep {
    /// The reason a walk records for a folder it does not follow.
    pub fn reason(self) -> &'static str {
        match self {
            WalkStep::Follow => "followed",
            WalkStep::AlreadyListed => "already_listed",
            WalkStep::LoopsBack => "loops_back",
            WalkStep::OutsideRoot => "outside_root",
        }
    }

    /// The same reason in words, to follow "the listing of X names it, but".
    pub fn explain(self) -> &'static str {
        match self {
            WalkStep::Follow => "it is followed",
            WalkStep::AlreadyListed => "it was already listed",
            WalkStep::LoopsBack => "it is that folder or one above it",
            WalkStep::OutsideRoot => "it is outside the path being walked",
        }
    }
}

/// The walk root, the folders a walk has listed, and the rule that decides
/// whether it lists the next one. See the module documentation.
#[derive(Debug, Clone)]
pub struct WalkScope {
    /// Positions (see [`position_key`]) of the spellings of the root: the
    /// caller's, then those adopted from the root's listing.
    spellings: Vec<String>,
    /// The lower-cased segments an adopted spelling must end with: the
    /// caller's root without a leading `~` or namespace (`disk:`). Empty for
    /// `/`, `~` and `disk:/`, which any spelling agrees with.
    suffix: String,
    /// Identities ([`walk_path_key`]) of every folder listed or queued,
    /// spellings of the root included.
    visited: HashSet<String>,
    /// Identities of the spellings of the root, letter case kept: what a
    /// recursive delete compares (see [`WalkScope::holds_as_spelled`]).
    identities: Vec<String>,
    /// An absolute root already names its authority. A listing may teach a
    /// read another spelling, but cannot authorize deleting outside this one.
    absolute_delete_root: Option<String>,
}

impl WalkScope {
    /// A walk of `root`, which counts as listed.
    pub fn new(root: &str) -> Self {
        let position = position_key(root);
        let agreement = agreement_key(root);
        let mut segments = agreement.split('/').filter(|segment| !segment.is_empty());
        let mut kept: Vec<&str> = Vec::new();
        if let Some(first) = segments.next() {
            // `~` and a namespace such as Yandex's `disk:` name where the
            // root stands, not a folder its children are spelled under.
            if first != "~" && !first.ends_with(':') {
                kept.push(first);
            }
        }
        kept.extend(segments);
        Self {
            suffix: kept.join("/"),
            spellings: vec![position],
            visited: HashSet::from([walk_path_key(root)]),
            identities: vec![walk_path_key(root)],
            absolute_delete_root: (root.starts_with(['/', '\\'])
                || (root.as_bytes().get(1) == Some(&b':')
                    && matches!(root.as_bytes().get(2), Some(b'/' | b'\\'))))
            .then(|| walk_path_key(&root.replace('\\', "/"))),
        }
    }

    /// A destructive walk may use the provider's explicit resolution of the
    /// requested root (for example GitLab's repository base or Dropbox's
    /// canonical case). That answer describes the requested root itself,
    /// rather than letting an arbitrary child listing expand its authority.
    /// Repo-relative answers are rooted before becoming deletion authority.
    pub fn for_delete(root: &str, resolved_root: Option<&str>) -> Self {
        let requested = Self::new(root);
        match resolved_root.filter(|path| !path.is_empty()) {
            Some(path) => {
                let agreement = agreement_key(path);
                let suffix = &requested.suffix;
                if suffix.is_empty()
                    || agreement == format!("/{suffix}")
                    || agreement.ends_with(&format!("/{suffix}"))
                {
                    Self::new(&walk_path_key(&path.replace('\\', "/")))
                } else {
                    requested
                }
            }
            None => requested,
        }
    }

    /// Adopt the spellings of the root that its own listing names (see the
    /// module documentation). Call it with the root's listing before any
    /// folder of that listing is admitted.
    pub fn learn_root_listing<'a, I>(&mut self, entries: I)
    where
        I: IntoIterator<Item = &'a RemoteEntry>,
    {
        for entry in entries {
            if let Some(parent) = listed_parent(&entry.name, &entry.path) {
                self.learn_root_spelling(&parent);
            }
        }
    }

    /// Adopt `parent`, the parent a root listing names for one of its entries
    /// (see [`listed_parent`]), as a spelling of the root if it agrees with
    /// the root the caller asked for.
    pub fn learn_root_spelling(&mut self, parent: &str) {
        let position = position_key(parent);
        let identity = walk_path_key(parent);
        let known = self.spellings.contains(&position);
        if (known && self.identities.contains(&identity))
            || self.spellings.len().max(self.identities.len()) >= MAX_ROOT_SPELLINGS
        {
            return;
        }
        let agreement = agreement_key(parent);
        let agrees = self.suffix.is_empty()
            || agreement == format!("/{}", self.suffix)
            || agreement.ends_with(&format!("/{}", self.suffix));
        if agrees {
            if !known {
                self.spellings.push(position);
            }
            if !self.identities.contains(&identity) {
                self.identities.push(identity.clone());
            }
            self.visited.insert(identity);
        }
    }

    /// Whether a path is the root itself, under any exact known spelling.
    /// Self entries are not descendants and need no further scan.
    pub fn is_root(&self, path: &str) -> bool {
        self.identities.contains(&walk_path_key(path))
    }

    /// A well-formed self entry or object marker for this scope's root.
    /// An alias with another name still reaches the walk's refusal guards.
    pub fn is_root_entry(&self, entry: &RemoteEntry) -> bool {
        if !self.is_root(&entry.path) {
            return false;
        }
        let identity = walk_path_key(&entry.path);
        let name = entry.name.trim_end_matches('/');
        name.is_empty() || name == "." || identity.rsplit('/').next() == Some(name)
    }

    /// Map a strict descendant to the local name under this walk's root.
    /// Prefer the longest exact root spelling learned from the root listing,
    /// preserving the case and literal characters of every child component.
    /// This does not expand a walk's authority or change deletion guards.
    pub fn relative_path(&self, path: &str) -> Option<String> {
        let path = walk_path_key(path);
        if self.identities.contains(&path) {
            return None;
        }
        self.identities
            .iter()
            .filter_map(|root| {
                let relative = if root == "/" {
                    path.strip_prefix('/')
                } else {
                    path.strip_prefix(root)
                        .and_then(|tail| tail.strip_prefix('/'))
                }?;
                (!relative.is_empty()).then_some((root.len(), relative))
            })
            .max_by_key(|(length, _)| *length)
            .map(|(_, relative)| relative.to_string())
    }

    /// Decide whether a walk lists `child`, a folder the listing of
    /// `listed_dir` named. [`WalkStep::Follow`] also records it as listed, so
    /// a later listing that names it again gets [`WalkStep::AlreadyListed`].
    pub fn admit(&mut self, listed_dir: &str, child: &str) -> WalkStep {
        match self.locate(listed_dir, child) {
            WalkStep::Follow if !self.visited.insert(walk_path_key(child)) => {
                WalkStep::AlreadyListed
            }
            step => step,
        }
    }

    /// Where `path`, an entry the listing of `listed_dir` named, stands: the
    /// answer [`WalkScope::admit`] gives, with nothing recorded and no
    /// [`WalkStep::AlreadyListed`]. For an entry a walk acts on without
    /// listing it, such as a file a recursive delete removes: it must be
    /// strictly under the root too, or the delete reaches outside the folder.
    pub fn locate(&self, listed_dir: &str, path: &str) -> WalkStep {
        let position = position_key(path);
        let listed = position_key(listed_dir);
        if position == listed || is_strictly_under(&listed, &position) {
            return WalkStep::LoopsBack;
        }
        if self
            .spellings
            .iter()
            .any(|root| position == *root || is_strictly_under(root, &position))
        {
            return WalkStep::LoopsBack;
        }
        if !self
            .spellings
            .iter()
            .any(|root| is_strictly_under(&position, root))
        {
            return WalkStep::OutsideRoot;
        }
        WalkStep::Follow
    }

    /// Where `path`, an entry the listing of `listed_dir` named, stands for a
    /// walk that lists the folders it `follows` and acts on everything else
    /// without listing it: [`WalkScope::admit`] for the first,
    /// [`WalkScope::locate`] for the rest. A file is held to the root as a
    /// folder is: a size that counted `../keep.txt`, a copy that fetched it
    /// or a cleanup that deleted it would reach outside the folder as surely
    /// as one that listed a folder there.
    pub fn place(&mut self, listed_dir: &str, path: &str, follows: bool) -> WalkStep {
        if follows {
            self.admit(listed_dir, path)
        } else {
            self.locate(listed_dir, path)
        }
    }

    /// [`WalkScope::place`] for a walk that deletes what it takes (a cleanup,
    /// a dedupe, an empty-folder prune): the entry must also sit under a
    /// spelling of the root with the letter case kept
    /// ([`WalkScope::holds_as_spelled`]), as a recursive delete requires
    /// ([`listing_to_remove`]). `place` ignores case so that a read does not
    /// refuse a case-insensitive provider; a delete of `/X/a` on a
    /// case-sensitive store, from a walk of `/x`, removes a file of another
    /// folder. An entry that fails only that test is [`WalkStep::OutsideRoot`].
    pub fn place_to_delete(&mut self, listed_dir: &str, path: &str, follows: bool) -> WalkStep {
        match self.place(listed_dir, path, follows) {
            WalkStep::Follow | WalkStep::AlreadyListed if !self.holds_as_spelled(path) => {
                WalkStep::OutsideRoot
            }
            step => step,
        }
    }

    /// `path` sits strictly under a spelling of the root with the letter case
    /// kept. [`WalkScope::locate`] ignores case, so that a walk does not
    /// refuse a case-insensitive provider for spelling the root its own way;
    /// a delete must not take that liberty, since on a case-sensitive store
    /// `/X/a` is not in `/x`.
    pub fn holds_as_spelled(&self, path: &str) -> bool {
        let identity = walk_path_key(path);
        if let Some(root) = &self.absolute_delete_root {
            return is_strictly_under(&identity, root);
        }
        self.identities
            .iter()
            .any(|root| is_strictly_under(&identity, root))
    }
}

/// The entries of `folder`'s listing that a recursive delete of `folder` may
/// act on: each path once, every one strictly under `folder`. A listing that
/// names an entry anywhere else (`folder` itself, one of its ancestors, or a
/// path outside it) is refused whole, before anything in it is deleted.
///
/// A provider whose recursive delete lists a folder and deletes what the
/// listing names deletes wherever the listing points. ImageKit named every
/// folder `/`: `rm -r` of a folder went back to the account root, deleted the
/// files there and never ended. Skipping such an entry would not do either: a
/// delete that left part of the folder behind and answered `Ok` is a partial
/// `rm -r` reported as a whole one, so the delete fails and says why.
///
/// An absolute `folder` is taken as the provider spells it, letter case
/// included, so a listing of it must name its entries under it. A relative
/// one is resolved by the provider against a base the caller does not know,
/// and its spelling is learned from its own listing as
/// [`WalkScope::learn_root_listing`] learns a walk root: a delete trusts a
/// listing only as far as it has to.
pub fn listing_to_remove(
    folder: &str,
    entries: Vec<RemoteEntry>,
) -> Result<Vec<RemoteEntry>, ProviderError> {
    let mut scope = WalkScope::new(folder);
    if !folder.starts_with('/') {
        scope.learn_root_listing(&entries);
    }
    let mut seen: HashSet<String> = HashSet::with_capacity(entries.len());
    let mut kept = Vec::with_capacity(entries.len());
    for entry in entries {
        // Named twice, deleted once: a second delete of the same path fails
        // on a path that is already gone.
        if !seen.insert(walk_path_key(&entry.path)) {
            continue;
        }
        match scope.locate(folder, &entry.path) {
            WalkStep::Follow | WalkStep::AlreadyListed if scope.holds_as_spelled(&entry.path) => {
                kept.push(entry)
            }
            WalkStep::Follow | WalkStep::AlreadyListed => {
                return Err(refused_removal(folder, &entry.path, WalkStep::OutsideRoot))
            }
            step => return Err(refused_removal(folder, &entry.path, step)),
        }
    }
    Ok(kept)
}

/// The error of a recursive delete of `folder` whose listing named `path`,
/// which is not inside it (see [`listing_to_remove`]).
pub fn refused_removal(folder: &str, path: &str, step: WalkStep) -> ProviderError {
    let why = match step {
        WalkStep::OutsideRoot => "outside it",
        _ => "that folder or one above it",
    };
    ProviderError::ServerError(format!(
        "refusing to delete {folder} recursively: its listing names {path:?}, which is {why} \
         ({}); nothing listed there was deleted",
        step.reason()
    ))
}

/// The keys of a flat prefix listing that a recursive delete or move of the
/// folder `prefix` acts on: each key once, every one starting with `prefix`.
/// Object stores (S3, B2, Swift, a GitLab tree) name objects by literal keys,
/// so the comparison is literal, case and dots included: `a/../b` listed
/// under `a/` is a key of `a/`, as the store keeps it.
///
/// The store filters the listing by the prefix, and nothing else checked
/// that it did. A key outside the prefix is refused whole, before anything
/// is acted on: a delete would remove an object of another folder, and a
/// move, whose new key replaces the prefix, would copy that object onto
/// itself and then delete the only copy. `action` names what was refused
/// (`delete`, `move`).
pub fn keys_under_prefix(
    prefix: &str,
    keys: Vec<String>,
    action: &str,
) -> Result<Vec<String>, ProviderError> {
    if let Some(stray) = keys.iter().find(|key| !key.starts_with(prefix)) {
        return Err(ProviderError::ServerError(format!(
            "refusing to {action} {prefix:?}: its listing names the key {stray:?}, which is \
             outside it ({}); nothing listed there was changed",
            WalkStep::OutsideRoot.reason()
        )));
    }
    let mut seen: HashSet<String> = HashSet::with_capacity(keys.len());
    Ok(keys
        .into_iter()
        .filter(|key| seen.insert(key.clone()))
        .collect())
}

/// A provider's own recursive `find`: the walk scope of its root, and the
/// paths it has already reported. A `find` that followed whatever folder a
/// listing named searched the account root under ImageKit and never ended,
/// and reported each file of a Swift folder once per level.
#[derive(Debug, Clone)]
pub struct FindWalk {
    scope: WalkScope,
    root_listed: bool,
    reported: HashSet<String>,
}

/// One listing of a [`FindWalk`], sorted.
#[derive(Debug, Default)]
pub struct FoundListing {
    /// The entries the search may report: each path once over the whole
    /// walk, without the folders it refused.
    pub entries: Vec<RemoteEntry>,
    /// The folders to list next, each once over the whole walk.
    pub follow: Vec<String>,
    /// The entries the listing named outside the root, or on the folder
    /// listed or above it, with the reason: the search did not go there and
    /// does not report them, so it did not see the whole tree.
    pub refused: Vec<(String, WalkStep)>,
}

impl FindWalk {
    /// A search of `root`. Its first listing must be the root's, so that a
    /// root the provider spells differently is learned from it.
    pub fn new(root: &str) -> Self {
        Self {
            scope: WalkScope::new(root),
            root_listed: false,
            reported: HashSet::new(),
        }
    }

    /// Sort the listing of `listed`. Only folders that are not symlinks are
    /// followed ([`RemoteEntry::is_walkable_dir`]), as before; any other
    /// entry is reported only where a folder would be followed
    /// ([`WalkScope::place`]).
    pub fn sort(&mut self, listed: &str, entries: Vec<RemoteEntry>) -> FoundListing {
        if !self.root_listed {
            self.scope.learn_root_listing(&entries);
            self.root_listed = true;
        }
        let mut sorted = FoundListing::default();
        for entry in entries {
            let follows = entry.is_walkable_dir();
            match self.scope.place(listed, &entry.path, follows) {
                WalkStep::Follow if follows => sorted.follow.push(entry.path.clone()),
                WalkStep::Follow | WalkStep::AlreadyListed => {}
                step => {
                    sorted.refused.push((entry.path.clone(), step));
                    continue;
                }
            }
            if self.reported.insert(walk_path_key(&entry.path)) {
                sorted.entries.push(entry);
            }
        }
        sorted
    }
}

/// The error of a provider `find` under `root` whose listing of `listed`
/// named `path` outside the root or back on itself, for a `find` that fails
/// when a folder cannot be listed.
pub fn refused_search(root: &str, listed: &str, path: &str, step: WalkStep) -> ProviderError {
    ProviderError::ServerError(format!(
        "cannot search {root}: the listing of {listed} names {path:?}, which is \
         outside the folder searched or back on it ({})",
        step.reason()
    ))
}

/// The error of a recursive copy or download of `folder` whose listing of
/// `listed` named `path`, which the walk does not follow ([`WalkScope`]). A
/// transfer that skipped it would leave part of the folder behind and report
/// success; one that followed it would copy what is not in the folder.
pub fn refused_transfer(folder: &str, listed: &str, path: &str, step: WalkStep) -> String {
    format!(
        "cannot transfer {folder} whole: the listing of {listed} names {path:?}, but {} ({})",
        step.explain(),
        step.reason()
    )
}

/// The identity of a remote path for a walk: rooted, repeated slashes
/// collapsed, no trailing slash, `.` and `..` resolved (`..` stops at the
/// root). `""`, `.` and `/` are all `/`. Two listed paths with the same
/// identity are the same folder or the same file to a walk, which is what a
/// dedupe needs to never take a file for a duplicate of itself.
pub fn walk_path_key(path: &str) -> String {
    let mut segments: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other),
        }
    }
    if segments.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", segments.join("/"))
    }
}

/// Keep the first of the items that share a path identity ([`walk_path_key`])
/// and drop the rest; returns how many were dropped. A walk lists each folder
/// once, so a path met twice comes from a defective listing; a dedupe that
/// kept both would group a file with itself and delete it as its own
/// duplicate, which removes the only copy.
pub fn retain_first_per_path<T>(items: &mut Vec<T>, path: impl Fn(&T) -> &str) -> usize {
    let before = items.len();
    let mut seen: HashSet<String> = HashSet::with_capacity(before);
    items.retain(|item| seen.insert(walk_path_key(path(item))));
    before - items.len()
}

/// The parent a listing entry names: the entry's path without its last
/// segment, when that segment is the entry's own name. An entry whose path
/// does not end in its name (ImageKit's `/` for every folder) names nothing.
pub fn listed_parent(name: &str, path: &str) -> Option<String> {
    let identity = walk_path_key(path);
    let (parent, last) = identity.rsplit_once('/')?;
    if last.is_empty() || last != name {
        return None;
    }
    Some(if parent.is_empty() {
        "/".to_string()
    } else {
        parent.to_string()
    })
}

/// The position of a path, compared for "under the root" and "the folder
/// being listed or above it": its identity with `\` read as `/`, lower-cased.
fn position_key(path: &str) -> String {
    walk_path_key(&path.replace('\\', "/")).to_lowercase()
}

/// The form two spellings of the root are compared in: the position, with
/// the spaces around each segment set aside, since several providers trim
/// the path they are given before they list it.
fn agreement_key(path: &str) -> String {
    let position = position_key(path);
    let segments: Vec<&str> = position
        .split('/')
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .collect();
    format!("/{}", segments.join("/"))
}

/// `child` sits below `ancestor`, both as [`position_key`] returns them.
fn is_strictly_under(child: &str, ancestor: &str) -> bool {
    if ancestor == "/" {
        return child != "/";
    }
    child.len() > ancestor.len()
        && child.starts_with(ancestor)
        && child.as_bytes()[ancestor.len()] == b'/'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str, path: &str) -> RemoteEntry {
        RemoteEntry::directory(name.to_string(), path.to_string())
    }

    fn file(name: &str, path: &str) -> RemoteEntry {
        RemoteEntry::file(name.to_string(), path.to_string(), 1)
    }

    #[test]
    fn a_path_identity_is_rooted_collapsed_and_resolved() {
        for (path, identity) in [
            ("", "/"),
            (".", "/"),
            ("/", "/"),
            ("//", "/"),
            ("dir/a", "/dir/a"),
            ("/dir/a/", "/dir/a"),
            ("/dir//a", "/dir/a"),
            ("/dir/./a", "/dir/a"),
            ("/dir/x/../a", "/dir/a"),
            ("/../..", "/"),
            ("/Docs", "/Docs"),
        ] {
            assert_eq!(walk_path_key(path), identity, "{path:?}");
        }
    }

    /// ImageKit's old listing: every folder with the path `/`. Refused under
    /// any root, as the root itself or one of its ancestors.
    #[test]
    fn a_folder_spelled_as_the_account_root_loops_back() {
        let mut at_root = WalkScope::new("/");
        assert_eq!(at_root.admit("/", "/"), WalkStep::LoopsBack);
        let mut below = WalkScope::new("/photos");
        assert_eq!(below.admit("/photos", "/"), WalkStep::LoopsBack);
        assert_eq!(below.admit("/photos/2024", "/"), WalkStep::LoopsBack);
        assert_eq!(below.admit("/photos/2024", "/photos"), WalkStep::LoopsBack);
    }

    /// Swift's listing of a folder named the folder itself. Refused whether
    /// the folder is the root or below it, in whatever case the path comes.
    #[test]
    fn a_folder_listed_inside_itself_loops_back() {
        let mut scope = WalkScope::new("/");
        assert_eq!(scope.admit("/", "/x"), WalkStep::Follow);
        assert_eq!(scope.admit("/x", "/x"), WalkStep::LoopsBack);
        assert_eq!(scope.admit("/x", "/X/"), WalkStep::LoopsBack);
        let mut at = WalkScope::new("/x");
        assert_eq!(at.admit("/x", "/x"), WalkStep::LoopsBack);
    }

    #[test]
    fn a_folder_outside_the_root_is_refused_and_one_inside_is_listed_once() {
        let mut scope = WalkScope::new("/root");
        assert_eq!(scope.admit("/root", "/elsewhere"), WalkStep::OutsideRoot);
        assert_eq!(scope.admit("/root", "/rooted"), WalkStep::OutsideRoot);
        assert_eq!(scope.admit("/root/a", "/root/b/c"), WalkStep::Follow);
        assert_eq!(
            scope.admit("/root/b", "/root/b/c/"),
            WalkStep::AlreadyListed
        );
        assert_eq!(scope.admit("/root", "/root/a"), WalkStep::Follow);
        assert_eq!(scope.admit("/root", "root/a"), WalkStep::AlreadyListed);
    }

    /// Case is kept in the identity: two folders that differ only in case are
    /// two folders, and both are listed.
    #[test]
    fn folders_that_differ_only_in_case_are_both_listed() {
        let mut scope = WalkScope::new("/");
        assert_eq!(scope.admit("/", "/Docs"), WalkStep::Follow);
        assert_eq!(scope.admit("/", "/docs"), WalkStep::Follow);
    }

    #[test]
    fn relative_names_use_the_longest_known_root_without_prefix_bleed() {
        for (root, spelled) in [
            ("docs", "/home/u/docs"),
            ("/Docs", "/docs"),
            ("disk:/docs", "/docs"),
            ("/docs ", "/docs"),
            ("/", "/home/u"),
        ] {
            let mut scope = WalkScope::new(root);
            scope.learn_root_listing(&[file("a.txt", &format!("{spelled}/a.txt"))]);
            assert_eq!(
                scope
                    .relative_path(&format!("{spelled}/Sub/back\\slash.txt"))
                    .as_deref(),
                Some("Sub/back\\slash.txt"),
                "{root}"
            );
            assert_eq!(
                scope.relative_path(spelled),
                None,
                "the root is not its own child"
            );
            assert!(scope.is_root(root));
            assert!(scope.is_root(&format!("{spelled}/")));
            assert!(!scope.is_root(&format!("{spelled}/a.txt")));
            assert!(!scope.is_root(&format!("{spelled}-other")));
            let name = spelled.rsplit('/').next().unwrap();
            assert!(scope.is_root_entry(&file(name, &format!("{spelled}/"))));
            assert!(scope.is_root_entry(&file("", &format!("{spelled}/"))));
            assert!(!scope.is_root_entry(&file("different-name", spelled)));
            if root != "/" {
                assert_eq!(scope.relative_path(&format!("{spelled}-other/a.txt")), None);
            }
        }
    }

    /// GitHub returns repo-relative paths, with no leading `/`.
    #[test]
    fn repo_relative_children_are_under_their_root() {
        let mut scope = WalkScope::new("/src");
        assert_eq!(scope.admit("/src", "src/lib"), WalkStep::Follow);
        assert_eq!(scope.admit("src/lib", "src/lib/a"), WalkStep::Follow);
        let mut relative = WalkScope::new("src");
        assert_eq!(relative.admit("src", "src/lib"), WalkStep::Follow);
    }

    /// SFTP resolves a relative root against its working directory and lists
    /// absolute children. The root's listing says how it spells the root.
    #[test]
    fn a_relative_root_takes_the_spelling_its_listing_names() {
        let listing = [
            file("a.txt", "/home/u/www/a.txt"),
            dir("css", "/home/u/www/css"),
        ];
        for root in ["www", "./www", "~/www"] {
            let mut unlearned = WalkScope::new(root);
            assert_eq!(
                unlearned.admit(root, "/home/u/www/css"),
                WalkStep::OutsideRoot,
                "{root}: the caller's spelling alone does not hold the children"
            );
            let mut scope = WalkScope::new(root);
            scope.learn_root_listing(&listing);
            assert_eq!(
                scope.admit(root, "/home/u/www/css"),
                WalkStep::Follow,
                "{root}"
            );
            assert_eq!(
                scope.admit("/home/u/www/css", "/home/u/www/css/img"),
                WalkStep::Follow,
                "{root}"
            );
            assert_eq!(
                scope.admit("/home/u/www/css", "/home/u/www"),
                WalkStep::LoopsBack,
                "{root}: the root in the provider's spelling"
            );
            assert_eq!(
                scope.admit("/home/u/www/css", "/home/u"),
                WalkStep::LoopsBack,
                "{root}"
            );
            assert_eq!(
                scope.admit("/home/u/www/css", "/home/v/x"),
                WalkStep::OutsideRoot,
                "{root}"
            );
        }
        let mut home = WalkScope::new("~");
        home.learn_root_listing(&[dir("www", "/home/u/www")]);
        assert_eq!(home.admit("~", "/home/u/www"), WalkStep::Follow);
    }

    /// GitLab joins even an absolute path to its current path, and a
    /// case-insensitive provider may spell the root in another case.
    #[test]
    fn a_root_spelled_under_a_base_or_in_another_case_is_learned() {
        let mut gitlab = WalkScope::new("/data");
        gitlab.learn_root_listing(&[dir("x", "base/data/x")]);
        assert_eq!(gitlab.admit("/data", "base/data/x"), WalkStep::Follow);

        let mut dropbox = WalkScope::new("/photos");
        assert_eq!(dropbox.admit("/photos", "/Photos/2024"), WalkStep::Follow);
        dropbox.learn_root_listing(&[dir("2024", "/Photos/2024")]);
        assert_eq!(
            dropbox.admit("/Photos/2024", "/PHOTOS/2024/a"),
            WalkStep::Follow
        );

        let mut backslashes = WalkScope::new("\\Documents\\Photos");
        backslashes.learn_root_listing(&[dir("x", "/Documents/Photos/x")]);
        assert_eq!(
            backslashes.admit("\\Documents\\Photos", "/Documents/Photos/x"),
            WalkStep::Follow
        );
    }

    /// A folder that lists itself names the root's parent, which never ends
    /// with the root: nothing is learned from it, and the root does not grow.
    #[test]
    fn a_self_entry_teaches_nothing_about_the_root() {
        let mut scope = WalkScope::new("/x");
        scope.learn_root_listing(&[dir("x", "/x"), file("a.txt", "/x/a.txt")]);
        assert_eq!(scope.admit("/x", "/x"), WalkStep::LoopsBack);
        assert_eq!(scope.admit("/x", "/y"), WalkStep::OutsideRoot);
        assert_eq!(scope.admit("/x", "/x/z"), WalkStep::Follow);

        let mut nested = WalkScope::new("/a/b");
        nested.learn_root_listing(&[dir("b", "/a/b")]);
        assert_eq!(nested.admit("/a/b", "/a/c"), WalkStep::OutsideRoot);

        // ImageKit's `/` names no parent at all.
        assert_eq!(listed_parent("photos", "/"), None);
        assert_eq!(listed_parent("a.txt", "/x/a.txt").as_deref(), Some("/x"));
        assert_eq!(listed_parent("a.txt", "/a.txt").as_deref(), Some("/"));
    }

    #[test]
    fn a_path_met_twice_is_kept_once() {
        let mut files = vec![
            ("/a/x.txt", 1),
            ("a/x.txt", 2),
            ("/a//x.txt/", 3),
            ("/a/X.txt", 4),
            ("/b/x.txt", 5),
        ];
        assert_eq!(retain_first_per_path(&mut files, |file| file.0), 2);
        assert_eq!(files, [("/a/x.txt", 1), ("/a/X.txt", 4), ("/b/x.txt", 5)]);
    }

    /// The default roots: everything under the account root is under them,
    /// and the root's own listing adds the spelling a provider uses for its
    /// home, so that spelling coming back as a child is refused too.
    #[test]
    fn the_default_roots_learn_the_home_spelling() {
        for root in ["", ".", "/"] {
            let mut scope = WalkScope::new(root);
            scope.learn_root_listing(&[dir("a", "/home/u/a")]);
            assert_eq!(scope.admit(root, "/home/u/a"), WalkStep::Follow, "{root:?}");
            assert_eq!(
                scope.admit(root, "/home/u"),
                WalkStep::LoopsBack,
                "{root:?}"
            );
            assert_eq!(scope.admit(root, "/"), WalkStep::LoopsBack, "{root:?}");
        }
    }

    /// A root may name its namespace first: Yandex takes `disk:/x` and lists
    /// the children of that folder as `/x/...`. `disk:` is not a folder, so
    /// the root's listing teaches the plain spelling, and `disk:/` agrees with
    /// any parent, as `/` does.
    #[test]
    fn a_root_that_names_its_namespace_learns_the_plain_spelling() {
        let mut folder = WalkScope::new("disk:/x");
        folder.learn_root_listing(&[dir("c", "/x/c")]);
        assert_eq!(folder.admit("disk:/x", "/x/c"), WalkStep::Follow);
        assert_eq!(folder.admit("disk:/x", "/x"), WalkStep::LoopsBack);
        assert_eq!(folder.admit("disk:/x", "/y"), WalkStep::OutsideRoot);

        let mut account = WalkScope::new("disk:/");
        account.learn_root_listing(&[dir("x", "/x")]);
        assert_eq!(account.admit("disk:/", "/x"), WalkStep::Follow);
    }

    fn paths(entries: &[RemoteEntry]) -> Vec<&str> {
        entries.iter().map(|entry| entry.path.as_str()).collect()
    }

    /// A recursive delete acts on a listing only when everything in it is
    /// inside the folder: a folder spelled `/` (ImageKit), the folder listed
    /// inside itself as a folder or as a file (Swift's marker), its parent,
    /// another folder and a file of another folder each refuse the listing
    /// whole. A path named twice is acted on once.
    #[test]
    fn a_removal_listing_holds_only_what_is_inside_the_folder() {
        let kept = listing_to_remove(
            "/x",
            vec![
                file("a.txt", "/x/a.txt"),
                dir("d", "/x/d"),
                file("a.txt", "/x//a.txt/"),
                dir("deep", "/x/d/deep"),
            ],
        )
        .expect("everything is inside /x");
        assert_eq!(paths(&kept), ["/x/a.txt", "/x/d", "/x/d/deep"]);

        for (stray, reason) in [
            (dir("d1", "/"), "loops_back"),
            (dir("x", "/x"), "loops_back"),
            (file("x", "/x/"), "loops_back"),
            (dir("up", "/x/d/../.."), "loops_back"),
            (dir("y", "/y"), "outside_root"),
            (file("b.txt", "/y/b.txt"), "outside_root"),
            (file("x2", "/x2/a"), "outside_root"),
        ] {
            let path = stray.path.clone();
            let err = listing_to_remove("/x", vec![file("a.txt", "/x/a.txt"), stray])
                .expect_err(&format!("{path} is not inside /x"));
            let message = err.to_string();
            assert!(message.contains(reason), "{path}: {message}");
            assert!(message.contains("/x"), "{path}: {message}");
        }
    }

    /// A relative folder is resolved by the provider against a base the
    /// caller does not know, so its spelling is learned from its listing,
    /// letter case included. An absolute one is not: a listing of `/x` that
    /// names `/base/x/a`, or `/X/a`, names a path outside it. Repo-relative
    /// children of an absolute folder (GitHub) are inside it.
    #[test]
    fn a_removal_listing_learns_the_spelling_of_a_relative_folder_only() {
        let kept = listing_to_remove(
            "www",
            vec![file("a", "/home/u/www/a"), dir("css", "/home/u/www/css")],
        )
        .expect("the listing names how the provider spells www");
        assert_eq!(kept.len(), 2);
        assert!(listing_to_remove("www", vec![dir("v", "/home/u/v")]).is_err());

        let err = listing_to_remove("/x", vec![file("a", "/base/x/a")]).unwrap_err();
        assert!(err.to_string().contains("outside_root"), "{err}");

        assert_eq!(
            listing_to_remove("/src", vec![file("a.rs", "src/a.rs")])
                .unwrap()
                .len(),
            1
        );
        let err = listing_to_remove("/photos", vec![file("a.jpg", "/Photos/a.jpg")]).unwrap_err();
        assert!(err.to_string().contains("outside_root"), "{err}");
        assert_eq!(
            listing_to_remove("docs", vec![file("a", "/home/u/Docs/a")])
                .unwrap()
                .len(),
            1
        );
    }

    /// A walk lists the folders it follows and acts on the rest without
    /// listing them: a file outside the root, or on the folder listed or
    /// above it, is refused as such a folder is, and a file is never
    /// recorded as listed.
    #[test]
    fn a_file_is_held_to_the_root_as_a_folder_is() {
        let mut scope = WalkScope::new("/root");
        assert_eq!(scope.place("/root", "/root/a.txt", false), WalkStep::Follow);
        assert_eq!(scope.place("/root", "/root/a.txt", false), WalkStep::Follow);
        assert_eq!(
            scope.place("/root", "/root/../keep.txt", false),
            WalkStep::OutsideRoot
        );
        assert_eq!(
            scope.place("/root", "/keep.txt", false),
            WalkStep::OutsideRoot
        );
        assert_eq!(
            scope.place("/root/d", "/root/d", false),
            WalkStep::LoopsBack
        );
        assert_eq!(scope.place("/root/d", "/root", false), WalkStep::LoopsBack);
        assert_eq!(scope.place("/root", "/root/d", true), WalkStep::Follow);
        assert_eq!(
            scope.place("/root", "/root/d", true),
            WalkStep::AlreadyListed
        );
        assert_eq!(
            scope.place("/root", "/elsewhere", true),
            WalkStep::OutsideRoot
        );
    }

    /// A walk that deletes takes an entry only where its path keeps the case
    /// of a spelling of the root, the root's listing included: a read takes
    /// `/X/d/a` in a walk of `/x`, a delete refuses it.
    #[test]
    fn a_delete_walk_takes_only_what_is_under_the_root_as_spelled() {
        let mut scope = WalkScope::new("/x");
        assert_eq!(scope.place("/x/d", "/X/d/a", false), WalkStep::Follow);
        assert_eq!(
            scope.place_to_delete("/x/d", "/X/d/a", false),
            WalkStep::OutsideRoot
        );
        assert_eq!(
            scope.place_to_delete("/x/d", "/X/d/e", true),
            WalkStep::OutsideRoot
        );
        assert_eq!(
            scope.place_to_delete("/x/d", "/x/D/a", false),
            WalkStep::Follow
        );
        assert_eq!(scope.place_to_delete("/x", "/x/d", true), WalkStep::Follow);
        assert_eq!(
            scope.place_to_delete("/x", "/x/d", true),
            WalkStep::AlreadyListed
        );
        assert_eq!(
            scope.place_to_delete("/x", "/keep", false),
            WalkStep::OutsideRoot
        );

        let mut learned = WalkScope::new("/x");
        learned.learn_root_listing(&[file("a", "/X/a")]);
        assert_eq!(
            learned.place_to_delete("/x", "/X/a", false),
            WalkStep::OutsideRoot
        );
    }

    #[test]
    fn deletion_resolution_cannot_replace_the_requested_root_with_an_ancestor() {
        for resolved in ["/", "/elsewhere", "/photo"] {
            let mut scope = WalkScope::for_delete("/photos", Some(resolved));
            assert_eq!(
                scope.place_to_delete("/photos", "/elsewhere/a", false),
                WalkStep::OutsideRoot
            );
            assert_eq!(
                scope.place_to_delete("/photos", "/photos/a", false),
                WalkStep::Follow
            );
        }
    }

    #[test]
    fn backslash_absolute_delete_roots_keep_their_requested_authority() {
        let mut scope = WalkScope::for_delete("\\Documents\\Photos", None);
        assert_eq!(
            scope.place_to_delete("\\Documents\\Photos", "/Documents/Photos/x", false),
            WalkStep::Follow
        );
        assert_eq!(
            scope.place_to_delete("\\Documents\\Photos", "/elsewhere/x", false),
            WalkStep::OutsideRoot
        );
    }

    #[test]
    fn listing_aliases_never_expand_absolute_deletion_authority() {
        let mut scope = WalkScope::new("/root");
        scope.learn_root_listing(&[file("x.aerotmp", "/outside/root/x.aerotmp")]);
        assert_eq!(
            scope.place("/root", "/outside/root/x.aerotmp", false),
            WalkStep::Follow
        );
        assert_eq!(
            scope.place_to_delete("/root", "/outside/root/x.aerotmp", false),
            WalkStep::OutsideRoot
        );
        assert_eq!(
            scope.place_to_delete("/root", "/root/x.aerotmp", false),
            WalkStep::Follow
        );

        // A provider explicitly resolves the root's case/base, so its real
        // children remain usable without trusting another listing's aliases.
        let mut resolved = WalkScope::for_delete("/photos", Some("base/Photos"));
        resolved.learn_root_listing(&[file("a", "/elsewhere/base/Photos/a")]);
        assert_eq!(
            resolved.place_to_delete("/photos", "base/Photos/a", false),
            WalkStep::Follow
        );
        assert_eq!(
            resolved.place_to_delete("/photos", "/elsewhere/base/Photos/a", false),
            WalkStep::OutsideRoot
        );
        assert_eq!(
            resolved.place_to_delete("/photos", "base/photos/a", false),
            WalkStep::OutsideRoot
        );

        let mut relative = WalkScope::for_delete("www", None);
        relative.learn_root_listing(&[file("a", "/home/user/www/a")]);
        assert_eq!(
            relative.place_to_delete("www", "/home/user/www/a", false),
            WalkStep::Follow
        );
    }

    /// `locate` answers as `admit` does and records nothing.
    #[test]
    fn locating_an_entry_records_nothing() {
        let mut scope = WalkScope::new("/x");
        assert_eq!(scope.locate("/x", "/x/d"), WalkStep::Follow);
        assert_eq!(scope.locate("/x", "/x/d"), WalkStep::Follow);
        assert_eq!(scope.admit("/x", "/x/d"), WalkStep::Follow);
        assert_eq!(scope.admit("/x", "/x/d"), WalkStep::AlreadyListed);
        assert_eq!(scope.locate("/x", "/x/d"), WalkStep::Follow);
        assert_eq!(scope.locate("/x/d", "/x"), WalkStep::LoopsBack);
        assert_eq!(scope.locate("/x/d", "/y"), WalkStep::OutsideRoot);
    }

    /// A flat listing is acted on only when every key starts with the
    /// prefix, compared as the literal string the store keeps; a key named
    /// twice is acted on once.
    #[test]
    fn a_prefix_listing_holds_only_keys_under_the_prefix() {
        let keys = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
        assert_eq!(
            keys_under_prefix("x/", keys(&["x/", "x/a", "x/../b", "x/a"]), "delete").unwrap(),
            ["x/", "x/a", "x/../b"]
        );
        for stray in ["y/a", "X/a", "x", "/x/a", ""] {
            let err = keys_under_prefix("x/", keys(&["x/a", stray]), "move").unwrap_err();
            let message = err.to_string();
            assert!(
                message.contains("outside_root") && message.contains("move"),
                "{stray:?}: {message}"
            );
        }
        assert_eq!(
            keys_under_prefix("", keys(&["a", "b/c"]), "delete").unwrap(),
            ["a", "b/c"]
        );
    }

    /// A provider `find` follows each folder inside the root once, refuses a
    /// folder spelled `/`, the folder listed inside itself, and a folder or
    /// file outside the root, and reports each path once. A symlinked folder
    /// is reported and not followed.
    #[test]
    fn a_find_walk_stays_inside_its_root_and_reports_each_path_once() {
        let mut walk = FindWalk::new("/x");
        let mut link = dir("link", "/x/link");
        link.is_symlink = true;
        let root = walk.sort(
            "/x",
            vec![
                file("a.txt", "/x/a.txt"),
                dir("d", "/x/d"),
                dir("d", "/x/d/"),
                file("a.txt", "/x//a.txt"),
                dir("up", "/"),
                dir("y", "/y"),
                file("../keep.txt", "/keep.txt"),
                link,
            ],
        );
        assert_eq!(paths(&root.entries), ["/x/a.txt", "/x/d", "/x/link"]);
        assert_eq!(root.follow, ["/x/d"]);
        assert_eq!(
            root.refused,
            [
                ("/".to_string(), WalkStep::LoopsBack),
                ("/y".to_string(), WalkStep::OutsideRoot),
                ("/keep.txt".to_string(), WalkStep::OutsideRoot)
            ]
        );

        let below = walk.sort(
            "/x/d",
            vec![
                dir("d", "/x/d"),
                file("b", "/x/d/b"),
                file("a.txt", "/x/a.txt"),
            ],
        );
        assert_eq!(paths(&below.entries), ["/x/d/b"]);
        assert!(below.follow.is_empty());
        assert_eq!(below.refused, [("/x/d".to_string(), WalkStep::LoopsBack)]);

        let err = refused_search("/x", "/x/d", "/x/d", WalkStep::LoopsBack).to_string();
        assert!(err.contains("loops_back") && err.contains("/x/d"), "{err}");
    }

    /// A relative search root learns how the provider spells it from its
    /// first listing, so the walk does not refuse the whole tree.
    #[test]
    fn a_find_walk_of_a_relative_root_learns_its_spelling() {
        let mut walk = FindWalk::new("www");
        let root = walk.sort("www", vec![dir("css", "/home/u/www/css")]);
        assert_eq!(root.follow, ["/home/u/www/css"]);
        assert!(root.refused.is_empty());
    }

    /// Providers that clean the root before they list it: Koofr, Jottacloud,
    /// Drime, kDrive, Internxt, FileLu, Twake and 4shared trim the spaces
    /// around it, and 4shared drops a `..` segment instead of going up. The
    /// listing spells the cleaned root, which still agrees with the caller's.
    #[test]
    fn a_root_the_provider_cleans_before_listing_is_learned() {
        for root in ["/x ", " /x", "/x /", "x "] {
            let mut scope = WalkScope::new(root);
            scope.learn_root_listing(&[dir("c", "/x/c")]);
            assert_eq!(scope.admit(root, "/x/c"), WalkStep::Follow, "{root:?}");
            assert_eq!(scope.admit(root, "/y"), WalkStep::OutsideRoot, "{root:?}");
        }

        let mut dotted = WalkScope::new("/a/../b");
        dotted.learn_root_listing(&[dir("c", "/a/b/c")]);
        assert_eq!(dotted.admit("/a/../b", "/a/b/c"), WalkStep::Follow);
        assert_eq!(dotted.admit("/a/../b", "/a/c"), WalkStep::OutsideRoot);
    }
}
