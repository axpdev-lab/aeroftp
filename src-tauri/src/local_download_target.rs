// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Destination identity for GUI and CLI recursive downloads. Comparison keys
//! never change the exact remote identity or the path passed to a writer.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use unicode_normalization::UnicodeNormalization;

pub struct LocalTargetClaim {
    pub remote_path: String,
    pub local_path: PathBuf,
}

/// Independent filesystem equivalences. Case-sensitive APFS still folds
/// canonical Unicode spellings; Linux filesystems need not fold either.
#[derive(Clone, Copy)]
pub struct DestinationRules {
    pub fold_case: bool,
    pub fold_normalization: bool,
}

#[derive(Debug)]
pub struct LocalTargetConflict {
    pub local_path: PathBuf,
    first_remote: String,
    second_remote: String,
}

impl std::fmt::Display for LocalTargetConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Ambiguous local target '{}': remote entries '{}' and '{}' both map to it",
            self.local_path.display(),
            self.first_remote,
            self.second_remote
        )
    }
}

pub struct LocalTargetGuard {
    rules: DestinationRules,
    claimed: HashMap<String, String>,
}

impl LocalTargetGuard {
    pub fn new(destination: &Path) -> Self {
        Self::with_rules(destination_rules(destination))
    }

    pub fn with_rules(rules: DestinationRules) -> Self {
        Self {
            rules,
            claimed: HashMap::new(),
        }
    }

    fn key(&self, path: &Path) -> String {
        let mut key = path.to_string_lossy().into_owned();
        if self.rules.fold_normalization {
            key = key.nfc().collect();
        }
        if self.rules.fold_case {
            key = case_fold_key(&key);
            // Folding can decompose (`İ` becomes `i` plus a combining dot),
            // so compose again for a normalization-insensitive volume.
            if self.rules.fold_normalization {
                key = key.nfc().collect();
            }
        }
        key
    }

    /// Validate a whole listing before publishing any claim. Repeated listings
    /// of the same exact source are harmless, but siblings claiming one target
    /// are refused even when the server repeated their remote spelling.
    pub fn claim_listing(
        &mut self,
        claims: &[LocalTargetClaim],
    ) -> Result<(), LocalTargetConflict> {
        let mut listing = HashMap::new();
        for claim in claims {
            let key = self.key(&claim.local_path);
            if let Some(first) = listing.insert(key.clone(), claim.remote_path.clone()) {
                return Err(ambiguous_local_target(
                    &first,
                    &claim.remote_path,
                    &claim.local_path,
                ));
            }
            if let Some(first) = self.claimed.get(&key) {
                if crate::sync_core::walk_path_key(first)
                    != crate::sync_core::walk_path_key(&claim.remote_path)
                {
                    return Err(ambiguous_local_target(
                        first,
                        &claim.remote_path,
                        &claim.local_path,
                    ));
                }
            }
        }
        self.claimed.extend(listing);
        Ok(())
    }
}

/// Why Windows cannot hold a name, or `None`. A colon does not fail there: it
/// opens an alternate stream and leaves a 0-byte file under the part before it,
/// which a reverse mirror then uploads. A trailing dot or space is dropped on
/// the way in, so the name that lands is another one and a Compare never meets
/// it again (Requested 124).
fn windows_name_problem(name: &str) -> Option<String> {
    if let Some(found) = name
        .chars()
        .find(|c| matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*' | '\\') || (*c as u32) < 32)
    {
        return Some(format!("Windows cannot hold a name with {found:?}"));
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Some("Windows cannot hold a name ending in a dot or a space".to_string());
    }
    crate::windows_acl::check_windows_reserved(name)
        .map(|reserved| format!("Windows reserves the name {reserved}"))
}

/// Why this machine cannot hold a local path relative to a destination.
fn unwritable_on(path: &str, windows: bool) -> Option<String> {
    if !windows {
        return None;
    }
    path.split('/').find_map(|segment| {
        windows_name_problem(segment).map(|reason| format!("{reason}: {segment}"))
    })
}

/// Refuse a download whose path this machine cannot hold, before anything is
/// created for it. Every folder on the way is read too: a remote folder named
/// `bad.` would land as `bad` and take the file with it. A drive prefix is not a
/// name, so a colon there is fine.
pub fn refuse_unwritable_name(local_path: &str) -> Result<(), String> {
    for component in Path::new(local_path).components() {
        if let std::path::Component::Normal(name) = component {
            if let Some(reason) = unwritable_on(&name.to_string_lossy(), cfg!(windows)) {
                return Err(reason);
            }
        }
    }
    Ok(())
}

/// A file a run cannot download as it is, and why.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LocalTargetProblem {
    pub path: String,
    pub reason: String,
}

