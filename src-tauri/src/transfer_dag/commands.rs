// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Tauri surface for the multipart resume store.
//!
//! The store has a hard cap, and the documented escape from it is
//! `forget_endpoint`. Until now that escape existed only on `aeroftp
//! checkpoints`, so a GUI-only user had no way to reach it: the cap could
//! silently drop their oldest resumable transfer and nothing offered to clear a
//! decommissioned server instead. A Tauri command for it was added during the
//! v4.1.8 audit and removed again in the pre-tag pass, correctly, because
//! nothing called it. This time the screen comes with it.
//!
//! `forget_endpoint` matches all four identity values exactly, so a listing is
//! not a convenience here, it is the only way to supply them. Both commands are
//! thin: the store is the single implementation, shared with the CLI.

use log::warn;
use serde::Serialize;

use super::TransferCheckpointStore;

/// One destination in the resume store, with how much it occupies.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointEndpoint {
    pub provider: String,
    pub protocol: String,
    pub host: String,
    pub account: String,
    pub records: usize,
    pub updated_unix_secs: u64,
}

/// Lists the destinations holding resume records, newest first.
///
/// Async because the work is a directory walk plus a JSON parse per record, and
/// a synchronous command runs on the GTK main thread: a store near its cap would
/// freeze the window while the settings panel opened.
#[tauri::command]
pub async fn checkpoint_endpoints() -> Result<Vec<CheckpointEndpoint>, String> {
    tokio::task::spawn_blocking(checkpoint_endpoints_blocking)
        .await
        .unwrap_or_else(|err| {
            warn!("checkpoint_endpoints blocking task failed: {err}");
            Err("Failed to read the resume store".to_string())
        })
}

fn checkpoint_endpoints_blocking() -> Result<Vec<CheckpointEndpoint>, String> {
    let store = TransferCheckpointStore::default_store()?;
    Ok(store
        .endpoints()?
        .into_iter()
        .map(|(id, records, updated_unix_secs)| CheckpointEndpoint {
            provider: id.provider,
            protocol: id.protocol,
            host: id.host,
            account: id.account,
            records,
            updated_unix_secs,
        })
        .collect())
}

/// Drops every resume record for one destination and reports how many went.
///
/// Zero removed is not an error: the end state asked for is already the case.
/// The caller distinguishes it so the UI can say nothing matched rather than
/// claiming a success the user cannot see.
/// Async for the same reason as the listing: it unlinks one file per record.
#[tauri::command]
pub async fn checkpoint_forget_endpoint(
    provider: String,
    protocol: String,
    host: String,
    account: String,
) -> Result<usize, String> {
    tokio::task::spawn_blocking(move || {
        let store = TransferCheckpointStore::default_store()?;
        store.forget_endpoint(&provider, &protocol, &host, &account)
    })
    .await
    .unwrap_or_else(|err| {
        warn!("checkpoint_forget_endpoint blocking task failed: {err}");
        Err("Failed to clear the resume records".to_string())
    })
}
