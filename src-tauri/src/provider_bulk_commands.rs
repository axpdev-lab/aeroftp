// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! AeroSync's way to a provider's native multi-file transfer (Requested 77):
//! whether the connected provider has one, and a run of files through it
//! with one outcome per file. Grouping, settlement and Stop live in
//! [`crate::transfer_bulk`], the core the GUI batch commands use too.

use std::sync::Arc;

use tauri::{AppHandle, State};

use crate::provider_commands::{ProviderState, TransferOperationGuard};
use crate::transfer_bulk::{SyncBulkItem, SyncBulkResult};
use crate::transfer_event_sink::{AppHandleSink, TransferEventSink};
use crate::transfer_settings::{DEFAULT_TIMEOUT_SECONDS, MAX_TIMEOUT_SECONDS, MIN_TIMEOUT_SECONDS};

/// The directions the connected provider moves through its native bulk.
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize)]
pub struct BulkSupport {
    pub upload: bool,
    pub download: bool,
}

async fn bulk_support(state: &ProviderState) -> BulkSupport {
    let provider = state.provider.lock().await;
    provider
        .as_ref()
        .map(|provider| {
            let (upload, download) =
                crate::transfer_bulk::bulk_directions(&provider.transfer_capabilities());
            BulkSupport { upload, download }
        })
        .unwrap_or_default()
}

/// Whether the connected provider sends and receives several files in one
/// native call. AeroSync asks once when a run starts; a provider without
/// the bulk keeps the run on its single-file commands.
#[tauri::command]
pub async fn provider_bulk_support(state: State<'_, ProviderState>) -> Result<BulkSupport, String> {
    Ok(bulk_support(&state).await)
}

/// Send or receive the files of an AeroSync run that come in a row, through
/// the provider's native bulk: one call per folder, one outcome per file in
/// the order given. See [`crate::transfer_bulk::run_sync_bulk`] for what
/// each outcome means to the run.
#[tauri::command]
pub async fn provider_sync_transfer_bulk(
    app: AppHandle,
    state: State<'_, ProviderState>,
    upload: bool,
    items: Vec<SyncBulkItem>,
    timeout_seconds: Option<u64>,
) -> Result<Vec<SyncBulkResult>, String> {
    let sink: Arc<dyn TransferEventSink> = Arc::new(AppHandleSink::new(app));
    sync_transfer_bulk(&state, &sink, upload, &items, timeout_seconds).await
}

