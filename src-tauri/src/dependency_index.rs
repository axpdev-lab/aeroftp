// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! The Dependencies panel: every crate AeroFTP depends on directly, at the
//! version it actually builds with, and what crates.io has that is newer.
//!
//! Nothing in the list is written by hand. `build.rs` reads the direct
//! dependencies of `Cargo.toml` and of every path crate it pulls in
//! (`peer-l0`), takes from `Cargo.lock` the version the manifest requirement
//! selects, and generates [`DEPENDENCIES`]. The hand-written list it replaces
//! had drifted in both directions: 17 direct dependencies were missing, and for
//! the crates locked in more than one version it showed the highest one in the
//! lock rather than ours, so `aes-gcm` read 0.11.0 while the manifest pins
//! `=0.10.3`.

use futures_util::stream::{self, StreamExt};
use semver::{Version, VersionReq};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::time::Duration;

/// One direct dependency, resolved at compile time by `build.rs`.
pub(crate) struct DependencyEntry {
    /// Package name on crates.io (never the `package =` alias).
    pub name: &'static str,
    /// The locked version the manifest requirement selects.
    pub version: &'static str,
    /// The requirement exactly as the manifest writes it.
    pub requirement: &'static str,
    pub category: &'static str,
}

include!(concat!(env!("OUT_DIR"), "/dependencies.rs"));

#[derive(Clone, serde::Serialize)]
pub struct DependencyInfo {
    name: String,
    version: String,
    requirement: String,
    category: String,
}

