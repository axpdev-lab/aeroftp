// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Tests of the trash entry point that need no server. The per-provider
//! routes are pinned in each provider's own tests, against its mock server.

use super::*;
use std::collections::BTreeSet;

/// Every trash manager the GUI mounts, with the trash commands it invokes and
/// the provider the CLI and the agent must reach with the same operations.
/// `ops` is (restore, delete, empty) as the manager offers them.
type Manager = (
    &'static str,
    TrashKind,
    &'static [&'static str],
    (bool, bool, bool),
);

const MANAGERS: &[Manager] = &[
    (
        "AzureTrashManager",
        TrashKind::Azure,
        &["azure_list_deleted_blobs", "azure_undelete_blob"],
        (true, false, false),
    ),
    (
        "B2HiddenManager",
        TrashKind::B2,
        &["b2_list_hidden", "b2_restore_hidden", "b2_permanent_delete"],
        (true, true, false),
    ),
    (
        "BoxTrashManager",
        TrashKind::Box,
        &[
            "box_list_trash",
            "box_restore_from_trash",
            "box_permanent_delete",
        ],
        (true, true, false),
    ),
    // Empty is the manager's loop of permanent deletes.
    (
        "DropboxTrashManager",
        TrashKind::Dropbox,
        &[
            "dropbox_account_type",
            "dropbox_list_trash",
            "dropbox_restore_from_trash",
            "dropbox_permanent_delete",
        ],
        (true, true, true),
    ),
    // No Empty in the manager; the CLI had `trash empty` on FileLu before.
    (
        "FileLuTrashManager",
        TrashKind::FileLu,
        &[
            "filelu_list_deleted",
            "filelu_restore_file",
            "filelu_permanent_delete",
        ],
        (true, true, true),
    ),
    (
        "GoogleDriveTrashManager",
        TrashKind::GoogleDrive,
        &[
            "google_drive_list_trash",
            "google_drive_restore_from_trash",
            "google_drive_permanent_delete",
        ],
        (true, true, false),
    ),
    (
        "InternxtTrashManager",
        TrashKind::Internxt,
        &["internxt_list_trash"],
        (false, false, false),
    ),
    (
        "JottacloudTrashManager",
        TrashKind::Jottacloud,
        &[
            "jottacloud_list_trash",
            "jottacloud_restore_from_trash",
            "jottacloud_permanent_delete",
            "jottacloud_empty_trash",
        ],
        (true, true, true),
    ),
    (
        "KDriveTrashManager",
        TrashKind::KDrive,
        &[
            "kdrive_list_trash",
            "kdrive_restore_from_trash",
            "kdrive_permanently_delete_trash",
            "kdrive_empty_trash",
        ],
        (true, true, true),
    ),
    (
        "KoofrTrashManager",
        TrashKind::Koofr,
        &[
            "koofr_list_trash",
            "koofr_restore_trash",
            "koofr_empty_trash",
        ],
        (true, false, true),
    ),
    (
        "MegaTrashManager",
        TrashKind::Mega,
        &[
            "mega_list_trash",
            "mega_restore_from_trash",
            "mega_permanent_delete",
        ],
        (true, true, false),
    ),
    (
        "NextcloudTrashManager",
        TrashKind::Nextcloud,
        &[
            "webdav_list_trash",
            "webdav_restore_trash",
            "webdav_delete_trash",
            "webdav_empty_trash",
        ],
        (true, true, true),
    ),
    (
        "OneDriveTrashManager",
        TrashKind::OneDrive,
        &[
            "onedrive_list_trash",
            "onedrive_restore_from_trash",
            "onedrive_permanent_delete",
        ],
        (true, true, false),
    ),
    (
        "OpenDriveTrashManager",
        TrashKind::OpenDrive,
        &[
            "opendrive_list_trash",
            "opendrive_restore_from_trash",
            "opendrive_permanent_delete",
            "opendrive_empty_trash",
        ],
        (true, true, true),
    ),
    (
        "PCloudTrashManager",
        TrashKind::PCloud,
        &[
            "pcloud_list_trash",
            "pcloud_restore_from_trash",
            "pcloud_permanently_delete_trash",
            "pcloud_empty_trash",
        ],
        (true, true, true),
    ),
    // Purge is a restore mode of `s3_restore_from_trash`.
    (
        "S3TrashManager",
        TrashKind::S3,
        &["s3_list_trash", "s3_restore_from_trash", "s3_empty_trash"],
        (true, true, true),
    ),
    (
        "YandexTrashManager",
        TrashKind::Yandex,
        &[
            "yandex_list_trash",
            "yandex_restore_from_trash",
            "yandex_permanent_delete",
            "yandex_empty_trash",
        ],
        (true, true, true),
    ),
    (
        "ZohoTrashManager",
        TrashKind::Zoho,
        &[
            "zoho_list_trash",
            "zoho_restore_from_trash",
            "zoho_permanent_delete",
        ],
        (true, true, false),
    ),
];