/// `files` are the entries of `paths` that are files; the others are folders.
fn target_problems(
    rules: DestinationRules,
    windows: bool,
    paths: &[String],
    files: &[String],
) -> Vec<LocalTargetProblem> {
    let guard = LocalTargetGuard::with_rules(rules);
    // Every folder on the way counts: `Foo/a.txt` and `foo/b.txt` would share one
    // folder and could not keep the two names.
    let prefixes = |path: &'_ str| -> Vec<String> {
        let parts: Vec<&str> = path.split('/').collect();
        (1..=parts.len()).map(|n| parts[..n].join("/")).collect()
    };
    let mut spellings: HashMap<String, Vec<String>> = HashMap::new();
    for path in paths {
        for prefix in prefixes(path) {
            let same = spellings.entry(guard.key(Path::new(&prefix))).or_default();
            if !same.contains(&prefix) {
                same.push(prefix);
            }
        }
    }
    // A store can hold a file `foo` beside `foo/bar.txt`; a disk cannot.
    let file_keys: HashMap<String, &String> = files
        .iter()
        .map(|file| (guard.key(Path::new(file)), file))
        .collect();
    let mut under_a_file: HashMap<&String, String> = HashMap::new();
    let known_files: std::collections::HashSet<&String> = files.iter().collect();
    for path in paths {
        let parts = prefixes(path);
        let inside = if known_files.contains(path) {
            &parts[..parts.len() - 1]
        } else {
            &parts[..]
        };
        if let Some(file) = inside
            .iter()
            .find_map(|prefix| file_keys.get(&guard.key(Path::new(prefix))))
        {
            let reason = format!("{file} is a file, and {path} would go inside it");
            under_a_file.insert(path, reason.clone());
            under_a_file.insert(file, reason);
        }
    }
    let mut problems = Vec::new();
    for path in paths {
        if let Some(reason) = unwritable_on(path, windows) {
            problems.push(LocalTargetProblem {
                path: path.clone(),
                reason,
            });
            continue;
        }
        if let Some(reason) = under_a_file.get(path) {
            problems.push(LocalTargetProblem {
                path: path.clone(),
                reason: reason.clone(),
            });
            continue;
        }
        let collision = prefixes(path).into_iter().find_map(|prefix| {
            let same = &spellings[&guard.key(Path::new(&prefix))];
            same.iter().find(|other| **other != prefix).cloned()
        });
        if let Some(other) = collision {
            problems.push(LocalTargetProblem {
                path: path.clone(),
                reason: format!(
                    "This disk cannot tell {path} apart from {other}: one would replace the other"
                ),
            });
        }
    }
    problems
}

/// A file a run would write where the disk already holds the same name spelled
/// differently: it would replace that entry, `foo.txt` over `Foo.txt`. The same
/// spelling is an ordinary update. A folder spelled differently only takes the
/// files in, so it is not read here.
fn existing_spelling_problems(
    rules: DestinationRules,
    destination: &Path,
    files: &[String],
) -> Vec<LocalTargetProblem> {
    let guard = LocalTargetGuard::with_rules(rules);
    let mut listed: HashMap<String, HashMap<String, Vec<String>>> = HashMap::new();
    let mut problems = Vec::new();
    for file in files {
        let (parent, name) = file.rsplit_once('/').unwrap_or(("", file));
        let entries = listed.entry(parent.to_string()).or_insert_with(|| {
            let mut entries: HashMap<String, Vec<String>> = HashMap::new();
            // A folder not there yet holds nothing to replace.
            for entry in std::fs::read_dir(destination.join(parent))
                .into_iter()
                .flatten()
                .flatten()
            {
                let found = entry.file_name().to_string_lossy().into_owned();
                entries
                    .entry(guard.key(Path::new(&found)))
                    .or_default()
                    .push(found);
            }
            entries
        });
        let held = entries.get(&guard.key(Path::new(name)));
        let other = held
            .filter(|names| !names.iter().any(|found| found == name))
            .and_then(|names| names.first());
        if let Some(other) = other {
            problems.push(LocalTargetProblem {
                path: file.clone(),
                reason: format!("This disk already holds {other}, which {name} would replace"),
            });
        }
    }
    problems
}

