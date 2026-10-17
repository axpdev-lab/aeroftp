//! Proton Drive, native Rust client (Tier 0, development only).
//!
//! This is the in-process successor to [`super::proton`], which drives
//! Proton's official `proton-drive` CLI. It is compiled only with the
//! `proton-native` feature, which is off by default and never shipped: the
//! CLI wrapper stays the production path until this module passes the gates
//! recorded in the roadmap appendix (`APPENDIX-PROTON-DRIVE/TODO.md`).
//!
//! Where the code comes from: the logic is ported from Proton's own Proton
//! Drive SDK (<https://github.com/ProtonDriveApps/sdk>, MIT, Copyright (c)
//! 2025-2026 Proton AG, see `LICENSE-PROTON-DRIVE-SDK.md` next to this file)
//! and every port names the SDK file and commit it follows. OpenPGP and key
//! handling use Proton's official `proton-crypto-account` crate with its pure
//! Rust backend. Nothing here is reverse engineered from network traffic or
//! from a third-party client.
//!
//! Rules this module must keep, from the SDK README "Usage Guidelines":
//! - only official endpoints (`drive-api.proton.me`, `account.proton.me`);
//! - an honest `x-pm-appversion` ([`app_version`]), and never
//!   `x-pm-drive-sdk-version`, which only the real SDK may send;
//! - retries, backoff and rate limiting no more aggressive than the SDK's
//!   ([`api`]);
//! - event-based sync, no polling and no frequent recursive traversals;
//! - no Proton branding, and a third-party disclosure wherever account
//!   details are asked for ([`THIRD_PARTY_DISCLOSURE`]).
//!
//! Sign-in is the SDK's browser session fork ([`auth`]): AeroFTP never sees
//! the account password.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

pub mod account;
pub mod api;
pub mod app_version;
pub mod auth;
pub mod client;
pub mod commands;
pub mod download;
pub mod drive_crypto;
pub mod error;
pub mod management;
pub mod nodes;
pub mod provider;
pub mod session;
pub mod upload;
pub mod vault;

pub use error::ProtonError;

/// Text the SDK README requires wherever a third-party app asks for Proton
/// account details. The UI shows it, translated, next to the sign-in button.
pub const THIRD_PARTY_DISCLOSURE: &str =
    "This is a third-party application not officially supported by Proton.";

/// The SDK commit the ports in this module follow. Bump it only together with
/// a re-read of the ported files at the new commit.
pub const SDK_REFERENCE_COMMIT: &str = "8ffae00";

