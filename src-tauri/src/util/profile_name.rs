// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! No two saved profiles share a name.
//!
//! Two profiles of one name cannot be told apart in the lists, and
//! `--profile NAME` takes the first. A profile created, edited, duplicated or
//! imported onto a name another profile holds therefore gets the first free
//! " (1)", " (2)", ...: [`unique_profile_names`] is applied to every write of a
//! profile list (`user_partitions::insert_profiles_with_dek`), whichever
//! window, command or import made it. Imports also claim names up front with
//! [`ProfileNames`], to report what they renamed. The frontend applies the
//! same rule (`src/utils/uniqueProfileName.ts`) only where it writes without
//! the partition layer (the legacy store).

use std::collections::HashSet;

/// What two names collide on: the CLI selects a profile by its name,
/// case-insensitive (`resolve_profile_selector`).
fn key(name: &str) -> String {
    name.to_lowercase()
}

/// The names in use, handing out names not yet taken.
pub struct ProfileNames {
    taken: HashSet<String>,
}

impl ProfileNames {
    /// The names `existing` already holds.
    pub fn new<'a>(existing: impl IntoIterator<Item = &'a str>) -> Self {
        Self {
            taken: existing.into_iter().map(key).collect(),
        }
    }

    /// The names of the profiles `existing` (JSON objects) already holds.
    pub fn of_profiles<'a>(existing: impl IntoIterator<Item = &'a serde_json::Value>) -> Self {
        Self::new(existing.into_iter().filter_map(profile_name))
    }

    /// `name` when no profile has it yet, else the first of "name (1)",
    /// "name (2)", ... that none has. The name returned is taken from then on.
    pub fn claim(&mut self, name: &str) -> String {
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

/// An imported profile that took another name than the one it came with.
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
pub fn claim_profile_names(
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
    // Settled before any rename: a profile renamed by the first pass must not
    // read as renamed by the user and be claimed again by the second.
    let keeps: Vec<bool> = profiles.iter().map(unchanged).collect();
    let mut stored = profiles.to_vec();
    let mut names = ProfileNames::new(std::iter::empty());
    let mut renamed = Vec::new();
    for pass in [true, false] {
        for (profile, _) in stored.iter_mut().zip(&keeps).filter(|(_, k)| **k == pass) {
            renamed.extend(claim_profile_names(
                &mut names,
                std::slice::from_mut(profile),
            ));
        }
    }
    (stored, renamed)
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

    #[test]
    fn only_the_imported_profiles_are_renamed_and_each_rename_is_reported() {
        let existing = [json!({"id": "a", "name": "Proton API"})];
        let mut names = ProfileNames::of_profiles(&existing);
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