async fn sync_transfer_bulk(
    state: &ProviderState,
    sink: &Arc<dyn TransferEventSink>,
    upload: bool,
    items: &[SyncBulkItem],
    timeout_seconds: Option<u64>,
) -> Result<Vec<SyncBulkResult>, String> {
    if items.is_empty() {
        return Ok(Vec::new());
    }
    if upload {
        // The fail-closed guard of `provider_upload_file`, over every target.
        let targets: Vec<&str> = items.iter().map(|item| item.remote_path.as_str()).collect();
        state.guard_no_raw_crypt_write_outside("Upload", &targets)?;
    }
    let support = bulk_support(state).await;
    if !(if upload {
        support.upload
    } else {
        support.download
    }) {
        return Err(format!(
            "{} has no native bulk on this connection",
            if upload { "Upload" } else { "Download" }
        ));
    }
    // A disconnect or a session swap waits for this run, as for every
    // transfer that works on the shared provider.
    let _in_flight = TransferOperationGuard::acquire(state);
    // The live token, never a fresh one: see `run_sync_bulk`.
    let token = state.current_cancel_token().await;
    let timeout = timeout_seconds
        .unwrap_or(DEFAULT_TIMEOUT_SECONDS)
        .clamp(MIN_TIMEOUT_SECONDS, MAX_TIMEOUT_SECONDS);
    Ok(
        crate::transfer_bulk::run_sync_bulk(&state.provider, sink, items, upload, &token, timeout)
            .await,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::bulk::{BulkProgress, BulkTransferItem, BulkTransferOutcome};
    use crate::providers::{ProviderError, ProviderType, RemoteEntry, StorageProvider};
    use crate::transfer_event_sink::NoopTransferSink;
    use std::sync::atomic::Ordering;

    /// A bulk provider that completes every file and counts its calls.
    struct Bulk {
        calls: Arc<std::sync::Mutex<usize>>,
        bulk: bool,
    }

    #[async_trait::async_trait]
    impl StorageProvider for Bulk {
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
        fn provider_type(&self) -> ProviderType {
            ProviderType::Sftp
        }
        fn display_name(&self) -> String {
            "bulk".into()
        }
        async fn connect(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn disconnect(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        fn is_connected(&self) -> bool {
            true
        }
        async fn list(&mut self, _: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
            Ok(Vec::new())
        }
        async fn pwd(&mut self) -> Result<String, ProviderError> {
            Ok("/".into())
        }
        async fn cd(&mut self, _: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn cd_up(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn download(
            &mut self,
            _: &str,
            _: &str,
            _: Option<Box<dyn Fn(u64, u64) + Send>>,
        ) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn download_to_bytes(&mut self, _: &str) -> Result<Vec<u8>, ProviderError> {
            Ok(Vec::new())
        }
        async fn upload(
            &mut self,
            _: &str,
            _: &str,
            _: Option<Box<dyn Fn(u64, u64) + Send>>,
        ) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn mkdir(&mut self, _: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn delete(&mut self, _: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn rmdir(&mut self, _: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn rmdir_recursive(&mut self, _: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn rename(&mut self, _: &str, _: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
            Err(ProviderError::NotFound(path.into()))
        }
        async fn size(&mut self, _: &str) -> Result<u64, ProviderError> {
            Ok(0)
        }
        async fn exists(&mut self, _: &str) -> Result<bool, ProviderError> {
            Ok(false)
        }
        async fn keep_alive(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn server_info(&mut self) -> Result<String, ProviderError> {
            Ok(String::new())
        }
        fn supports_bulk_upload(&self) -> bool {
            self.bulk
        }
        fn supports_bulk_download(&self) -> bool {
            self.bulk
        }
        async fn upload_bulk(
            &mut self,
            items: &[BulkTransferItem],
            _: Option<BulkProgress>,
        ) -> Result<Vec<BulkTransferOutcome>, ProviderError> {
            *self.calls.lock().unwrap() += 1;
            Ok(items
                .iter()
                .map(|_| BulkTransferOutcome::Completed)
                .collect())
        }
    }

    fn connected(bulk: bool) -> (ProviderState, Arc<std::sync::Mutex<usize>>) {
        let calls = Arc::new(std::sync::Mutex::new(0));
        let state = ProviderState::new();
        *state.provider.try_lock().unwrap() = Some(Box::new(Bulk {
            calls: calls.clone(),
            bulk,
        }));
        (state, calls)
    }

    fn items() -> Vec<SyncBulkItem> {
        ["a.txt", "b.txt"]
            .iter()
            .map(|name| SyncBulkItem {
                local_path: format!("/tmp/sync-bulk-cmd/{name}"),
                remote_path: format!("/r/{name}"),
                size: 4,
                modified: None,
            })
            .collect()
    }

    async fn run(state: &ProviderState, upload: bool) -> Result<Vec<SyncBulkResult>, String> {
        let sink: Arc<dyn TransferEventSink> = Arc::new(NoopTransferSink);
        sync_transfer_bulk(state, &sink, upload, &items(), None).await
    }

    #[tokio::test]
    async fn the_support_follows_the_provider_and_a_disconnected_session_has_none() {
        let (state, _) = connected(true);
        assert_eq!(
            bulk_support(&state).await,
            BulkSupport {
                upload: true,
                download: true
            }
        );
        let (state, _) = connected(false);
        assert_eq!(bulk_support(&state).await, BulkSupport::default());
        assert!(
            run(&state, true).await.is_err(),
            "no bulk: refused, the run goes per file"
        );
        assert_eq!(
            bulk_support(&ProviderState::new()).await,
            BulkSupport::default()
        );
    }

    /// AeroSync resets the cancel state once, when its run starts. A Stop
    /// that came after it must still stop the bulk: a reset here, as the GUI
    /// batch commands do when they start, would have swallowed it.
    #[tokio::test]
    async fn a_stop_requested_before_the_call_is_kept() {
        let (state, calls) = connected(true);
        state.request_cancel().await;
        let results = run(&state, true).await.unwrap();
        assert_eq!(
            results,
            [SyncBulkResult::NotAttempted, SyncBulkResult::NotAttempted]
        );
        assert_eq!(*calls.lock().unwrap(), 0);
        assert!(state.current_cancel_token().await.is_cancelled());
    }

    /// The guard of `provider_upload_file`: an upload into a session whose
    /// crypt overlay is locked would write plaintext into the encrypted store.
    #[tokio::test]
    async fn an_upload_into_a_locked_crypt_session_is_refused() {
        let (state, calls) = connected(true);
        state.active_crypt_overlay.store(true, Ordering::SeqCst);
        state.overlay_wrapped.store(false, Ordering::SeqCst);
        let refused = run(&state, true).await.unwrap_err();
        assert!(refused.contains("Upload is blocked"), "{refused}");
        assert_eq!(*calls.lock().unwrap(), 0);
        let (state, calls) = connected(true);
        let sent = run(&state, true).await.unwrap();
        assert_eq!(sent, [SyncBulkResult::Completed, SyncBulkResult::Completed]);
        assert_eq!(*calls.lock().unwrap(), 1);
    }
}
