// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! No two saved profiles share a name.
//!
//! Two profiles of one name cannot be told apart in the lists, and
//! `--profile NAME` takes the first. A profile created, edited, duplicated or
//! imported onto a name another profile holds therefore gets the first free
//! " (1)", " (2)", ...: [`unique_profile_names`] is applied to every write of a
//! profile list (`user_partitions::insert_profiles_with_dek`), whichever
//! window, command or import made it, and to the writes that bypass it (the
//! legacy blob, a keystore merge, the legacy migration, a partition database
//! or blob restored whole from a backup). A command that names the profile it saved,
//! or the ones it renamed, takes the names from the list stored
//! ([`name_of`], [`renamed_by_write`]): only the write knows which profile
//! had a name first. The frontend's twin of [`unique_profile_names`]
//! (`src/utils/uniqueProfileName.ts`) is applied only where it writes without
//! the partition layer (the legacy store), against the list that store holds.

use std::collections::HashSet;

/// What two names collide on: the CLI selects a profile by its name,
/// case-insensitive (`resolve_profile_selector`).
fn key(name: &str) -> String {
    name.to_lowercase()
}

/// The names in use, handing out names not yet taken.
struct ProfileNames {
    taken: HashSet<String>,
}

impl ProfileNames {
    /// The names `existing` already holds.
    fn new<'a>(existing: impl IntoIterator<Item = &'a str>) -> Self {
        Self {
            taken: existing.into_iter().map(key).collect(),
        }
    }

    /// Takes `name` as it is when no profile has it yet; false when one has.
    fn reserve(&mut self, name: &str) -> bool {
        self.taken.insert(key(name))
    }

    /// `name` when no profile has it yet, else the first of "name (1)",
    /// "name (2)", ... that none has. The name returned is taken from then on.
    fn claim(&mut self, name: &str) -> String {
        let mut claimed = name.to_string();
        let mut n = 1u32;
        while self.taken.contains(&key(&claimed)) {
            claimed = format!("{name} ({n})");
            n += 1;
        }
        self.taken.insert(key(&claimed));
        claimed
    }
}

/// A profile stored under another name than the one it was saved with.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RenamedProfile {
    pub id: String,
    pub from: String,
    pub to: String,
}

fn profile_name(profile: &serde_json::Value) -> Option<&str> {
    profile.get("name").and_then(|v| v.as_str())
}

/// Give each profile of `added` (JSON objects with a `name`) a name that no
/// profile of `names` holds, nor an earlier one of `added`: the ones renamed.
fn claim_profile_names(
    names: &mut ProfileNames,
    added: &mut [serde_json::Value],
) -> Vec<RenamedProfile> {
    let mut renamed = Vec::new();
    for profile in added {
        let Some(name) = profile_name(profile).map(str::to_string) else {
            continue;
        };
        let claimed = names.claim(&name);
        if claimed != name {
            renamed.push(RenamedProfile {
                id: profile
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                from: name,
                to: claimed.clone(),
            });
            profile["name"] = serde_json::Value::String(claimed);
        }
    }
    renamed
}

fn profile_id(profile: &serde_json::Value) -> Option<&str> {
    profile.get("id").and_then(|v| v.as_str())
}

/// `profiles`, about to replace `previous`, with no two of one name: a
/// profile that keeps the id and the name it had keeps its name (the first
/// in list order, when an older list already held two of one name); a new or
/// renamed profile whose name another one holds gets the first free " (1)",
/// " (2)", ... Every write of a profile list goes through this
/// (`user_partitions::insert_profiles_with_dek`), so no path can store two
/// profiles of one name. Returns the list to store and the profiles renamed.
pub fn unique_profile_names(
    previous: &[serde_json::Value],
    profiles: &[serde_json::Value],
) -> (Vec<serde_json::Value>, Vec<RenamedProfile>) {
    let before: std::collections::HashMap<&str, &str> = previous
        .iter()
        .filter_map(|p| Some((profile_id(p)?, profile_name(p)?)))
        .collect();
    let unchanged = |p: &serde_json::Value| {
        profile_id(p).and_then(|id| before.get(id)).copied() == profile_name(p)
            && profile_name(p).is_some()
    };
    let keeps: Vec<bool> = profiles.iter().map(unchanged).collect();
    let mut stored = profiles.to_vec();
    let mut names = ProfileNames::new(std::iter::empty());
    // Every name a profile can keep as it is goes before any number is given,
    // so a number never takes a name another profile has: first the names the
    // profiles that had them keep, then the free names asked for. The
    // profiles left (a second of one name) get the first free " (1)", " (2)",
    // ..., those that had their name first.
    let mut numbered = Vec::new();
    for pass in [true, false] {
        for (i, _) in keeps.iter().enumerate().filter(|(_, k)| **k == pass) {
            if profile_name(&stored[i]).is_some_and(|name| !names.reserve(name)) {
                numbered.push(i);
            }
        }
    }
    let mut renamed = Vec::new();
    for i in numbered {
        renamed.extend(claim_profile_names(
            &mut names,
            std::slice::from_mut(&mut stored[i]),
        ));
    }
    (stored, renamed)
}

