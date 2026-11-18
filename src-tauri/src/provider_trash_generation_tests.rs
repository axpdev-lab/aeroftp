// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Every GUI trash manager command acts only on the connection its dialog was
//! opened for (`provider_connection_generation`, read by App at the open).

use super::*;
use crate::providers::{ProviderError, ProviderType, RemoteEntry, StorageProvider};
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use tauri::Manager;

/// A provider that counts every time a command consults it. A trash command
/// reaches a service only through its provider, so a count of zero means no
/// request went out.
struct Counted(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl StorageProvider for Counted {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self.0.fetch_add(1, Ordering::SeqCst);
        self
    }
    fn provider_type(&self) -> ProviderType {
        self.0.fetch_add(1, Ordering::SeqCst);
        ProviderType::Sftp
    }
    fn display_name(&self) -> String {
        self.0.fetch_add(1, Ordering::SeqCst);
        "counted".into()
    }
    async fn connect(&mut self) -> Result<(), ProviderError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn is_connected(&self) -> bool {
        self.0.fetch_add(1, Ordering::SeqCst);
        true
    }
    async fn list(&mut self, _: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Vec::new())
    }
    async fn pwd(&mut self) -> Result<String, ProviderError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok("/".into())
    }
    async fn cd(&mut self, _: &str) -> Result<(), ProviderError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn download(
        &mut self,
        _: &str,
        _: &str,
        _: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn download_to_bytes(&mut self, _: &str) -> Result<Vec<u8>, ProviderError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Vec::new())
    }
    async fn upload(
        &mut self,
        _: &str,
        _: &str,
        _: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn mkdir(&mut self, _: &str) -> Result<(), ProviderError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn delete(&mut self, _: &str) -> Result<(), ProviderError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn rmdir(&mut self, _: &str) -> Result<(), ProviderError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn rmdir_recursive(&mut self, _: &str) -> Result<(), ProviderError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn rename(&mut self, _: &str, _: &str) -> Result<(), ProviderError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(ProviderError::NotFound(path.into()))
    }
    async fn size(&mut self, _: &str) -> Result<u64, ProviderError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(0)
    }
    async fn exists(&mut self, _: &str) -> Result<bool, ProviderError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(false)
    }
    async fn keep_alive(&mut self) -> Result<(), ProviderError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn server_info(&mut self) -> Result<String, ProviderError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(String::new())
    }
}

