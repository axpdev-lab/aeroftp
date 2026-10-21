// SPDX-License-Identifier: GPL-3.0-or-later
// Canonical equivalence is a correspondence, never a filesystem path rewrite.
use std::collections::{BTreeMap, BTreeSet, HashMap};
use unicode_normalization::UnicodeNormalization;

#[derive(Debug)]
pub(crate) struct Pair {
    pub local: Option<String>,
    pub remote: Option<String>,
    pub local_path: String,
    pub remote_path: String,
    pub blocked: bool,
}

type Tree = BTreeMap<String, BTreeMap<String, bool>>;

fn tree(entries: &HashMap<String, crate::sync::FileInfo>) -> Tree {
    let mut tree = Tree::new();
    for (path, info) in entries {
        if crate::sync::validate_relative_path(path).is_err() {
            continue;
        }
        let parts: Vec<_> = path.split('/').collect();
        let mut parent = String::new();
        for (i, name) in parts.iter().enumerate() {
            tree.entry(parent.clone())
                .or_default()
                .insert(name.to_string(), i + 1 < parts.len() || info.is_dir);
            parent = join(&parent, name);
        }
    }
    tree
}

fn join(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_string()
    } else {
        format!("{parent}/{name}")
    }
}

/// Walk siblings under already resolved parents. Exact pairs win; a fallback
/// requires uniqueness in the *whole* sibling group, including exact matches.
pub(crate) fn pair_paths(
    local: &HashMap<String, crate::sync::FileInfo>,
    remote: &HashMap<String, crate::sync::FileInfo>,
) -> Vec<Pair> {
    let lt = tree(local);
    let rt = tree(remote);
    let empty = BTreeMap::new();
    let mut pending = vec![(
        Some(String::new()),
        Some(String::new()),
        String::new(),
        String::new(),
        false,
    )];
    let mut result = Vec::new();
    while let Some((lp, rp, ld, rd, blocked)) = pending.pop() {
        let ls = lp.as_ref().and_then(|p| lt.get(p)).unwrap_or(&empty);
        let rs = rp.as_ref().and_then(|p| rt.get(p)).unwrap_or(&empty);
        let groups = |siblings: &BTreeMap<String, bool>| {
            let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for name in siblings.keys() {
                groups
                    .entry(name.nfc().collect())
                    .or_default()
                    .push(name.clone());
            }
            groups
        };
        let lg = groups(ls);
        let rg = groups(rs);
        let mut used = BTreeSet::new();
        let mut pairs = Vec::new();
        for name in ls.keys() {
            let key: String = name.nfc().collect();
            let exact = rs.contains_key(name);
            let candidates = rg.get(&key);
            let unique = lg[&key].len() == 1 && candidates.is_some_and(|c| c.len() == 1);
            let counterpart = if exact {
                Some(name.clone())
            } else if unique {
                Some(candidates.unwrap()[0].clone())
            } else {
                None
            };
            let ambiguous = !exact && candidates.is_some() && !unique;
            if let Some(r) = &counterpart {
                used.insert(r.clone());
            }
            pairs.push((Some(name.clone()), counterpart, ambiguous));
        }
        for name in rs.keys().filter(|n| !used.contains(*n)) {
            let key: String = name.nfc().collect();
            pairs.push((None, Some(name.clone()), lg.contains_key(&key)));
        }
        for (ln, rn, ambiguous) in pairs {
            let lpath = join(&ld, ln.as_ref().or(rn.as_ref()).unwrap());
            let rpath = join(&rd, rn.as_ref().or(ln.as_ref()).unwrap());
            let actual_l = ln.as_ref().and_then(|n| lp.as_ref().map(|p| join(p, n)));
            let actual_r = rn.as_ref().and_then(|n| rp.as_ref().map(|p| join(p, n)));
            let collision = ln
                .as_ref()
                .zip(rn.as_ref())
                .is_some_and(|(l, r)| ls[l] != rs[r]);
            let blocked = blocked || ambiguous || collision;
            if actual_l.as_ref().is_some_and(|p| local.contains_key(p))
                || actual_r.as_ref().is_some_and(|p| remote.contains_key(p))
            {
                result.push(Pair {
                    local: actual_l.clone(),
                    remote: actual_r.clone(),
                    local_path: lpath.clone(),
                    remote_path: rpath.clone(),
                    blocked,
                });
            }
            if ln.as_ref().is_some_and(|n| ls[n]) || rn.as_ref().is_some_and(|n| rs[n]) {
                pending.push((actual_l, actual_r, lpath, rpath, blocked));
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::{classify_with_summary, CompareOptions, FileInfo, SyncStatus};

    fn inventory(entries: &[(&str, bool, u64)]) -> HashMap<String, FileInfo> {
        entries
            .iter()
            .map(|(path, is_dir, size)| {
                (
                    path.to_string(),
                    FileInfo {
                        name: path.rsplit('/').next().unwrap().to_string(),
                        path: path.to_string(),
                        size: *size,
                        modified: None,
                        is_dir: *is_dir,
                        checksum: None,
                        checksum_alg: None,
                    },
                )
            })
            .collect()
    }

    #[test]
    fn canonical_unicode_equal_files_and_empty_directories() {
        let local = inventory(&[
            ("🇯🇵/εσωτερικο\u{301}ς", true, 0),
            ("cafe\u{301}.txt", false, 8),
            ("ascii", false, 8),
        ]);
        let remote = inventory(&[
            ("🇯🇵/εσωτερικός", true, 4096),
            ("café.txt", false, 8),
            ("ascii", false, 8),
        ]);
        let report = classify_with_summary(local, remote, &CompareOptions::default(), None);
        assert_eq!(report.summary.identical_count, 3);
        assert!(report
            .differences
            .iter()
            .all(|r| r.status == SyncStatus::Identical));
    }

    #[test]
    fn canonical_unicode_nested_paths_new_children_and_overwrites() {
        let local = inventory(&[
            ("cafe\u{301}/é/a", false, 8),
            ("cafe\u{301}/é/new", false, 8),
        ]);
        let remote = inventory(&[("café/e\u{301}/a", false, 4)]);
        let pairs = pair_paths(&local, &remote);
        let overwrite = pairs.iter().find(|p| p.local_path.ends_with("/a")).unwrap();
        assert_eq!(overwrite.remote_path, "café/e\u{301}/a");
        assert!(!overwrite.blocked);
        let new = pairs
            .iter()
            .find(|p| p.local_path.ends_with("/new"))
            .unwrap();
        assert_eq!(new.remote_path, "café/e\u{301}/new");
        assert!(new.remote.is_none());
        let report = classify_with_summary(local, remote, &CompareOptions::default(), None);
        let row = report
            .differences
            .iter()
            .find(|r| r.relative_path.ends_with("/a"))
            .unwrap();
        assert!(row.local_info.is_some() && row.remote_info.is_some());
        assert_eq!(row.remote_relative_path.as_deref(), Some("café/e\u{301}/a"));
    }

    #[test]
    fn canonical_unicode_exact_twins_win_ambiguous_descendants_blocked() {
        let local = inventory(&[("café/a", false, 8), ("cafe\u{301}/b", false, 8)]);
        let remote = inventory(&[("café/a", false, 8)]);
        let pairs = pair_paths(&local, &remote);
        assert!(
            !pairs
                .iter()
                .find(|p| p.local_path == "café/a")
                .unwrap()
                .blocked
        );
        assert!(
            pairs
                .iter()
                .find(|p| p.local_path == "cafe\u{301}/b")
                .unwrap()
                .blocked
        );
        let report = classify_with_summary(local, remote, &CompareOptions::default(), None);
        assert_eq!(report.differences.len(), 1);
        assert_eq!(report.differences[0].status, SyncStatus::Conflict);
        assert!(report.differences[0].name_match_blocked);
    }

    #[test]
    fn canonical_unicode_type_collision_blocks_descendants() {
        let local = inventory(&[("cafe\u{301}", true, 0), ("cafe\u{301}/new", false, 8)]);
        let remote = inventory(&[("café", false, 8)]);
        assert!(pair_paths(&local, &remote).iter().all(|p| p.blocked));
    }
    #[test]
    fn canonical_unicode_remote_only_child_uses_local_parent_and_index_key() {
        let local = inventory(&[("cafe\u{301}", true, 0)]);
        let remote = inventory(&[("café", true, 4096), ("café/new", false, 8)]);
        let report = classify_with_summary(local, remote, &CompareOptions::default(), None);
        let row = report
            .differences
            .iter()
            .find(|r| r.status == SyncStatus::RemoteOnly)
            .unwrap();
        assert_eq!(row.relative_path, "cafe\u{301}/new");
        assert_eq!(row.local_relative_path.as_deref(), Some("cafe\u{301}/new"));
        assert_eq!(row.remote_relative_path.as_deref(), Some("café/new"));
    }
}
