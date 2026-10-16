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
//! a folder.
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
//! that path, or ends with it, case aside; any parent for `/` or `~`). A
//! folder that listed itself does not teach anything: its parent is the
//! root's parent, which never ends with the root.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use crate::providers::RemoteEntry;
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
}

/// The walk root, the folders a walk has listed, and the rule that decides
/// whether it lists the next one. See the module documentation.
#[derive(Debug, Clone)]
pub struct WalkScope {
    /// Positions (see [`position_key`]) of the spellings of the root: the
    /// caller's, then those adopted from the root's listing.
    spellings: Vec<String>,
    /// The lower-cased segments an adopted spelling must end with: the
    /// caller's root without a leading `~`. Empty for `/` and `~`, which any
    /// spelling agrees with.
    suffix: String,
    /// Identities ([`walk_path_key`]) of every folder listed or queued,
    /// spellings of the root included.
    visited: HashSet<String>,
}

impl WalkScope {
    /// A walk of `root`, which counts as listed.
    pub fn new(root: &str) -> Self {
        let position = position_key(root);
        let mut segments = position.split('/').filter(|segment| !segment.is_empty());
        let mut kept: Vec<&str> = Vec::new();
        if let Some(first) = segments.next() {
            if first != "~" {
                kept.push(first);
            }
        }
        kept.extend(segments);
        Self {
            suffix: kept.join("/"),
            spellings: vec![position],
            visited: HashSet::from([walk_path_key(root)]),
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
        if self.spellings.contains(&position) || self.spellings.len() >= MAX_ROOT_SPELLINGS {
            return;
        }
        let agrees = self.suffix.is_empty()
            || position == format!("/{}", self.suffix)
            || position.ends_with(&format!("/{}", self.suffix));
        if agrees {
            self.spellings.push(position);
            self.visited.insert(walk_path_key(parent));
        }
    }

    /// Decide whether a walk lists `child`, a folder the listing of
    /// `listed_dir` named. [`WalkStep::Follow`] also records it as listed, so
    /// a later listing that names it again gets [`WalkStep::AlreadyListed`].
    pub fn admit(&mut self, listed_dir: &str, child: &str) -> WalkStep {
        let position = position_key(child);
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
        if self.visited.insert(walk_path_key(child)) {
            WalkStep::Follow
        } else {
            WalkStep::AlreadyListed
        }
    }
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
}
