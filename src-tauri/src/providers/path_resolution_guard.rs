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
/// module, which names the patterns on purpose. A checkout with CRLF line
/// endings (Git for Windows' default) is cut at the same place.
fn production(source: &str) -> &str {
    let at = [
        "\n#[cfg(test)]\nmod tests {",
        "\r\n#[cfg(test)]\r\nmod tests {",
    ]
    .iter()
    .find_map(|marker| source.find(marker))
    .expect("the source has a test module to cut at");
    &source[..at]
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
    sites_where(source, |line| line.contains(needle))
}

/// Every `(line, function)` of `source` whose code (comments aside)
/// satisfies `matches`. A struct declared inside a function owns its fields
/// only up to its closing brace; the lines after it belong to the function
/// again (`resume_upload` in OneDrive declares one).
fn sites_where(source: &str, matches: impl Fn(&str) -> bool) -> Vec<(usize, &str)> {
    let mut current = "";
    // The indentation of an open struct's closing brace, and the function
    // to go back to after it.
    let mut open_struct: Option<(String, &str)> = None;
    let mut found = Vec::new();
    for (index, line) in source.lines().enumerate() {
        if open_struct
            .as_ref()
            .is_some_and(|(closing, _)| line == closing)
        {
            if let Some((_, outer)) = open_struct.take() {
                current = outer;
            }
            continue;
        }
        if let Some(name) = opened_fn(line) {
            let indent = &line[..line.len() - line.trim_start().len()];
            if line.contains("struct ") && line.trim_end().ends_with('{') && open_struct.is_none() {
                open_struct = Some((format!("{indent}}}"), current));
            }
            current = name;
        }
        if !line.trim_start().starts_with("//") && matches(line) {
            found.push((index + 1, current));
        }
    }
    found
}

/// Whether `line` uses `field` whole: not joined with a relative path
/// (`field.trim_end_matches('/')` followed by the rest of the path), not
/// assigned (`field =`), not declared (`field:`). A whole current folder in
/// place of a parent is the bug the OneDrive `rename` and `server_side_copy`
/// had.
fn uses_whole(line: &str, field: &str) -> bool {
    line.match_indices(field).any(|(at, _)| {
        let after = &line[at + field.len()..];
        !(after.starts_with(".trim_end_matches('/')")
            || after.starts_with(':')
            || after.starts_with(" ="))
    })
}

/// Fail on any line of `source` that uses `field` whole outside `allowed`,
/// after proving the guard sees such a use where it belongs.
fn assert_whole_uses_confined(file: &str, source: &str, field: &str, allowed: &[&str]) {
    let found = sites_where(production(source), |line| uses_whole(line, field));
    assert!(
        found.iter().any(|(_, function)| allowed.contains(function)),
        "{file}: no whole use of `{field}` found in {allowed:?}; the guard is not reading the real source"
    );
    let stray: Vec<_> = found
        .into_iter()
        .filter(|(_, function)| !allowed.contains(function))
        .collect();
    assert!(
        stray.is_empty(),
        "{file}: `{field}` used whole outside {allowed:?}, take the parent of an absolute path instead: {stray:?}"
    );
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

/// A Windows checkout with CRLF line endings has no LF-only marker: the cut
/// found none and the guard panicked before reading a line.
#[test]
fn a_crlf_checkout_is_cut_at_its_test_module() {
    assert_eq!(
        production("fn a() {}\r\n#[cfg(test)]\r\nmod tests {\r\n}\r\n"),
        "fn a() {}"
    );
    assert_eq!(
        production("fn a() {}\n#[cfg(test)]\nmod tests {\n}\n"),
        "fn a() {}"
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
///
/// Three needles, since the literal `current_path.clone()` missed every other
/// form: any use of `current_path` is confined to the functions that
/// resolve a relative path onto it (so a new function that touches it is
/// read before it passes), a whole use of it (a borrow, a copy, a
/// `.to_string()`, anything but the join of a relative path) to the ones
/// where the current folder really is the answer, and `current_item_id`, its
/// id, to the functions that keep it.
#[test]
fn onedrive_takes_the_current_folder_as_a_parent_only_where_allowed() {
    let source = include_str!("onedrive.rs");
    assert_confined(
        "onedrive.rs",
        source,
        "current_path",
        &[
            "OneDriveProvider",
            "new",
            "connect",
            "list",
            "pwd",
            "cd",
            "download",
            "resume_download",
            "download_to_bytes",
            "upload",
            "mkdir",
            "delete",
            "delete_permanent",
            "patch_into_place",
            "stat",
            "create_share_link",
            "server_side_copy",
            "resume_upload",
            "begin_multipart_upload",
            "copy_destination",
        ],
    );
    assert_whole_uses_confined(
        "onedrive.rs",
        source,
        "current_path",
        &["list", "pwd", "cd", "mkdir", "server_side_copy"],
    );
    assert_confined(
        "onedrive.rs",
        source,
        "current_item_id",
        &["OneDriveProvider", "new", "connect", "list", "cd"],
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
                  fn outer() {\n    struct Inner {\n        a: u8,\n    }\n\
                  \x20   let parent = self.current_path.to_string();\n\
                  \x20   let joined = format!(\"{}/x\", self.current_path.trim_end_matches('/'));\n}\n\
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
    // After a struct declared inside a function, the lines are the
    // function's again; only the `.to_string()` is a whole use.
    assert_eq!(found_in("current_path"), ["outer", "outer"]);
    let whole: Vec<&str> = sites_where(production(source), |line| uses_whole(line, "current_path"))
        .into_iter()
        .map(|(_, function)| function)
        .collect();
    assert_eq!(whole, ["outer"]);
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