/// The paths a run would download that `destination` cannot hold apart, or at
/// all. Both sides of a collision are named: which one should win is not ours
/// to choose, and the loser's contents would be gone.
pub fn local_target_problems(
    destination: &Path,
    paths: &[String],
    files: &[String],
) -> Vec<LocalTargetProblem> {
    let rules = destination_rules(destination);
    let mut problems = target_problems(rules, cfg!(windows), paths, files);
    for problem in existing_spelling_problems(rules, destination, files) {
        if !problems.iter().any(|known| known.path == problem.path) {
            problems.push(problem);
        }
    }
    problems
}

/// Case key for a case-insensitive volume: map every character to its full
/// uppercase and every character of that to its full lowercase, and repeat
/// until the key stops changing. NTFS compares through an uppercase table and
/// APFS through Unicode case folding, so `ς`, `σ` and `Σ` are one name on
/// both, and APFS also folds the Kelvin sign to `k` and `ß` and `ẞ` to `ss`.
/// One pass is not enough: `ẞ` lowercases to `ß`, which only the next pass
/// turns into `ss`. `str::to_lowercase` is not a stable key either: it applies
/// the final-sigma rule and keeps `ς` apart from `σ`. Where the volumes differ
/// (`ß` and `ss`, or dotless `ı`, are distinct names on NTFS) the key merges:
/// a needless refusal is safer than two files landing on one name.
fn case_fold_key(name: &str) -> String {
    let fold = |s: &str| -> String {
        s.chars()
            .flat_map(char::to_uppercase)
            .flat_map(char::to_lowercase)
            .collect()
    };
    let mut key = fold(name);
    // Each pass maps a string to one that no later pass changes after at most
    // a few steps; the bound only guards against a future Unicode table that
    // cycles.
    for _ in 0..4 {
        let next = fold(&key);
        if next == key {
            break;
        }
        key = next;
    }
    key
}

fn ambiguous_local_target(first: &str, second: &str, local: &Path) -> LocalTargetConflict {
    LocalTargetConflict {
        local_path: local.to_path_buf(),
        first_remote: first.into(),
        second_remote: second.into(),
    }
}

fn destination_rules(destination: &Path) -> DestinationRules {
    // A new nested destination inherits the closest existing parent's volume.
    let parent = destination
        .ancestors()
        .find(|path| path.is_dir())
        .unwrap_or(Path::new("."));
    // Each attempt owns a private, randomly named namespace. No pre-existing
    // file can be removed, including the uppercase PID/counter probe spelling
    // used by older versions. TempDir cleans up only this created namespace.
    let probed = tempfile::Builder::new()
        .prefix(".aeroftp-name-probe-")
        .tempdir_in(parent)
        .ok()
        .and_then(|dir| {
            let case = probe_alias(dir.path(), "case", "CASE").ok()?;
            let normalization = probe_alias(dir.path(), "café", "cafe\u{301}").ok()?;
            Some(DestinationRules {
                fold_case: case,
                fold_normalization: normalization,
            })
        });
    // When probing is unavailable, refuse possible collisions conservatively.
    #[allow(unused_mut)]
    let mut rules = probed.unwrap_or(DestinationRules {
        fold_case: true,
        fold_normalization: true,
    });
    #[cfg(test)]
    {
        if let Ok(value) = std::env::var("AEROFTP_TEST_LOCAL_CASE_FOLD") {
            if value == "1" || value == "0" {
                rules.fold_case = value == "1";
            }
        }
        if let Ok(value) = std::env::var("AEROFTP_TEST_LOCAL_NORMALIZATION_FOLD") {
            if value == "1" || value == "0" {
                rules.fold_normalization = value == "1";
            }
        }
    }
    rules
}

