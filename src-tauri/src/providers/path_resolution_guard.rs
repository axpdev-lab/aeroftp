//! Structural guard for the path resolution of Google Drive, Zoho WorkDrive
//! and OneDrive.
//!
//! These providers turn a path into a folder id in many places (25 in Drive,
//! 15 in Zoho). Until 2026-09-25 every site carried its own copy of the rule,
//! and the copies had drifted: a one-segment path went to the current folder
//! even with a leading slash, the Drive downloads sent it to the root, and a
//! longer relative path always went to the root. The rule now lives in one
//! `parent_folder_id` per provider (OneDrive: `parent_of_absolute`). The
//! sites are too many to drive one by one through a double, so this test is
//! what keeps them from diverging again: it reads the production source and
//! fails when a path is resolved outside the functions allowed to do it.
//!
//! It matches what a site has to contain, not how one is usually written:
//! `.resolve_path(` (rustfmt breaks a long call as `self\n    .resolve_path(`),
//! `current_folder_id` in any form (`&self.current_folder_id` borrows it
//! without `.clone()`), and `"root"` in any form (`"root".into()`).

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/// The production part of a provider source: everything before its test
/// module, which names the patterns on purpose.
fn production(source: &str) -> &str {
    source
        .split("\n#[cfg(test)]\nmod tests {")
        .next()
        .filter(|part| part.len() < source.len())
        .expect("the source has a test module to cut at")
}

/// The name of the function or struct a line opens, if it opens one: a
/// struct's fields belong to the struct, not to the function above it.
fn opened_fn(line: &str) -> Option<&str> {
    let mut rest = line.trim_start();
    for prefix in ["pub(crate) ", "pub(super) ", "pub ", "async "] {
        rest = rest.strip_prefix(prefix).unwrap_or(rest);
    }
    let rest = rest.strip_prefix("async ").unwrap_or(rest);
    let name = rest
        .strip_prefix("fn ")
        .or_else(|| rest.strip_prefix("struct "))?;
    let end = name.find(|c: char| !(c.is_alphanumeric() || c == '_'))?;
    Some(&name[..end])
}

/// Every `(line, function)` of `source` whose code (comments aside)
/// contains `needle`.
fn sites<'a>(source: &'a str, needle: &str) -> Vec<(usize, &'a str)> {
    let mut current = "";
    let mut found = Vec::new();
    for (index, line) in source.lines().enumerate() {
        if let Some(name) = opened_fn(line) {
            current = name;
        }
        if !line.trim_start().starts_with("//") && line.contains(needle) {
            found.push((index + 1, current));
        }
    }
    found
}

/// Fail on any `needle` outside `allowed`, after proving the guard sees
/// the needle where it belongs: an empty match must not pass for a clean one.
fn assert_confined(file: &str, source: &str, needle: &str, allowed: &[&str]) {
    let found = sites(production(source), needle);
    assert!(
        found.iter().any(|(_, function)| allowed.contains(function)),
        "{file}: `{needle}` not found in {allowed:?}; the guard is not reading the real source"
    );
    let stray: Vec<_> = found
        .into_iter()
        .filter(|(_, function)| !allowed.contains(function))
        .collect();
    assert!(
        stray.is_empty(),
        "{file}: `{needle}` outside {allowed:?}, resolve through parent_folder_id instead: {stray:?}"
    );
}

#[test]
fn google_drive_resolves_every_path_through_parent_folder_id() {
    let source = include_str!("google_drive.rs");
    assert_confined(
        "google_drive.rs",
        source,
        ".resolve_path(",
        &["parent_folder_id", "cd"],
    );
    assert_confined(
        "google_drive.rs",
        source,
        "current_folder_id",
        &[
            "GoogleDriveProvider",
            "new",
            "connect",
            "parent_folder_id",
            "list",
            "cd",
        ],
    );
    assert_confined(
        "google_drive.rs",
        source,
        "\"root\"",
        &["new", "connect", "resolve_path"],
    );
}