/// Run one trash command by name at `generation`, with harmless arguments.
/// Only the outcome kind matters: refused as another connection, or not.
async fn call(state: State<'_, ProviderState>, name: &str, generation: u64) -> Result<(), String> {
    let s = || "x".to_string();
    let v = || vec!["x".to_string()];
    macro_rules! go {
        ($e:expr) => {
            $e.await.map(|_| ())
        };
    }
    match name {
        "zoho_list_trash" => go!(zoho_list_trash(state, generation)),
        "zoho_permanent_delete" => go!(zoho_permanent_delete(state, v(), generation)),
        "zoho_restore_from_trash" => go!(zoho_restore_from_trash(state, v(), generation)),
        "jottacloud_list_trash" => go!(jottacloud_list_trash(state, generation)),
        "jottacloud_restore_from_trash" => {
            go!(jottacloud_restore_from_trash(state, v(), generation))
        }
        "jottacloud_permanent_delete" => go!(jottacloud_permanent_delete(state, v(), generation)),
        "jottacloud_empty_trash" => go!(jottacloud_empty_trash(state, generation)),
        "mega_list_trash" => go!(mega_list_trash(state, generation)),
        "mega_restore_from_trash" => go!(mega_restore_from_trash(state, v(), generation)),
        "mega_permanent_delete" => go!(mega_permanent_delete(state, v(), generation)),
        "google_drive_list_trash" => go!(google_drive_list_trash(state, generation)),
        "google_drive_restore_from_trash" => {
            go!(google_drive_restore_from_trash(state, v(), generation))
        }
        "google_drive_permanent_delete" => {
            go!(google_drive_permanent_delete(state, v(), generation))
        }
        "opendrive_list_trash" => go!(opendrive_list_trash(state, generation)),
        "opendrive_restore_from_trash" => {
            go!(opendrive_restore_from_trash(state, Vec::new(), generation))
        }
        "opendrive_permanent_delete" => {
            go!(opendrive_permanent_delete(state, Vec::new(), generation))
        }
        "opendrive_empty_trash" => go!(opendrive_empty_trash(state, generation)),
        "yandex_list_trash" => go!(yandex_list_trash(state, generation)),
        "yandex_restore_from_trash" => go!(yandex_restore_from_trash(state, v(), generation)),
        "yandex_permanent_delete" => go!(yandex_permanent_delete(state, v(), generation)),
        "yandex_empty_trash" => go!(yandex_empty_trash(state, generation)),
        "box_list_trash" => go!(box_list_trash(state, generation)),
        "box_restore_from_trash" => go!(box_restore_from_trash(state, s(), s(), generation)),
        "box_permanent_delete" => go!(box_permanent_delete(state, s(), s(), generation)),
        "filelu_list_deleted" => go!(filelu_list_deleted(state, generation)),
        "filelu_restore_file" => go!(filelu_restore_file(state, s(), generation)),
        "filelu_permanent_delete" => go!(filelu_permanent_delete(state, s(), generation)),
        "dropbox_list_trash" => go!(dropbox_list_trash(state, s(), generation)),
        "dropbox_restore_from_trash" => {
            go!(dropbox_restore_from_trash(state, s(), s(), generation))
        }
        "dropbox_permanent_delete" => go!(dropbox_permanent_delete(state, s(), generation)),
        "dropbox_account_type" => go!(dropbox_account_type(state, generation)),
        "onedrive_list_trash" => go!(onedrive_list_trash(state, generation)),
        "onedrive_restore_from_trash" => go!(onedrive_restore_from_trash(state, s(), generation)),
        "onedrive_permanent_delete" => go!(onedrive_permanent_delete(state, s(), generation)),
        "s3_list_trash" => go!(s3_list_trash(state, s(), false, generation)),
        "s3_restore_from_trash" => go!(s3_restore_from_trash(state, s(), s(), s(), generation)),
        "s3_empty_trash" => go!(s3_empty_trash(state, s(), false, true, generation)),
        "azure_list_deleted_blobs" => go!(azure_list_deleted_blobs(state, generation)),
        "azure_undelete_blob" => go!(azure_undelete_blob(state, Some(s()), None, generation)),
        "internxt_list_trash" => go!(internxt_list_trash(state, generation)),
        "pcloud_list_trash" => go!(pcloud_list_trash(state, generation)),
        "pcloud_restore_from_trash" => {
            go!(pcloud_restore_from_trash(state, s(), false, generation))
        }
        "pcloud_empty_trash" => go!(pcloud_empty_trash(state, generation)),
        "pcloud_permanently_delete_trash" => {
            go!(pcloud_permanently_delete_trash(
                state,
                s(),
                false,
                generation
            ))
        }
        "kdrive_list_trash" => go!(kdrive_list_trash(state, generation)),
        "kdrive_restore_from_trash" => go!(kdrive_restore_from_trash(state, s(), generation)),
        "kdrive_permanently_delete_trash" => {
            go!(kdrive_permanently_delete_trash(state, s(), generation))
        }
        "kdrive_empty_trash" => go!(kdrive_empty_trash(state, generation)),
        "b2_list_hidden" => go!(b2_list_hidden(state, s(), generation)),
        "b2_restore_hidden" => go!(b2_restore_hidden(state, s(), generation)),
        "b2_permanent_delete" => go!(b2_permanent_delete(state, s(), generation)),
        "koofr_list_trash" => go!(crate::providers::koofr::koofr_list_trash(state, generation)),
        "koofr_restore_trash" => go!(crate::providers::koofr::koofr_restore_trash(
            state,
            vec![(s(), s())],
            generation
        )),
        "koofr_empty_trash" => go!(crate::providers::koofr::koofr_empty_trash(
            state, generation
        )),
        other => panic!("no such trash command: {other}"),
    }
}