fn repo_file(rel: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The `invoke('name'` / `invoke<T>('name'` command names in a TSX source.
fn invoked_commands(source: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut rest = source;
    while let Some(at) = rest.find("invoke") {
        rest = &rest[at + "invoke".len()..];
        let mut tail = rest;
        if tail.starts_with('<') {
            // Skip a type argument, nested angle brackets included.
            let mut depth = 0usize;
            let mut end = None;
            for (i, c) in tail.char_indices() {
                match c {
                    '<' => depth += 1,
                    '>' => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(i + 1);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            match end {
                Some(end) => tail = &tail[end..],
                None => continue,
            }
        }
        let Some(tail) = tail.strip_prefix('(') else {
            continue;
        };
        let Some(quote) = tail.chars().next().filter(|c| *c == '\'' || *c == '"') else {
            continue;
        };
        let tail = &tail[1..];
        if let Some(end) = tail.find(quote) {
            out.insert(tail[..end].to_string());
        }
    }
    out
}

/// The body of `pub async fn name(` in one of the command sources.
fn command_body(name: &str) -> String {
    for rel in [
        "src/provider_commands.rs",
        "src/providers/koofr.rs",
        "src/providers/webdav.rs",
    ] {
        let source = repo_file(rel);
        if let Some(at) = source.find(&format!("pub async fn {name}(")) {
            let end = source[at..].find("\n}\n").expect("command end") + at;
            return source[at..end].to_string();
        }
    }
    panic!("no Tauri command `{name}`");
}

/// The repo guard: a provider with a trash manager in the GUI is reachable
/// from the CLI `trash` command and the agent `remote_trash` tool, with the
/// operations the manager offers, and the manager's commands take the same
/// path as theirs.
#[test]
fn every_gui_trash_manager_is_reachable_from_the_cli_and_the_agent() {
    let app = repo_file("../src/App.tsx");
    let mounted: BTreeSet<String> = app
        .match_indices('<')
        .filter_map(|(i, _)| {
            let name: String = app[i + 1..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect();
            (name.ends_with("TrashManager") || name.ends_with("HiddenManager")).then_some(name)
        })
        .collect();
    let listed: BTreeSet<String> = MANAGERS.iter().map(|m| m.0.to_string()).collect();
    assert_eq!(
        mounted, listed,
        "a GUI trash manager is missing from MANAGERS (and so from the CLI and the agent), or one is gone"
    );

    let kinds: BTreeSet<_> = MANAGERS.iter().map(|m| m.1 as u8).collect();
    assert_eq!(
        kinds.len(),
        TrashKind::ALL.len(),
        "one TrashKind per manager"
    );

    for (component, kind, commands, (restore, delete, empty)) in MANAGERS {
        let source = repo_file(&format!("../src/components/{component}.tsx"));
        let invoked = invoked_commands(&source);
        let expected: BTreeSet<String> = commands.iter().map(|c| c.to_string()).collect();
        assert_eq!(
            invoked, expected,
            "{component} invokes other trash commands than listed"
        );
        assert_eq!(
            trash_caps(*kind),
            TrashCaps {
                restore: *restore,
                delete: *delete,
                empty: *empty
            },
            "{component}: the CLI and the agent offer other operations than the manager"
        );
        for command in *commands {
            assert!(
                command_body(command).contains("crate::provider_trash::"),
                "`{command}` ({component}) does not go through provider_trash, so the CLI and the agent can drift from it"
            );
        }
    }
}

#[test]
fn invoked_commands_reads_typed_and_untyped_calls() {
    let found = invoked_commands(
        "await invoke<Map<string, Array<number>>>('a_b', {}); invoke(\"c\"); invoke(name);",
    );
    assert_eq!(found, BTreeSet::from(["a_b".to_string(), "c".to_string()]));
}

#[test]
fn an_id_part_with_a_colon_round_trips() {
    let id = format!("m:{}:{}", esc("ver:%1"), "dir/a:b.txt");
    let (flag, rest) = split_id(TrashKind::S3, &id).unwrap();
    let (version, key) = split_id(TrashKind::S3, rest).unwrap();
    assert_eq!(
        (flag.as_str(), version.as_str(), key),
        ("m", "ver:%1", "dir/a:b.txt")
    );
}

#[test]
fn an_id_without_its_prefix_is_refused_as_an_id() {
    assert!(matches!(
        split_id(TrashKind::Box, "12345"),
        Err(TrashError::BadId(_))
    ));
    assert!(matches!(
        dir_flag(TrashKind::PCloud, "x"),
        Err(TrashError::BadId(_))
    ));
}

#[test]
fn a_refusal_says_what_the_trash_offers() {
    let message = not_offered(TrashKind::GoogleDrive, "empty").to_string();
    assert_eq!(
        message,
        "empty is not available on Google Drive: the Google Drive trash offers list, restore, delete, as in the app's trash manager"
    );
    assert!(not_offered(TrashKind::Internxt, "restore")
        .to_string()
        .ends_with("the Internxt trash offers list, as in the app's trash manager"));
}

fn internxt() -> crate::providers::internxt::InternxtProvider {
    crate::providers::internxt::InternxtProvider::new(crate::providers::types::InternxtConfig {
        email: "alice@example.com".to_string(),
        password: secrecy::SecretString::from("pw".to_string()),
        two_factor_code: None,
        initial_path: None,
    })
}

/// Internxt's manager only lists: restore, delete and empty are refused
/// before anything reaches the server.
#[tokio::test]
async fn an_operation_the_manager_lacks_is_refused_before_any_request() {
    let mut provider = internxt();
    for op in [
        TrashOp::Restore { id: "/a".into() },
        TrashOp::Delete { id: "/a".into() },
        TrashOp::Empty {
            path: "/".into(),
            dry_run: true,
        },
    ] {
        match run(&mut provider, op.clone()).await {
            Err(TrashError::NotOffered(m)) => assert!(m.contains("Internxt"), "{m}"),
            other => panic!("{op:?}: {other:?}"),
        }
    }
}

#[tokio::test]
async fn a_backend_without_a_trash_is_refused() {
    let config = crate::providers::ProviderConfig {
        name: "ftp".into(),
        provider_type: ProviderType::Ftp,
        host: "127.0.0.1".into(),
        port: Some(21),
        username: Some("u".into()),
        password: Some("p".into()),
        initial_path: None,
        extra: Default::default(),
    };
    let mut provider = crate::providers::ftp::FtpProvider::new(
        crate::providers::types::FtpConfig::from_provider_config(&config).expect("ftp config"),
    );
    assert_eq!(trash_kind(&mut provider), None);
    assert_eq!(
        run(&mut provider, TrashOp::List { path: "/".into() }).await,
        Err(TrashError::NoTrash)
    );
}
