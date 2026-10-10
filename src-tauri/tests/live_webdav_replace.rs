// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! What a WebDAV replace does on a real server, and what is left when its
//! move fails.
//!
//! RFC 4918 section 9.9.3 has a MOVE with `Overwrite: T` delete the
//! destination before it moves. Measured on 2026-10-10: Apache mod_dav and
//! golang.org/x/net/webdav unlink the destination and then rename, and a
//! rename that fails leaves no destination at all; Nextcloud sends it to the
//! trash first, and a MOVE whose source another client is reading fails
//! with 500 `LockedException` after that. `WebDavProvider::replace`
//! therefore sets the old file aside, moves the new one in, and only then
//! deletes the old one, and every MOVE it sends refuses a taken name.
//!
//! The first run replaces a file and checks that only the new content is
//! left, under the old name. The second, with
//! `AEROFTP_LIVE_WEBDAV_HOLD_SOURCE=1`, is for a server that locks a file
//! while it is read (Nextcloud): a GET of the staged file is held open while
//! the replace runs, the replace fails, and the old file must still be
//! under its name with nothing set aside left over. A server that does not
//! lock a file being read lets that replace through, which the run reports
//! as a fact about the server.
//!
//! ```bash
//! AEROFTP_LIVE_WEBDAV_URL=https://cloud.example.test/remote.php/dav/files/someone \
//! AEROFTP_LIVE_WEBDAV_USER=someone \
//! AEROFTP_LIVE_WEBDAV_PASS=... \
//!   cargo test --test live_webdav_replace -- --ignored --nocapture
//! ```
//!
//! The URL is the folder the paths start from, so a path appended to it is
//! the URL of that resource. The scratch folder is created under it and
//! removed at the end.

use ftp_client_gui_lib::providers::types::WebDavConfig;
use ftp_client_gui_lib::providers::webdav::WebDavProvider;
use ftp_client_gui_lib::providers::StorageProvider;
use secrecy::SecretString;

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

async fn put(provider: &mut WebDavProvider, path: &str, data: &[u8]) {
    let local = tempfile::NamedTempFile::new().expect("local file");
    std::fs::write(local.path(), data).expect("write local file");
    provider
        .upload(&local.path().to_string_lossy(), path, None)
        .await
        .unwrap_or_else(|e| panic!("upload {path}: {e}"));
}

async fn names(provider: &mut WebDavProvider, dir: &str) -> Vec<String> {
    let mut names: Vec<String> = provider
        .list(dir)
        .await
        .unwrap_or_else(|e| panic!("list {dir}: {e}"))
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    names.sort();
    names
}

#[tokio::test]
#[ignore = "needs a real WebDAV server; see the module comment"]
async fn a_replace_never_loses_the_file_it_replaces() {
    let (Some(url), Some(user), Some(pass)) = (
        env("AEROFTP_LIVE_WEBDAV_URL"),
        env("AEROFTP_LIVE_WEBDAV_USER"),
        env("AEROFTP_LIVE_WEBDAV_PASS"),
    ) else {
        panic!(
            "set AEROFTP_LIVE_WEBDAV_URL, AEROFTP_LIVE_WEBDAV_USER and AEROFTP_LIVE_WEBDAV_PASS"
        );
    };
    let mut provider = WebDavProvider::new(WebDavConfig {
        url: url.clone(),
        username: user.clone(),
        password: SecretString::from(pass.clone()),
        initial_path: None,
        provider_id: None,
        verify_cert: true,
        anonymous: false,
    })
    .expect("provider");
    provider.connect().await.expect("connect");

    assert!(!provider.supports_atomic_replace().await.expect("asked"));
    assert!(provider.replace_sets_aside());

    let unique = uuid::Uuid::new_v4().simple().to_string();
    let dir = format!("/aeroftp-live-webdav-replace-{}", &unique[..8]);
    provider.mkdir(&dir).await.expect("scratch folder");
    let (target, staged) = (format!("{dir}/html.txt"), format!("{dir}/html.txt.tmp"));

    // `html.txt`: set aside under a name that begins with `.ht`, which
    // Apache's default configuration refuses, the first MOVE failed.
    put(&mut provider, &target, b"old").await;
    put(&mut provider, &staged, b"new").await;
    provider.replace(&staged, &target).await.expect("replace");
    assert_eq!(
        provider.download_to_bytes(&target).await.expect("read"),
        b"new"
    );
    assert_eq!(names(&mut provider, &dir).await, ["html.txt"]);
    println!("replace over a file: new content under the old name, nothing else left");

    if env("AEROFTP_LIVE_WEBDAV_HOLD_SOURCE").as_deref() == Some("1") {
        // Large enough that the server is still sending it when the replace
        // runs: the reader takes one chunk and then holds the response.
        let big: Vec<u8> = (0..30_000_000u32).map(|i| (i % 251) as u8).collect();
        put(&mut provider, &staged, &big).await;
        let mut held = reqwest::Client::new()
            .get(format!("{}{staged}", url.trim_end_matches('/')))
            .basic_auth(&user, Some(&pass))
            .send()
            .await
            .expect("GET the staged file");
        assert!(held.status().is_success(), "{}", held.status());
        held.chunk().await.expect("first chunk");

        let outcome = provider.replace(&staged, &target).await;
        drop(held);
        match outcome {
            Err(e) => {
                println!("replace while the staged file is being read: {e}");
                assert_eq!(
                    provider.download_to_bytes(&target).await.expect("read"),
                    b"new",
                    "the file the failed replace was to replace is still there"
                );
                assert_eq!(
                    names(&mut provider, &dir).await,
                    ["html.txt", "html.txt.tmp"],
                    "nothing set aside is left over"
                );
            }
            Ok(()) => {
                println!("this server does not lock a file being read: the replace went through")
            }
        }
    }

    provider
        .rmdir_recursive(&dir)
        .await
        .expect("remove scratch folder");
}