#[test]
fn zoho_workdrive_resolves_every_path_through_parent_folder_id() {
    let source = include_str!("zoho_workdrive.rs");
    assert_confined(
        "zoho_workdrive.rs",
        source,
        ".resolve_path(",
        &["parent_folder_id", "cd"],
    );
    assert_confined(
        "zoho_workdrive.rs",
        source,
        "current_folder_id",
        &[
            "ZohoWorkdriveProvider",
            "new",
            "rclone_root_folder_id_from_discovery",
            "rclone_root_folder_id_for_export",
            "discover_team",
            "discover_privatespace",
            "parent_folder_id",
            "resolve_path",
            "list",
            "cd",
        ],
    );
    assert_confined(
        "zoho_workdrive.rs",
        source,
        ".get(\"/\")",
        &["resolve_path"],
    );
}

/// OneDrive resolves at each site, and the parent of a path goes through
/// `parent_of_absolute`: the current folder stands in for a parent only for
/// a relative one-segment path, which `list` and `mkdir` handle and `pwd`
/// reports. Taking it as the parent anywhere else is the bug `rename` had
/// (a move of `/x` into the current folder read as a rename in place) and
/// `server_side_copy` had (a copy to `/x` landed in the current folder).
#[test]
fn onedrive_takes_the_current_folder_as_a_parent_only_where_allowed() {
    let source = include_str!("onedrive.rs");
    assert_confined(
        "onedrive.rs",
        source,
        "current_path.clone()",
        &["list", "pwd", "mkdir"],
    );
}

/// The forms that slipped past the literal needles the guard searched for
/// until 2026-09-26: a call rustfmt breaks after `self`, the field borrowed
/// instead of cloned, the root id built with `.into()`. The needles now in
/// use see each of them, in the function that holds it, and a comment or a
/// struct field is not a site.
#[test]
fn the_guard_sees_the_forms_the_literal_needles_missed() {
    let source = "fn allowed() {\n    self.resolve_path(path)\n}\n\
                  struct Provider {\n    current_folder_id: String,\n}\n\
                  fn stray() {\n    let id = self\n        .resolve_path(&a_long_path)\n        .await?;\n\
                  \x20   let here = &self.current_folder_id;\n\
                  \x20   let root: String = \"root\".into();\n\
                  \x20   // self.resolve_path( in a comment\n}\n\
                  \n#[cfg(test)]\nmod tests {\n}\n";
    let found_in = |needle: &str| -> Vec<&str> {
        sites(production(source), needle)
            .into_iter()
            .map(|(_, function)| function)
            .collect()
    };
    assert_eq!(found_in(".resolve_path("), ["allowed", "stray"]);
    assert_eq!(found_in("current_folder_id"), ["Provider", "stray"]);
    assert_eq!(found_in("\"root\""), ["stray"]);
    for missed in [
        "self.resolve_path(",
        "self.current_folder_id.clone()",
        "\"root\".to_string()",
    ] {
        assert!(
            !found_in(missed).contains(&"stray"),
            "{missed} was expected to miss the stray forms"
        );
    }
}

fn entry_names(entries: Vec<super::RemoteEntry>) -> Vec<String> {
    entries.into_iter().map(|entry| entry.name).collect()
}

fn failed(step: &'static str) -> impl Fn(super::ProviderError) -> String {
    move |e| format!("{step}: {e}")
}

/// After `cd /t/sub`: `/x` lands at the root and `x` in `/t/sub`, for mkdir
/// and for rename.
async fn cd_then_one_segment_paths(
    p: &mut dyn super::StorageProvider,
    t: &str,
) -> Result<(), String> {
    let sub = format!("/{t}/sub");
    p.mkdir(&format!("/{t}"))
        .await
        .map_err(failed("mkdir /t"))?;
    p.mkdir(&sub).await.map_err(failed("mkdir /t/sub"))?;
    p.cd(&sub).await.map_err(failed("cd /t/sub"))?;
    p.mkdir(&format!("/{t}-abs"))
        .await
        .map_err(failed("mkdir /t-abs"))?;
    p.mkdir("rel").await.map_err(failed("mkdir rel"))?;
    let in_sub = entry_names(p.list(&sub).await.map_err(failed("list /t/sub"))?);
    if !in_sub.contains(&"rel".to_string()) || in_sub.contains(&format!("{t}-abs")) {
        return Err(format!(
            "after cd, mkdir resolved wrongly: /t/sub holds {in_sub:?}"
        ));
    }
    p.rename("rel", &format!("/{t}-moved"))
        .await
        .map_err(failed("rename rel -> /t-moved"))?;
    let in_sub = entry_names(p.list(&sub).await.map_err(failed("list /t/sub"))?);
    let at_root = entry_names(p.list("/").await.map_err(failed("list /"))?);
    for name in [format!("{t}-abs"), format!("{t}-moved")] {
        if !at_root.contains(&name) {
            return Err(format!(
                "{name} is not at the root; /t/sub holds {in_sub:?}"
            ));
        }
    }
    if !in_sub.is_empty() {
        return Err(format!(
            "/t/sub should be empty after the move, holds {in_sub:?}"
        ));
    }
    Ok(())
}