#[cfg(test)]
mod live_tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    /// Live, on a real Proton account: browser sign-in through the session
    /// fork, unlock of the user and primary address keys, then revoke.
    ///
    /// Needs a person at the browser, on any device. Run:
    /// `cargo test --lib --features proton-native proton_native::live_tests -- --ignored --nocapture`
    /// and open the printed `PROTON_SIGN_IN_URL`. Optional
    /// `AEROFTP_PROTON_LIVE_HOLD_SECS=<n>` keeps the session open n seconds.
    #[tokio::test]
    #[ignore = "live: needs a person to sign in to Proton in the browser"]
    async fn live_browser_sign_in_unlocks_the_primary_address() {
        let api = api::ProtonApi::new(app_version::app_version());
        let pending = auth::begin_browser_sign_in(&api, auth::DEFAULT_ACCOUNT_HOST)
            .await
            .expect("session fork");
        // The fork is not tied to this machine: the page can be opened in a
        // browser on any device. The fragment holds a one-time key for this
        // fork only, useless without the selector that never leaves this
        // process, and the fork expires after ten minutes.
        eprintln!("{THIRD_PARTY_DISCLOSURE}");
        eprintln!("PROTON_SIGN_IN_URL {}", pending.sign_in_url());
        let cancel = CancellationToken::new();
        let session = auth::wait_for_browser_sign_in(&api, pending, &cancel)
            .await
            .expect("browser sign-in");
        let account = account::load_account(&api, &session.key_password)
            .await
            .expect("key unlock");
        let primary = account
            .primary_address()
            .expect("primary address keys unlocked");
        assert!(!primary.keys.is_empty());
        eprintln!(
            "unlocked {} user key(s) and {} address(es); primary address has {} key(s)",
            account.user_keys().len(),
            account.addresses().len(),
            primary.keys.len()
        );
        // `AEROFTP_PROTON_LIVE_HOLD_SECS` keeps the session open before the
        // revoke, so the tester can read how Proton lists it.
        let hold = std::env::var("AEROFTP_PROTON_LIVE_HOLD_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0);
        if hold > 0 {
            eprintln!("PROTON_SESSION_OPEN for {hold} s");
            tokio::time::sleep(std::time::Duration::from_secs(hold)).await;
        }
        auth::sign_out(&api).await.expect("revoke the test session");
        eprintln!("PROTON_SESSION_REVOKED");
    }

    /// Live, on a real Proton account: one browser sign-in, then the whole
    /// Drive round trip the provider needs, inside a fresh test folder in My
    /// Files. Uploaded files stay there (in `keep/`) so they can be opened in
    /// the official web and Android apps: that is the acceptance test for the
    /// upload format. Prints counts and hashes only, never file names.
    ///
    /// `cargo test --lib --features proton-native proton_native::live_tests::live_drive_round_trip -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "live: needs a person to sign in to Proton in the browser"]
    async fn live_drive_round_trip() {
        use futures_util::FutureExt;
        use sha1::{Digest, Sha1};
        use std::sync::Arc;

        let api = Arc::new(api::ProtonApi::new(app_version::app_version()));
        let pending = auth::begin_browser_sign_in(&api, auth::DEFAULT_ACCOUNT_HOST)
            .await
            .expect("session fork");
        eprintln!("{THIRD_PARTY_DISCLOSURE}");
        eprintln!("PROTON_SIGN_IN_URL {}", pending.sign_in_url());
        let cancel = CancellationToken::new();
        let session = auth::wait_for_browser_sign_in(&api, pending, &cancel)
            .await
            .expect("browser sign-in");
        let account = account::load_account(&api, &session.key_password)
            .await
            .expect("key unlock");
        let drive = client::DriveClient::open(Arc::clone(&api), account)
            .await
            .expect("My Files");

        // Every step runs inside `catch_unwind`, so the revoke below runs
        // even when a step fails.
        let outcome = std::panic::AssertUnwindSafe(async {
            let root = drive.root().await.expect("root");
            let listing = drive.children(&root).await.expect("root listing");
            let undecryptable = listing.iter().filter(|n| n.name.is_err()).count();
            eprintln!(
                "STEP root: {} item(s), {} with undecryptable names",
                listing.len(),
                undecryptable
            );

            let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
            let folder_name = format!("aeroftp-native-test-{stamp}");
            let folder_id = drive
                .create_folder(&root, &folder_name)
                .await
                .expect("mkdir");
            let folder = drive.node(&folder_id).await.expect("folder node");
            assert!(folder.is_folder() && folder.hash_key.is_some());
            eprintln!("STEP mkdir: ok");

            let keep_id = drive
                .create_folder(&folder, "keep")
                .await
                .expect("mkdir keep");
            let keep = drive.node(&keep_id).await.expect("keep node");

            // A small file and a multi-block one (9 MiB = 3 blocks).
            let small: Vec<u8> = b"AeroFTP native Proton Drive upload test\n".repeat(20);
            let mut big = vec![0u8; 9 * 1024 * 1024];
            rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut big);
            let mtime = chrono::DateTime::from_timestamp(1_760_000_000, 0);
            let mut uploaded = Vec::new();
            for (name, data) in [("small.txt", &small), ("big.bin", &big)] {
                let mut reader = &data[..];
                let id = drive
                    .upload_new_file(&keep, name, &mut reader, data.len() as u64, mtime, |_| {})
                    .await
                    .unwrap_or_else(|e| panic!("upload {name}: {e}"));
                uploaded.push((id, data.clone()));
            }
            eprintln!("STEP upload: 2 file(s)");

            for (id, data) in &uploaded {
                let node = drive.node(id).await.expect("uploaded node");
                assert_eq!(
                    node.size(),
                    data.len() as u64,
                    "size from extended attributes"
                );
                assert_eq!(
                    node.xattr.modification_time, mtime,
                    "mtime from extended attributes"
                );
                let mut out = Vec::new();
                let outcome = drive
                    .download(&node, &mut out, |_| {})
                    .await
                    .expect("download");
                assert_eq!(&out, data, "downloaded content differs");
                let expected = hex::encode(Sha1::digest(data));
                assert_eq!(outcome.sha1, expected);
                assert_eq!(node.xattr.sha1.as_deref(), Some(expected.as_str()));
                eprintln!(
                    "STEP download: {} bytes in {} block(s), sha1 ok",
                    outcome.bytes, outcome.blocks
                );
            }

            // Rename, subfolder, move, conflict, trash, permanent delete.
            let work_id = drive
                .create_folder(&folder, "work")
                .await
                .expect("mkdir work");
            let work = drive.node(&work_id).await.expect("work node");
            let mut reader = &small[..];
            let file_id = drive
                .upload_new_file(
                    &work,
                    "a.txt",
                    &mut reader,
                    small.len() as u64,
                    None,
                    |_| {},
                )
                .await
                .expect("upload a.txt");
            let mut reader = &small[..];
            let conflict = drive
                .upload_new_file(
                    &work,
                    "a.txt",
                    &mut reader,
                    small.len() as u64,
                    None,
                    |_| {},
                )
                .await;
            assert!(
                matches!(conflict, Err(ProtonError::AlreadyExists { .. })),
                "a second a.txt must be refused: {conflict:?}"
            );
            let file = drive.node(&file_id).await.expect("a.txt");
            drive.rename(&file, "b.txt").await.expect("rename");
            let renamed = drive.node(&file_id).await.expect("b.txt");
            assert_eq!(renamed.display_name(), Some("b.txt"));
            eprintln!("STEP rename: ok");

            let sub_id = drive.create_folder(&work, "sub").await.expect("mkdir sub");
            let sub = drive.node(&sub_id).await.expect("sub node");
            drive
                .move_node(&renamed, &sub, Some("c.txt"))
                .await
                .expect("move");
            let moved = drive.node(&file_id).await.expect("moved");
            assert_eq!(moved.parent_link_id.as_deref(), Some(sub_id.as_str()));
            assert_eq!(moved.display_name(), Some("c.txt"));
            let mut out = Vec::new();
            drive
                .download(&moved, &mut out, |_| {})
                .await
                .expect("download after move");
            assert_eq!(out, small, "content must survive the move");
            eprintln!("STEP move: ok");

            // Moving a folder keeps its children readable.
            let resolved = drive
                .resolve_path(&format!("/{folder_name}/work/sub/c.txt"))
                .await
                .expect("path resolution");
            assert_eq!(resolved.link_id, file_id);
            drive.move_node(&sub, &keep, None).await.expect("dirmove");
            drive.forget_all().await;
            let resolved = drive
                .resolve_path(&format!("/{folder_name}/keep/sub/c.txt"))
                .await
                .expect("path after dirmove");
            let mut out = Vec::new();
            drive
                .download(&resolved, &mut out, |_| {})
                .await
                .expect("download after dirmove");
            assert_eq!(out, small);
            eprintln!("STEP dirmove: ok");

            drive.trash(&[&work_id]).await.expect("trash");
            drive.delete_permanently(&[&work_id]).await.expect("delete");
            drive.forget_all().await;
            let folder = drive.node(&folder_id).await.expect("folder again");
            let left: Vec<_> = drive
                .children(&folder)
                .await
                .expect("folder listing")
                .iter()
                .filter_map(|n| n.display_name().map(str::to_string))
                .collect();
            assert_eq!(left, vec!["keep".to_string()], "work/ must be gone");
            eprintln!("STEP trash+delete: ok");

            let quota = drive.quota().await.expect("quota");
            assert!(quota.total > 0);
            eprintln!("STEP quota: used {} of {} bytes", quota.used, quota.total);

            eprintln!(
                "KEEP the test folder for the web and Android check: My Files/{folder_name}/keep (small.txt, big.bin, sub/c.txt)"
            );
        })
        .catch_unwind()
        .await;
        // The session is revoked whatever happened above, so a failed run
        // never leaves a live session behind.
        let revoked = auth::sign_out(&api).await;
        eprintln!("PROTON_SESSION_REVOKED {}", revoked.is_ok());
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
        revoked.expect("revoke the test session");
    }
}