fn probe_alias(dir: &Path, first: &str, second: &str) -> std::io::Result<bool> {
    let original = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join(first))?;
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join(second))
    {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            // The counterpart must be the file we opened, rather than an
            // unrelated occupant. A collision fails the probe, never deletes it.
            let same = same_file::Handle::from_file(original)?
                == same_file::Handle::from_path(dir.join(second))?;
            if same {
                Ok(true)
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "probe collision",
                ))
            }
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::{target_problems, unwritable_on, DestinationRules};

    #[test]
    fn names_windows_cannot_hold_are_named_with_a_reason() {
        for path in [
            "colon:name.txt",
            "q?mark",
            "pipe|x",
            "trail-dot.",
            "trail-space ",
            "CON.txt",
            "sub/LPT1",
            "lpt9.log",
        ] {
            assert!(unwritable_on(path, true).is_some(), "{path}");
            assert!(unwritable_on(path, false).is_none(), "{path}");
        }
        for path in ["plain.txt", "CONFIG.txt", "COM0.txt", "a b/c.d"] {
            assert_eq!(unwritable_on(path, true), None, "{path}");
        }
    }

    /// A store can hold a file beside a key under its name, and a disk cannot:
    /// both are refused, and the rest goes on.
    #[test]
    fn a_file_with_another_path_under_it_is_refused_with_that_path() {
        let keeps = DestinationRules {
            fold_case: false,
            fold_normalization: false,
        };
        let files = [
            "foo".to_string(),
            "foo/bar.txt".to_string(),
            "ok/a.txt".to_string(),
        ];
        let problems = target_problems(keeps, false, &files, &files);
        assert_eq!(
            problems.iter().map(|p| p.path.as_str()).collect::<Vec<_>>(),
            ["foo", "foo/bar.txt"]
        );
        assert!(problems[0].reason.contains("foo is a file"));
        // A planned folder with the name of a planned file is the same conflict.
        let paths = ["foo".to_string(), "foo/a".to_string()];
        let folder_only = ["foo/a".to_string()];
        assert!(target_problems(keeps, false, &paths, &[]).is_empty());
        assert_eq!(target_problems(keeps, false, &paths, &folder_only).len(), 0);
        let same_name = ["foo".to_string(), "foo".to_string()];
        let one_file = ["foo".to_string()];
        assert!(target_problems(keeps, false, &same_name[..1], &one_file).is_empty());
    }

    /// Where the disk folds case a planned `foo.txt` would replace `Foo.txt`; the
    /// same spelling is an ordinary update, and a disk that keeps case holds both.
    #[test]
    fn a_file_is_refused_where_the_disk_holds_its_name_spelled_differently() {
        let dest = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dest.path().join("docs")).unwrap();
        std::fs::write(dest.path().join("docs/Foo.txt"), "mine").unwrap();
        std::fs::write(dest.path().join("same.txt"), "mine").unwrap();
        let folds = DestinationRules {
            fold_case: true,
            fold_normalization: true,
        };
        let files = [
            "docs/foo.txt".to_string(),
            "same.txt".to_string(),
            "new/absent.txt".to_string(),
        ];
        let problems = existing_spelling_problems(folds, dest.path(), &files);
        assert_eq!(
            problems.iter().map(|p| p.path.as_str()).collect::<Vec<_>>(),
            ["docs/foo.txt"]
        );
        assert!(
            problems[0].reason.contains("Foo.txt"),
            "{}",
            problems[0].reason
        );
        let keeps = DestinationRules {
            fold_case: false,
            fold_normalization: false,
        };
        assert!(existing_spelling_problems(keeps, dest.path(), &files).is_empty());
    }

    #[test]
    fn superscript_digits_are_reserved_on_windows_too() {
        for name in ["COM¹", "com².txt", "LPT³"] {
            assert!(windows_name_problem(name).is_some(), "{name}");
        }
        assert!(windows_name_problem("COM¹x").is_none());
    }

    #[test]
    fn names_that_differ_only_by_case_are_both_refused_where_the_disk_folds_case() {
        let paths = [
            "a/File.txt".to_string(),
            "a/file.txt".to_string(),
            "b.txt".to_string(),
        ];
        let folds = DestinationRules {
            fold_case: true,
            fold_normalization: true,
        };
        let problems = target_problems(folds, false, &paths, &paths);
        assert_eq!(
            problems.iter().map(|p| p.path.as_str()).collect::<Vec<_>>(),
            ["a/File.txt", "a/file.txt"]
        );
        // Folders that differ only by case collide too, whatever the files are.
        let folders = [
            "Foo/a.txt".to_string(),
            "foo/b.txt".to_string(),
            "bar/c.txt".to_string(),
        ];
        let problems = target_problems(folds, false, &folders, &folders);
        assert_eq!(
            problems.iter().map(|p| p.path.as_str()).collect::<Vec<_>>(),
            ["Foo/a.txt", "foo/b.txt"]
        );
        let keeps = DestinationRules {
            fold_case: false,
            fold_normalization: false,
        };
        assert!(target_problems(keeps, false, &paths, &paths).is_empty());
    }

    use super::*;
    fn claim(local: &str, remote: &str) -> LocalTargetClaim {
        LocalTargetClaim {
            local_path: PathBuf::from(local),
            remote_path: remote.into(),
        }
    }

    #[test]
    fn audit425_normalization_is_independent_of_case_and_io_keeps_spelling() {
        let claims = [
            claim("/tmp/café", "/root/café"),
            claim("/tmp/cafe\u{301}", "/root/cafe\u{301}"),
        ];
        let mut guard = LocalTargetGuard::with_rules(DestinationRules {
            fold_case: false,
            fold_normalization: true,
        });
        assert!(guard.claim_listing(&claims).is_err());
        assert_eq!(claims[1].remote_path, "/root/cafe\u{301}");
        assert_eq!(claims[1].local_path, PathBuf::from("/tmp/cafe\u{301}"));
        // A failed listing commits no claims.
        guard.claim_listing(&claims[..1]).unwrap();
    }

    #[test]
    fn audit425_case_key_merges_greek_sigmas_kelvin_and_both_sharp_s() {
        let insensitive = || {
            LocalTargetGuard::with_rules(DestinationRules {
                fold_case: true,
                fold_normalization: false,
            })
        };
        for (first, second) in [
            ("ς", "σ"),
            ("ΟΔΟΣ", "οδος"),
            ("οδοσ", "οδος"),
            ("\u{212A}", "k"),
            ("ß", "ss"),
            ("STRASSE", "straße"),
            ("ẞ", "ß"),
            ("ẞ", "ss"),
            ("STRAẞE", "strasse"),
        ] {
            assert!(
                insensitive()
                    .claim_listing(&[
                        claim(first, &format!("/{first}")),
                        claim(second, &format!("/{second}")),
                    ])
                    .is_err(),
                "{first} and {second} are one name on a case-insensitive volume"
            );
        }
        insensitive()
            .claim_listing(&[claim("i", "/i"), claim("İ", "/İ")])
            .unwrap();
    }

    #[test]
    fn audit425_case_and_normalization_fold_compose_after_folding() {
        // `ǰ` has no single-character uppercase: it folds to `j` plus a
        // combining caron, which must compose back to meet `J` + caron.
        let mut guard = LocalTargetGuard::with_rules(DestinationRules {
            fold_case: true,
            fold_normalization: true,
        });
        assert!(guard
            .claim_listing(&[claim("\u{1F0}", "/a"), claim("J\u{30C}", "/b")])
            .is_err());
    }

    #[test]
    fn audit425_sensitive_filesystem_retains_distinct_unicode_and_case() {
        let mut guard = LocalTargetGuard::with_rules(DestinationRules {
            fold_case: false,
            fold_normalization: false,
        });
        guard
            .claim_listing(&[
                claim("café", "/café"),
                claim("cafe\u{301}", "/cafe\u{301}"),
                claim("A", "/A"),
                claim("a", "/a"),
            ])
            .unwrap();
    }

    #[test]
    fn audit425_case_and_unicode_aliases_are_refused_across_listings() {
        let mut guard = LocalTargetGuard::with_rules(DestinationRules {
            fold_case: true,
            fold_normalization: true,
        });
        guard
            .claim_listing(&[claim("CAFÉ", "/first/CAFÉ")])
            .unwrap();
        assert!(guard
            .claim_listing(&[claim("cafe\u{301}", "/second/cafe\u{301}")])
            .is_err());
    }

    #[test]
    fn audit425_case_probe_preserves_preexisting_uppercase_file() {
        let dir = tempfile::tempdir().unwrap();
        let name = format!(".AEROFTP-CASE-PROBE-{}-0", std::process::id());
        let victim = dir.path().join(name);
        std::fs::write(&victim, b"keep me").unwrap();
        let _ = LocalTargetGuard::new(dir.path());
        assert_eq!(std::fs::read(victim).unwrap(), b"keep me");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn audit425_probe_collision_leaves_both_occupants_untouched() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("CASE"), b"original").unwrap();
        // On a case-sensitive volume this creates "case" and detects the
        // unrelated counterpart. On an insensitive volume create_new refuses.
        let _ = probe_alias(dir.path(), "case", "CASE");
        assert_eq!(std::fs::read(dir.path().join("CASE")).unwrap(), b"original");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn audit425_macos_destination_folds_normalization() {
        let dir = tempfile::tempdir().unwrap();
        assert!(destination_rules(dir.path()).fold_normalization);
    }
}