#[tauri::command]
pub fn get_dependencies() -> Vec<DependencyInfo> {
    DEPENDENCIES
        .iter()
        .map(|d| DependencyInfo {
            name: d.name.into(),
            version: d.version.into(),
            requirement: d.requirement.into(),
            category: d.category.into(),
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateStatus {
    /// The locked version is the newest stable release.
    UpToDate,
    /// A newer release fits the manifest requirement: `cargo update` reaches it.
    CompatibleUpdate,
    /// The newest release needs the manifest requirement changed.
    IncompatibleUpdate,
    /// An exact `=` requirement holds the crate back on purpose.
    Pinned,
    /// The index did not answer or the version could not be read. Deliberately
    /// not folded into `UpToDate`: an unanswered check is not a green one.
    Error,
}

#[derive(Clone, serde::Serialize)]
pub struct DependencyUpdate {
    name: String,
    version: String,
    latest: Option<String>,
    latest_compatible: Option<String>,
    status: UpdateStatus,
    error: Option<String>,
}

/// Path of a crate's file in the crates.io sparse index, per the registry
/// index layout documented in the Cargo book.
fn sparse_index_path(name: &str) -> String {
    let n = name.to_ascii_lowercase();
    match n.len() {
        1 => format!("1/{n}"),
        2 => format!("2/{n}"),
        3 => format!("3/{}/{n}", &n[..1]),
        _ => format!("{}/{}/{n}", &n[..2], &n[2..4]),
    }
}

#[derive(serde::Deserialize)]
struct IndexLine {
    vers: String,
    #[serde(default)]
    yanked: bool,
}

/// Stable, non-yanked releases listed in one sparse index file.
fn published_releases(index_file: &str) -> Vec<Version> {
    index_file
        .lines()
        .filter_map(|line| serde_json::from_str::<IndexLine>(line).ok())
        .filter(|entry| !entry.yanked)
        .filter_map(|entry| Version::parse(&entry.vers).ok())
        .filter(|version| version.pre.is_empty())
        .collect()
}

struct Assessment {
    latest: Option<Version>,
    latest_compatible: Option<Version>,
    status: UpdateStatus,
}

fn newest<'a>(releases: impl Iterator<Item = &'a Version>) -> Option<Version> {
    releases.max_by(|a, b| a.cmp_precedence(b)).cloned()
}

/// Compare a locked version against the published releases. Precedence
/// ignores build metadata, so `1.1.4+spec-1.1.0` is older than
/// `1.1.5+spec-1.1.0`, and `VersionReq` applies Cargo's caret rule, so a
/// minor bump under `0.x` is incompatible.
fn assess(locked: &str, requirement: &str, releases: &[Version]) -> Assessment {
    let latest = newest(releases.iter());
    let parsed_requirement = VersionReq::parse(requirement);
    let latest_compatible = parsed_requirement
        .as_ref()
        .ok()
        .and_then(|req| newest(releases.iter().filter(|v| req.matches(v))));
    // A requirement semver cannot read is an error, like an unanswered index:
    // without it a compatible update cannot be told from an incompatible one.
    let status = match (Version::parse(locked), &latest, &parsed_requirement) {
        (Ok(locked), Some(latest), Ok(_)) => {
            if locked.cmp_precedence(latest) != Ordering::Less {
                UpdateStatus::UpToDate
            } else if requirement.trim_start().starts_with('=') {
                UpdateStatus::Pinned
            } else if latest_compatible
                .as_ref()
                .is_some_and(|c| c.cmp_precedence(&locked) == Ordering::Greater)
            {
                UpdateStatus::CompatibleUpdate
            } else {
                UpdateStatus::IncompatibleUpdate
            }
        }
        _ => UpdateStatus::Error,
    };
    Assessment {
        latest,
        latest_compatible,
        status,
    }
}

const INDEX_BASE: &str = "https://index.crates.io";
const INDEX_CONCURRENCY: usize = 8;

async fn fetch_releases(client: &reqwest::Client, name: &str) -> Result<Vec<Version>, String> {
    let url = format!("{INDEX_BASE}/{}", sparse_index_path(name));
    let response = client.get(&url).send().await.map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }
    let body = response.text().await.map_err(|e| e.to_string())?;
    let releases = published_releases(&body);
    if releases.is_empty() {
        return Err("no stable release in the index".into());
    }
    Ok(releases)
}

/// Check every entry of [`DEPENDENCIES`] against the crates.io sparse index.
///
/// The sparse index is static files behind a CDN. The crates.io web API that
/// this used to call asks crawlers for at most one request per second, which a
/// 130-crate panel firing five at a time did not respect.
#[tauri::command]
pub async fn check_dependency_updates() -> Vec<DependencyUpdate> {
    let client = reqwest::Client::builder()
        .user_agent("AeroFTP (https://github.com/axpdev-lab/aeroftp)")
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let mut names: Vec<String> = DEPENDENCIES.iter().map(|d| d.name.to_string()).collect();
    names.sort_unstable();
    names.dedup();

    // Owned names and a client clone (an `Arc` inside) per future: a closure
    // over `&str` is typed for one caller-chosen lifetime, which is not general
    // enough for the Send bound `generate_handler!` puts on async commands.
    let fetched: HashMap<String, Result<Vec<Version>, String>> = stream::iter(names)
        .map(move |name| {
            let client = client.clone();
            async move {
                let releases = fetch_releases(&client, &name).await;
                (name, releases)
            }
        })
        .buffer_unordered(INDEX_CONCURRENCY)
        .collect()
        .await;

    DEPENDENCIES
        .iter()
        .map(|d| {
            let (assessment, error) = match fetched.get(d.name) {
                Some(Ok(releases)) => (assess(d.version, d.requirement, releases), None),
                Some(Err(e)) => (assess(d.version, d.requirement, &[]), Some(e.clone())),
                None => (
                    assess(d.version, d.requirement, &[]),
                    Some("not fetched".into()),
                ),
            };
            DependencyUpdate {
                name: d.name.into(),
                version: d.version.into(),
                latest: assessment.latest.map(|v| v.to_string()),
                latest_compatible: assessment.latest_compatible.map(|v| v.to_string()),
                status: assessment.status,
                error,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    fn releases(list: &[&str]) -> Vec<Version> {
        list.iter().map(|v| Version::parse(v).unwrap()).collect()
    }

    /// Direct dependencies of a manifest and of the path crates it pulls in,
    /// read here independently of `build.rs`, as `(package, requirement)`.
    /// Order does not matter to a set, so path crates are followed in place.
    fn manifest_dependencies(manifest: &Path, out: &mut BTreeSet<(String, String)>) {
        let text = std::fs::read_to_string(manifest).unwrap();
        let doc: toml::Table = text.parse().unwrap();
        let mut tables = Vec::new();
        if let Some(t) = doc.get("dependencies").and_then(|v| v.as_table()) {
            tables.push(t.clone());
        }
        if let Some(targets) = doc.get("target").and_then(|v| v.as_table()) {
            for target in targets.values() {
                if let Some(t) = target.get("dependencies").and_then(|v| v.as_table()) {
                    tables.push(t.clone());
                }
            }
        }
        for table in tables {
            for (key, value) in table {
                if let Some(path) = value.get("path").and_then(|p| p.as_str()) {
                    let child: PathBuf = manifest.parent().unwrap().join(path).join("Cargo.toml");
                    manifest_dependencies(&child, out);
                    continue;
                }
                let package = value
                    .get("package")
                    .and_then(|p| p.as_str())
                    .unwrap_or(&key)
                    .to_string();
                let requirement = value
                    .as_str()
                    .or_else(|| value.get("version").and_then(|v| v.as_str()))
                    .unwrap_or("*")
                    .to_string();
                out.insert((package, requirement));
            }
        }
    }

    /// A row stands for every declaration its version satisfies: `serde` is
    /// declared `1.0` by the app and `1` by `peer-l0`, and both resolve to one
    /// locked version, so the panel shows it once.
    #[test]
    fn the_list_covers_exactly_the_direct_dependencies_of_the_manifests() {
        let mut declared = BTreeSet::new();
        manifest_dependencies(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"),
            &mut declared,
        );
        let uncovered: Vec<_> = declared
            .iter()
            .filter(|(package, requirement)| {
                let req = VersionReq::parse(requirement).unwrap();
                !DEPENDENCIES.iter().any(|d| {
                    d.name == package && Version::parse(d.version).is_ok_and(|v| req.matches(&v))
                })
            })
            .collect();
        let declared_names: BTreeSet<&str> = declared.iter().map(|(p, _)| p.as_str()).collect();
        let undeclared: Vec<&str> = DEPENDENCIES
            .iter()
            .map(|d| d.name)
            .filter(|name| !declared_names.contains(name))
            .collect();
        assert!(
            uncovered.is_empty() && undeclared.is_empty(),
            "Dependencies panel out of sync with the manifests.\n\
             declared but not shown: {uncovered:?}\nshown but not declared: {undeclared:?}"
        );
    }

    #[test]
    fn every_entry_shows_the_version_its_requirement_selects() {
        for d in DEPENDENCIES {
            let req = VersionReq::parse(d.requirement)
                .unwrap_or_else(|e| panic!("{}: requirement {:?}: {e}", d.name, d.requirement));
            let version = Version::parse(d.version)
                .unwrap_or_else(|e| panic!("{}: locked version {:?}: {e}", d.name, d.version));
            assert!(
                req.matches(&version),
                "{} shows {} but the manifest asks for {}",
                d.name,
                d.version,
                d.requirement
            );
        }
    }

    #[test]
    fn build_metadata_does_not_hide_a_patch_release() {
        let a = assess(
            "1.1.4+spec-1.1.0",
            "1.1",
            &releases(&["1.1.4+spec-1.1.0", "1.1.5+spec-1.1.0"]),
        );
        assert_eq!(a.status, UpdateStatus::CompatibleUpdate);
        assert_eq!(a.latest_compatible.unwrap().to_string(), "1.1.5+spec-1.1.0");
    }

    #[test]
    fn a_minor_release_under_zero_major_is_incompatible() {
        let a = assess("0.6.0", "0.6", &releases(&["0.6.0", "0.7.0"]));
        assert_eq!(a.status, UpdateStatus::IncompatibleUpdate);
    }

    #[test]
    fn an_exact_pin_is_reported_as_pinned_not_as_an_update() {
        let a = assess("2.11.0", "=2.11.0", &releases(&["2.11.0", "2.11.5"]));
        assert_eq!(a.status, UpdateStatus::Pinned);
        assert_eq!(a.latest.unwrap().to_string(), "2.11.5");
        assert_eq!(a.latest_compatible.unwrap().to_string(), "2.11.0");
    }

    #[test]
    fn a_compatible_update_is_reported_even_when_a_newer_major_exists() {
        let a = assess("0.8.7", "0.8", &releases(&["0.8.7", "0.8.8", "0.10.2"]));
        assert_eq!(a.status, UpdateStatus::CompatibleUpdate);
        assert_eq!(a.latest.unwrap().to_string(), "0.10.2");
        assert_eq!(a.latest_compatible.unwrap().to_string(), "0.8.8");
    }

    #[test]
    fn pre_releases_and_yanked_versions_are_never_offered() {
        let index = [
            r#"{"vers":"1.0.0","yanked":false}"#,
            r#"{"vers":"1.1.0-rc.1","yanked":false}"#,
            r#"{"vers":"1.0.1","yanked":true}"#,
            "not json",
        ]
        .join("\n");
        let published = published_releases(&index);
        assert_eq!(published, releases(&["1.0.0"]));
        assert_eq!(
            assess("1.0.0", "1", &published).status,
            UpdateStatus::UpToDate
        );
    }

    #[test]
    fn an_unanswered_check_is_an_error_not_a_green() {
        assert_eq!(assess("1.0.0", "1", &[]).status, UpdateStatus::Error);
        assert_eq!(
            assess("unknown", "1", &releases(&["1.0.0"])).status,
            UpdateStatus::Error
        );
    }

    #[test]
    fn an_unreadable_requirement_is_an_error_not_an_incompatible_update() {
        let a = assess("1.0.0", "not a requirement", &releases(&["1.0.0", "2.0.0"]));
        assert_eq!(a.status, UpdateStatus::Error);
    }

    #[test]
    fn sparse_index_paths_follow_the_registry_layout() {
        assert_eq!(sparse_index_path("a"), "1/a");
        assert_eq!(sparse_index_path("cc"), "2/cc");
        assert_eq!(sparse_index_path("syn"), "3/s/syn");
        assert_eq!(sparse_index_path("Tauri"), "ta/ur/tauri");
    }
}