/// Connect `p`, run the scenario under `/aeroftp-live-cd-<ms>`, and remove
/// every folder it created, pass or fail.
async fn run_live(mut p: Box<dyn super::StorageProvider>, label: &str) {
    p.connect()
        .await
        .unwrap_or_else(|e| panic!("{label}: connect: {e}"));
    let t = format!("aeroftp-live-cd-{}", chrono::Utc::now().timestamp_millis());
    let outcome = cd_then_one_segment_paths(p.as_mut(), &t).await;
    let _ = p.cd("/").await;
    for dir in [format!("/{t}"), format!("/{t}-abs"), format!("/{t}-moved")] {
        let _ = p.rmdir_recursive(&dir).await;
    }
    outcome.unwrap_or_else(|e| panic!("{label}: {e}"));
    eprintln!("LIVE {label}: after cd, /x lands at the root and x in the current folder");
}

/// Live, opt-in companion of the guard: the case the Google Drive, Zoho
/// WorkDrive and OneDrive fixes are about, in one connected session, on
/// saved OAuth profiles of the development vault. The providers are built as
/// the CLI builds them for an OAuth profile: the client config from the vault
/// and the tokens stored under the profile id, which is what
/// `AEROFTP_LIVE_CD_{GDRIVE,ZOHO,ONEDRIVE}_PROFILE_ID` name.
#[tokio::test]
#[ignore = "live: needs OAuth profiles in the development vault"]
async fn live_cd_then_one_segment_paths_resolve_like_every_provider() {
    use super::google_drive::{GoogleDriveConfig, GoogleDriveProvider};
    use super::onedrive::{OneDriveConfig, OneDriveProvider};
    use super::zoho_workdrive::{ZohoWorkdriveConfig, ZohoWorkdriveProvider};
    use crate::bridge_commands::resolve_oauth_client_config;
    use crate::credential_store::CredentialStore;

    assert_eq!(CredentialStore::init().expect("open the vault"), "OK");
    let store = CredentialStore::from_cache().expect("the vault is open");
    let mut ran = 0;
    if let Ok(pid) = std::env::var("AEROFTP_LIVE_CD_GDRIVE_PROFILE_ID") {
        let (id, secret) = resolve_oauth_client_config(&store, "googledrive");
        let config = GoogleDriveConfig::new(&id, &secret);
        run_live(
            Box::new(GoogleDriveProvider::new(config).with_profile_id(pid)),
            "Google Drive",
        )
        .await;
        ran += 1;
    }
    if let Ok(pid) = std::env::var("AEROFTP_LIVE_CD_ZOHO_PROFILE_ID") {
        let (id, secret) = resolve_oauth_client_config(&store, "zohoworkdrive");
        let region = store
            .get("oauth_zohoworkdrive_region")
            .unwrap_or_else(|_| "us".to_string());
        let config = ZohoWorkdriveConfig::new(&id, &secret, &region);
        run_live(
            Box::new(ZohoWorkdriveProvider::new(config).with_profile_id(pid)),
            "Zoho WorkDrive",
        )
        .await;
        ran += 1;
    }
    if let Ok(pid) = std::env::var("AEROFTP_LIVE_CD_ONEDRIVE_PROFILE_ID") {
        let (id, secret) = resolve_oauth_client_config(&store, "onedrive");
        let config = OneDriveConfig::new(&id, &secret);
        run_live(
            Box::new(OneDriveProvider::new(config).with_profile_id(pid)),
            "OneDrive",
        )
        .await;
        ran += 1;
    }
    assert!(ran > 0, "set at least one AEROFTP_LIVE_CD_*_PROFILE_ID");
}