/// The name of the profile `id` in `profiles`.
pub fn name_of<'a>(profiles: &'a [serde_json::Value], id: &str) -> Option<&'a str> {
    profiles
        .iter()
        .find(|p| profile_id(p) == Some(id))
        .and_then(profile_name)
}

/// The profiles of `sent` that `stored`, the list a write of `sent` stored,
/// holds under another name, each with the name it was stored under: a
/// profile saved onto a taken name, and one of an older list's two of one
/// name that the write numbered. A name reported here is what selects that
/// profile, which a name claimed before the write may not be.
pub fn renamed_by_write(
    sent: &[serde_json::Value],
    stored: &[serde_json::Value],
) -> Vec<RenamedProfile> {
    sent.iter()
        .filter_map(|profile| {
            let id = profile_id(profile)?;
            let from = profile_name(profile)?;
            let to = name_of(stored, id)?;
            (to != from).then(|| RenamedProfile {
                id: id.to_string(),
                from: from.to_string(),
                to: to.to_string(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn names_of(list: &[serde_json::Value]) -> Vec<&str> {
        list.iter()
            .map(|p| p["name"].as_str().unwrap_or(""))
            .collect()
    }

    /// A profile created, edited or imported onto a name another profile
    /// holds is the one that changes; the profile that had the name keeps it,
    /// wherever it sits in the list.
    #[test]
    fn the_profile_that_had_the_name_keeps_it() {
        let previous = [
            json!({"id": "a", "name": "Lab"}),
            json!({"id": "b", "name": "Prod"}),
        ];
        // "b" renamed to "lab", placed before "a", and a new "Lab" at the end.
        let profiles = [
            json!({"id": "b", "name": "lab"}),
            json!({"id": "a", "name": "Lab"}),
            json!({"id": "c", "name": "Lab"}),
        ];
        let (stored, renamed) = unique_profile_names(&previous, &profiles);
        assert_eq!(names_of(&stored), ["lab (1)", "Lab", "Lab (2)"]);
        assert_eq!(
            renamed.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["b", "c"]
        );
    }

    /// A list saved before this rule may hold two profiles of one name: the
    /// next write keeps the first and numbers the others.
    #[test]
    fn an_older_list_with_two_of_one_name_is_put_right_on_the_next_write() {
        let previous = [
            json!({"id": "a", "name": "Proton API"}),
            json!({"id": "b", "name": "Proton API"}),
        ];
        let (stored, renamed) = unique_profile_names(&previous, &previous);
        assert_eq!(names_of(&stored), ["Proton API", "Proton API (1)"]);
        assert_eq!(renamed.len(), 1);
        let (again, none) = unique_profile_names(&stored, &stored);
        assert_eq!(again, stored, "a list with unique names is stored as it is");
        assert!(none.is_empty());
    }

    /// A number never takes a name another profile keeps: with an older list
    /// "Lab", "Lab", "Lab (1)", the second "Lab" gets "Lab (2)", and "Lab (1)"
    /// still selects the profile that had it. A free name asked for is kept
    /// the same way, ahead of the numbers.
    #[test]
    fn a_number_never_takes_a_name_another_profile_keeps() {
        let previous = [
            json!({"id": "a", "name": "Lab"}),
            json!({"id": "b", "name": "Lab"}),
            json!({"id": "c", "name": "Lab (1)"}),
        ];
        let (stored, renamed) = unique_profile_names(&previous, &previous);
        assert_eq!(names_of(&stored), ["Lab", "Lab (2)", "Lab (1)"]);
        assert_eq!(
            renamed.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["b"]
        );

        let older = &previous[..2];
        let mut sent = older.to_vec();
        sent.push(json!({"id": "x", "name": "Lab (1)"}));
        let (stored, _) = unique_profile_names(older, &sent);
        assert_eq!(names_of(&stored), ["Lab", "Lab (2)", "Lab (1)"]);
    }

    /// Over every list of three names drawn from a set that collides on case
    /// and on numbers, kept, new, or both: no two names are equal, and a
    /// profile whose name no other profile in the list has keeps it.
    #[test]
    fn a_name_only_one_profile_has_is_never_changed() {
        let pool = ["Lab", "lab", "Lab (1)", "Lab (2)", "Prod"];
        for x in 0..pool.len().pow(3) {
            let picked = [pool[x % 5], pool[x / 5 % 5], pool[x / 25 % 5]];
            let list: Vec<serde_json::Value> = picked
                .iter()
                .enumerate()
                .map(|(i, name)| json!({"id": i.to_string(), "name": name}))
                .collect();
            for kept in 0..=list.len() {
                let (stored, _) = unique_profile_names(&list[..kept], &list);
                let keys: HashSet<String> = names_of(&stored).into_iter().map(key).collect();
                assert_eq!(keys.len(), list.len(), "{picked:?} kept {kept}: {stored:?}");
                for (i, name) in picked.iter().enumerate() {
                    let alone = picked.iter().filter(|n| key(n) == key(name)).count() == 1;
                    if alone {
                        assert_eq!(names_of(&stored)[i], *name, "{picked:?} kept {kept}");
                    }
                }
            }
        }
    }

    /// The cases `src/utils/uniqueProfileName.test.ts` holds the frontend to.
    #[test]
    fn a_taken_name_gets_the_first_free_number() {
        let mut names = ProfileNames::new(["Prod", "prod (1)", "Lab"]);
        assert_eq!(names.claim("Staging"), "Staging");
        assert_eq!(names.claim("Prod"), "Prod (2)");
        assert_eq!(
            names.claim("PROD"),
            "PROD (3)",
            "names collide case-insensitively"
        );
        assert_eq!(
            names.claim("Staging"),
            "Staging (1)",
            "a batch collides with itself"
        );
        assert_eq!(names.claim("Prod (2)"), "Prod (2) (1)");
        assert_eq!(
            names.claim("lab "),
            "lab ",
            "a different name is left alone"
        );
    }

    /// An import onto an older list that holds two of one name: the write
    /// numbers the second old one first, so a name claimed for the import
    /// before the write ("Lab (1)") would select that old profile. The names
    /// reported come from the list stored, and each selects its own profile.
    #[test]
    fn the_renames_reported_are_the_names_stored() {
        let previous = [
            json!({"id": "a", "name": "Lab"}),
            json!({"id": "b", "name": "Lab"}),
        ];
        let sent = [
            previous[0].clone(),
            previous[1].clone(),
            json!({"id": "c", "name": "Lab"}),
        ];
        let (stored, _) = unique_profile_names(&previous, &sent);
        let renamed = renamed_by_write(&sent, &stored);
        assert_eq!(
            renamed,
            [
                RenamedProfile {
                    id: "b".into(),
                    from: "Lab".into(),
                    to: "Lab (1)".into()
                },
                RenamedProfile {
                    id: "c".into(),
                    from: "Lab".into(),
                    to: "Lab (2)".into()
                },
            ]
        );
        for r in &renamed {
            // `--profile NAME` takes the first profile of that name.
            let selected = stored
                .iter()
                .find(|p| p["name"].as_str().is_some_and(|n| key(n) == key(&r.to)));
            assert_eq!(selected.and_then(profile_id), Some(r.id.as_str()));
        }
        assert_eq!(name_of(&stored, "c"), Some("Lab (2)"));
        assert_eq!(name_of(&stored, "z"), None);
    }

    #[test]
    fn only_the_added_profiles_are_renamed_and_each_rename_is_reported() {
        let existing = [json!({"id": "a", "name": "Proton API"})];
        let mut names = ProfileNames::new(existing.iter().filter_map(profile_name));
        let mut added = vec![
            json!({"id": "b", "name": "Proton API"}),
            json!({"id": "c", "name": "Other"}),
            json!({"id": "d", "name": "Proton API"}),
            json!({"id": "e"}),
        ];
        let renamed = claim_profile_names(&mut names, &mut added);
        assert_eq!(existing[0]["name"], "Proton API");
        assert_eq!(added[0]["name"], "Proton API (1)");
        assert_eq!(added[1]["name"], "Other");
        assert_eq!(added[2]["name"], "Proton API (2)");
        assert!(added[3].get("name").is_none());
        assert_eq!(
            renamed,
            [
                RenamedProfile {
                    id: "b".into(),
                    from: "Proton API".into(),
                    to: "Proton API (1)".into()
                },
                RenamedProfile {
                    id: "d".into(),
                    from: "Proton API".into(),
                    to: "Proton API (2)".into()
                },
            ]
        );
    }
}