/// The trash manager commands the frontend sends, kept equal to the list the
/// generation wiring test reads in the managers.
const TRASH_COMMANDS: [&str; 54] = [
    "zoho_list_trash",
    "zoho_permanent_delete",
    "zoho_restore_from_trash",
    "jottacloud_list_trash",
    "jottacloud_restore_from_trash",
    "jottacloud_permanent_delete",
    "jottacloud_empty_trash",
    "mega_list_trash",
    "mega_restore_from_trash",
    "mega_permanent_delete",
    "google_drive_list_trash",
    "google_drive_restore_from_trash",
    "google_drive_permanent_delete",
    "opendrive_list_trash",
    "opendrive_restore_from_trash",
    "opendrive_permanent_delete",
    "opendrive_empty_trash",
    "yandex_list_trash",
    "yandex_restore_from_trash",
    "yandex_permanent_delete",
    "yandex_empty_trash",
    "box_list_trash",
    "box_restore_from_trash",
    "box_permanent_delete",
    "filelu_list_deleted",
    "filelu_restore_file",
    "filelu_permanent_delete",
    "dropbox_list_trash",
    "dropbox_restore_from_trash",
    "dropbox_permanent_delete",
    "dropbox_account_type",
    "onedrive_list_trash",
    "onedrive_restore_from_trash",
    "onedrive_permanent_delete",
    "s3_list_trash",
    "s3_restore_from_trash",
    "s3_empty_trash",
    "azure_list_deleted_blobs",
    "azure_undelete_blob",
    "internxt_list_trash",
    "pcloud_list_trash",
    "pcloud_restore_from_trash",
    "pcloud_empty_trash",
    "pcloud_permanently_delete_trash",
    "kdrive_list_trash",
    "kdrive_restore_from_trash",
    "kdrive_permanently_delete_trash",
    "kdrive_empty_trash",
    "b2_list_hidden",
    "b2_restore_hidden",
    "b2_permanent_delete",
    "koofr_list_trash",
    "koofr_restore_trash",
    "koofr_empty_trash",
];

/// A connect, disconnect or swap after the trash was opened: every command
/// is refused with the CONNECTION_CHANGED marker before it consults the
/// provider, so nothing reaches any service. At the generation that is still
/// live the same command gets through to the provider.
#[tokio::test]
async fn every_trash_command_refuses_another_connection_before_any_request() {
    let app = tauri::test::mock_app();
    app.manage(ProviderState::new());
    let touched = Arc::new(AtomicUsize::new(0));
    {
        let state = app.state::<ProviderState>();
        install_connected_provider(&state, Box::new(Counted(Arc::clone(&touched))), None, None)
            .await
            .unwrap();
    }
    let live = app
        .state::<ProviderState>()
        .connection_generation
        .load(Ordering::SeqCst);

    for name in TRASH_COMMANDS {
        touched.store(0, Ordering::SeqCst);
        let refused = call(app.state::<ProviderState>(), name, live + 1).await;
        assert!(
            matches!(&refused, Err(e) if e.starts_with(CONNECTION_CHANGED_MARKER)
                && e.contains("since the trash was opened")),
            "{name}: {refused:?}"
        );
        assert_eq!(
            touched.load(Ordering::SeqCst),
            0,
            "{name} consulted the provider of another connection"
        );

        let reached = call(app.state::<ProviderState>(), name, live).await;
        assert!(
            !matches!(&reached, Err(e) if e.starts_with(CONNECTION_CHANGED_MARKER)),
            "{name}: {reached:?}"
        );
        assert!(
            touched.load(Ordering::SeqCst) > 0,
            "{name} at the live generation never reached the provider"
        );
    }
}

/// The list above is every GUI trash manager command: each one that goes
/// through `provider_trash` takes a generation, none takes the provider lock
/// on its own.
#[test]
fn every_trash_manager_command_takes_the_generation() {
    for (file, source) in [
        ("provider_commands.rs", include_str!("provider_commands.rs")),
        ("providers/koofr.rs", include_str!("providers/koofr.rs")),
    ] {
        let mut at = 0;
        while let Some(found) = source[at..].find("#[tauri::command]") {
            let start = at + found;
            let end = source[start..]
                .find("\n}\n")
                .map_or(source.len(), |e| start + e);
            let command = &source[start..end];
            at = end;
            if !command.contains("crate::provider_trash::") {
                continue;
            }
            let name = command
                .split("pub async fn ")
                .nth(1)
                .and_then(|r| r.split('(').next())
                .unwrap_or("?");
            if name.starts_with("nextcloud_") || name.starts_with("webdav_") {
                // Nextcloud binds its generation in its own change.
                continue;
            }
            assert!(
                TRASH_COMMANDS.contains(&name),
                "{file}: {name} calls provider_trash but is not in TRASH_COMMANDS"
            );
            assert!(
                command.contains("generation: u64")
                    && command.contains(
                        "lock_connection_of_generation(generation, \"the trash was opened\")"
                    )
                    && !command.contains("state.provider.lock()"),
                "{file}: {name} does not lock its connection by generation"
            );
        }
    }
}
