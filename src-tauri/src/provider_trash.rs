// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! The server-side trash, the same path for the GUI, the CLI and the agent.
//!
//! The first half is one function per GUI trash command: the GUI's Tauri
//! command checks the provider type and calls it, so the code from the crypt
//! overlay peel down to the provider call is the same for every surface. Each
//! one keeps the GUI's crypt behaviour: it reaches the concrete provider
//! through [`concrete_provider_mut`], and decodes the display names exactly
//! where the GUI decodes them (never on MEGA, Nextcloud, FileLu, Azure or B2,
//! whose restore keys off the name or which the GUI shows raw).
//!
//! The second half, [`trash_kind`], [`trash_caps`] and [`run`], is what the
//! CLI `trash` command and the agent `remote_trash` tool call: one item shape
//! with an opaque `id` that `restore` and `delete` take back, and an op the GUI
//! does not offer on a provider refused with [`TrashError::NotOffered`].

use serde::Serialize;
use serde_json::{json, Map, Value};

use crate::crypt_overlay_provider::{
    concrete_provider_mut, decode_overlay_trash_keys, decode_overlay_trash_names,
};
use crate::providers::{ProviderType, RemoteEntry, StorageProvider, TrashEntry};

/// Why a trash call did not do what was asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrashError {
    /// The backend has no server-side trash the app reaches.
    NoTrash,
    /// The provider has a trash, but not this operation (the GUI does not
    /// offer it either). The sentence says what the provider offers.
    NotOffered(String),
    /// The id is not one `list` gives on this provider.
    BadId(String),
    /// The provider refused or failed: the message the GUI shows.
    Failed(String),
    /// An empty stopped part way: the whole sentence, with what already went.
    Partial(String),
}

impl std::fmt::Display for TrashError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TrashError::NoTrash => f.write_str(NO_TRASH),
            TrashError::NotOffered(s)
            | TrashError::BadId(s)
            | TrashError::Failed(s)
            | TrashError::Partial(s) => f.write_str(s),
        }
    }
}

impl std::error::Error for TrashError {}

impl From<String> for TrashError {
    fn from(message: String) -> Self {
        TrashError::Failed(message)
    }
}

/// The refusal for a backend outside [`TrashKind`].
pub const NO_TRASH: &str = "this backend has no server-side trash reachable from AeroFTP (the trash managers cover Nextcloud WebDAV, FileLu, Koofr, Google Drive, OneDrive, Box, Dropbox, MEGA, Zoho WorkDrive, Jottacloud, OpenDrive, Yandex Disk, pCloud, kDrive, Internxt, Azure Blob, S3 and Backblaze B2)";

type Res<T> = Result<T, TrashError>;

fn concrete<'a, T: 'static>(
    provider: &'a mut dyn StorageProvider,
    failure: &str,
) -> Res<&'a mut T> {
    concrete_provider_mut(provider)
        .as_any_mut()
        .downcast_mut::<T>()
        .ok_or_else(|| TrashError::Failed(failure.to_string()))
}

// ── Per-command functions: the GUI's Tauri commands call these ─────────────

use crate::providers::{
    azure::AzureProvider,
    box_provider::BoxProvider,
    dropbox::DropboxProvider,
    filelu::{DeletedFileEntry, FileLuProvider, TrashEmptied},
    google_drive::GoogleDriveProvider,
    internxt::InternxtProvider,
    jottacloud::{JottacloudProvider, TrashRestoreReport},
    kdrive::KDriveProvider,
    koofr::KoofrTrashEntry,
    mega::MegaCmdProvider,
    mega_native::MegaNativeProvider,
    onedrive::OneDriveProvider,
    opendrive::OpenDriveProvider,
    pcloud::PCloudProvider,
    webdav::NextcloudTrashEntry,
    webdav::WebDavProvider,
    yandex_disk::YandexDiskProvider,
    zoho_workdrive::ZohoWorkdriveProvider,
    B2Provider,
};

pub async fn zoho_list(p: &mut dyn StorageProvider) -> Res<Vec<RemoteEntry>> {
    let zoho = concrete::<ZohoWorkdriveProvider>(p, "Failed to access Zoho WorkDrive provider")?;
    let mut entries = zoho
        .list_trash()
        .await
        .map_err(|e| format!("Failed to list trash: {}", e))?;
    decode_overlay_trash_names(p, &mut entries);
    Ok(entries)
}

pub async fn zoho_restore(p: &mut dyn StorageProvider, file_ids: &[String]) -> Res<()> {
    let zoho = concrete::<ZohoWorkdriveProvider>(p, "Failed to access Zoho WorkDrive provider")?;
    if file_ids.len() == 1 {
        zoho.restore_from_trash(&file_ids[0])
            .await
            .map_err(|e| format!("Restore failed: {}", e).into())
    } else {
        zoho.restore_from_trash_batch(file_ids)
            .await
            .map_err(|e| format!("Restore batch failed: {}", e).into())
    }
}

pub async fn zoho_delete(p: &mut dyn StorageProvider, file_ids: &[String]) -> Res<()> {
    let zoho = concrete::<ZohoWorkdriveProvider>(p, "Failed to access Zoho WorkDrive provider")?;
    if file_ids.len() == 1 {
        zoho.permanent_delete(&file_ids[0])
            .await
            .map_err(|e| format!("Permanent delete failed: {}", e).into())
    } else {
        zoho.permanent_delete_batch(file_ids)
            .await
            .map_err(|e| format!("Permanent delete batch failed: {}", e).into())
    }
}

pub async fn jottacloud_list(p: &mut dyn StorageProvider) -> Res<Vec<RemoteEntry>> {
    let jotta = concrete::<JottacloudProvider>(p, "Failed to access Jottacloud provider")?;
    let mut entries = jotta
        .list_trash()
        .await
        .map_err(|e| format!("Failed to list trash: {}", e))?;
    decode_overlay_trash_names(p, &mut entries);
    Ok(entries)
}

pub async fn jottacloud_restore(
    p: &mut dyn StorageProvider,
    paths: &[String],
) -> Res<TrashRestoreReport> {
    let jotta = concrete::<JottacloudProvider>(p, "Failed to access Jottacloud provider")?;
    // Aggregate the per-entry reports so the UI shows what the server
    // actually confirmed, not what was selected (#397). A partial folder
    // restore arrives as Err carrying its confirmed counts; the remaining
    // selections are still attempted so one bad entry does not strand the
    // rest, then the combined failure is reported.
    let mut total = TrashRestoreReport::default();
    let mut errors: Vec<String> = Vec::new();
    for path in paths {
        match jotta.restore_from_trash(path).await {
            Ok(rep) => {
                total.files_restored += rep.files_restored;
                total.files_already_present += rep.files_already_present;
                total.dirs_restored += rep.dirs_restored;
            }
            Err(e) => errors.push(format!("Restore failed for {}: {}", path, e)),
        }
    }
    if !errors.is_empty() {
        return Err(TrashError::Failed(format!(
            "{} (overall: {} file(s) restored, {} already present, {} folder(s) revived)",
            errors.join("; "),
            total.files_restored,
            total.files_already_present,
            total.dirs_restored
        )));
    }
    Ok(total)
}

pub async fn jottacloud_delete(p: &mut dyn StorageProvider, paths: &[String]) -> Res<()> {
    let jotta = concrete::<JottacloudProvider>(p, "Failed to access Jottacloud provider")?;
    for path in paths {
        jotta
            .permanent_delete_from_trash(path)
            .await
            .map_err(|e| format!("Permanent delete failed for {}: {}", path, e))?;
    }
    Ok(())
}

/// Empty the whole Jottacloud trash (`files/v1/purge_trash`, rclone's
/// `cleanup`): the counts the server reports, files then folders.
pub async fn jottacloud_empty(p: &mut dyn StorageProvider) -> Res<(u64, u64)> {
    let jotta = concrete::<JottacloudProvider>(p, "Failed to access Jottacloud provider")?;
    jotta
        .empty_trash()
        .await
        .map_err(|e| format!("Empty trash failed: {}", e).into())
}

/// MEGA's rubbish bin, through the native API or MEGAcmd. Names stay as the
/// server sends them: restore keys off the name.
pub async fn mega_list(p: &mut dyn StorageProvider) -> Res<Vec<RemoteEntry>> {
    if let Some(native) = concrete_provider_mut(p)
        .as_any_mut()
        .downcast_mut::<MegaNativeProvider>()
    {
        return native
            .list_trash()
            .await
            .map_err(|e| format!("Failed to list trash: {}", e).into());
    }
    let mega = concrete::<MegaCmdProvider>(p, "Failed to access MEGA provider")?;
    mega.list_trash()
        .await
        .map_err(|e| format!("Failed to list trash: {}", e).into())
}

/// Restore into the session's current folder, as the GUI does.
pub async fn mega_restore(p: &mut dyn StorageProvider, filenames: &[String]) -> Res<()> {
    if let Some(native) = concrete_provider_mut(p)
        .as_any_mut()
        .downcast_mut::<MegaNativeProvider>()
    {
        let cwd = native.pwd().await.unwrap_or_else(|_| "/".to_string());
        for filename in filenames {
            native
                .restore_from_trash(filename, &cwd)
                .await
                .map_err(|e| format!("Restore failed for {}: {}", filename, e))?;
        }
        return Ok(());
    }
    let mega = concrete::<MegaCmdProvider>(p, "Failed to access MEGA provider")?;
    let cwd = mega.pwd().await.unwrap_or_else(|_| "/".to_string());
    for filename in filenames {
        mega.restore_from_trash(filename, &cwd)
            .await
            .map_err(|e| format!("Restore failed for {}: {}", filename, e))?;
    }
    Ok(())
}

pub async fn mega_delete(p: &mut dyn StorageProvider, filenames: &[String]) -> Res<()> {
    if let Some(native) = concrete_provider_mut(p)
        .as_any_mut()
        .downcast_mut::<MegaNativeProvider>()
    {
        for filename in filenames {
            native
                .permanent_delete_from_trash(filename)
                .await
                .map_err(|e| format!("Permanent delete failed for {}: {}", filename, e))?;
        }
        return Ok(());
    }
    let mega = concrete::<MegaCmdProvider>(p, "Failed to access MEGA provider")?;
    for filename in filenames {
        mega.permanent_delete_from_trash(filename)
            .await
            .map_err(|e| format!("Permanent delete failed for {}: {}", filename, e))?;
    }
    Ok(())
}

pub async fn google_drive_list(p: &mut dyn StorageProvider) -> Res<Vec<RemoteEntry>> {
    let gdrive = concrete::<GoogleDriveProvider>(p, "Failed to access Google Drive provider")?;
    let mut entries = gdrive
        .list_trash()
        .await
        .map_err(|e| format!("Failed to list trash: {}", e))?;
    decode_overlay_trash_names(p, &mut entries);
    Ok(entries)
}

pub async fn google_drive_restore(p: &mut dyn StorageProvider, file_ids: &[String]) -> Res<()> {
    let gdrive = concrete::<GoogleDriveProvider>(p, "Failed to access Google Drive provider")?;
    for file_id in file_ids {
        gdrive
            .restore_from_trash(file_id)
            .await
            .map_err(|e| format!("Restore failed for {}: {}", file_id, e))?;
    }
    Ok(())
}

pub async fn google_drive_delete(p: &mut dyn StorageProvider, file_ids: &[String]) -> Res<()> {
    let gdrive = concrete::<GoogleDriveProvider>(p, "Failed to access Google Drive provider")?;
    for file_id in file_ids {
        gdrive
            .permanent_delete(file_id)
            .await
            .map_err(|e| format!("Permanent delete failed for {}: {}", file_id, e))?;
    }
    Ok(())
}

pub async fn opendrive_list(p: &mut dyn StorageProvider) -> Res<Vec<RemoteEntry>> {
    let od = concrete::<OpenDriveProvider>(p, "Failed to access OpenDrive provider")?;
    let mut entries = od
        .list_trash()
        .await
        .map_err(|e| format!("Failed to list trash: {}", e))?;
    decode_overlay_trash_names(p, &mut entries);
    Ok(entries)
}

/// `items` are `(item_id, is_dir)`, as the GUI sends them.
pub async fn opendrive_restore(p: &mut dyn StorageProvider, items: &[(String, bool)]) -> Res<()> {
    let od = concrete::<OpenDriveProvider>(p, "Failed to access OpenDrive provider")?;
    for (item_id, is_dir) in items {
        od.restore_from_trash(item_id, *is_dir)
            .await
            .map_err(|e| format!("Restore failed for {}: {}", item_id, e))?;
    }
    Ok(())
}

pub async fn opendrive_delete(p: &mut dyn StorageProvider, items: &[(String, bool)]) -> Res<()> {
    let od = concrete::<OpenDriveProvider>(p, "Failed to access OpenDrive provider")?;
    for (item_id, is_dir) in items {
        od.permanent_delete_from_trash(item_id, *is_dir)
            .await
            .map_err(|e| format!("Permanent delete failed for {}: {}", item_id, e))?;
    }
    Ok(())
}

pub async fn opendrive_empty(p: &mut dyn StorageProvider) -> Res<()> {
    let od = concrete::<OpenDriveProvider>(p, "Failed to access OpenDrive provider")?;
    od.empty_trash()
        .await
        .map_err(|e| format!("Empty trash failed: {}", e).into())
}

pub async fn yandex_list(p: &mut dyn StorageProvider) -> Res<Vec<RemoteEntry>> {
    let yandex = concrete::<YandexDiskProvider>(p, "Failed to access Yandex Disk provider")?;
    let mut entries = yandex
        .list_trash()
        .await
        .map_err(|e| format!("Failed to list trash: {}", e))?;
    decode_overlay_trash_names(p, &mut entries);
    Ok(entries)
}

pub async fn yandex_restore(p: &mut dyn StorageProvider, paths: &[String]) -> Res<()> {
    let yandex = concrete::<YandexDiskProvider>(p, "Failed to access Yandex Disk provider")?;
    for path in paths {
        yandex
            .restore_from_trash(path)
            .await
            .map_err(|e| format!("Restore failed for {}: {}", path, e))?;
    }
    Ok(())
}

pub async fn yandex_delete(p: &mut dyn StorageProvider, paths: &[String]) -> Res<()> {
    let yandex = concrete::<YandexDiskProvider>(p, "Failed to access Yandex Disk provider")?;
    for path in paths {
        yandex
            .permanent_delete_from_trash(path)
            .await
            .map_err(|e| format!("Permanent delete failed for {}: {}", path, e))?;
    }
    Ok(())
}

pub async fn yandex_empty(p: &mut dyn StorageProvider) -> Res<()> {
    let yandex = concrete::<YandexDiskProvider>(p, "Failed to access Yandex Disk provider")?;
    yandex
        .empty_trash()
        .await
        .map_err(|e| format!("Empty trash failed: {}", e).into())
}

pub async fn box_list(p: &mut dyn StorageProvider) -> Res<Vec<RemoteEntry>> {
    let bx = concrete::<BoxProvider>(p, "Box downcast failed")?;
    let mut entries = bx
        .list_trash()
        .await
        .map_err(|e| format!("List trash failed: {}", e))?;
    decode_overlay_trash_names(p, &mut entries);
    Ok(entries)
}

pub async fn box_restore(p: &mut dyn StorageProvider, item_id: &str, item_type: &str) -> Res<()> {
    let bx = concrete::<BoxProvider>(p, "Box downcast failed")?;
    bx.restore_from_trash(item_id, item_type)
        .await
        .map_err(|e| format!("Restore failed: {}", e).into())
}

pub async fn box_delete(p: &mut dyn StorageProvider, item_id: &str, item_type: &str) -> Res<()> {
    let bx = concrete::<BoxProvider>(p, "Box downcast failed")?;
    bx.permanent_delete_from_trash(item_id, item_type)
        .await
        .map_err(|e| format!("Permanent delete failed: {}", e).into())
}

pub async fn filelu_list(p: &mut dyn StorageProvider) -> Res<Vec<DeletedFileEntry>> {
    let fl = concrete::<FileLuProvider>(p, "FileLu downcast failed")?;
    fl.list_deleted_files()
        .await
        .map_err(|e| e.to_string().into())
}

pub async fn filelu_restore(p: &mut dyn StorageProvider, file_code: &str) -> Res<()> {
    let fl = concrete::<FileLuProvider>(p, "FileLu downcast failed")?;
    fl.restore_deleted_file(file_code)
        .await
        .map_err(|e| e.to_string().into())
}

pub async fn filelu_delete(p: &mut dyn StorageProvider, file_code: &str) -> Res<()> {
    let fl = concrete::<FileLuProvider>(p, "FileLu downcast failed")?;
    fl.permanent_delete_file(file_code)
        .await
        .map_err(|e| e.to_string().into())
}

/// Empty the FileLu trash. The GUI has no Empty for FileLu; the CLI has had
/// one since before this module, so it stays. A stop part way fails with how
/// many items already went.
pub async fn filelu_empty(p: &mut dyn StorageProvider) -> Res<TrashEmptied> {
    let fl = concrete::<FileLuProvider>(p, "FileLu downcast failed")?;
    match fl.empty_trash().await {
        Ok(done) if done.left_without_code > 0 => Err(TrashError::Partial(format!(
            "trash not emptied: {} item(s) deleted, {} left because FileLu lists them without a file code; run `trash list` to see them",
            done.deleted, done.left_without_code
        ))),
        Ok(done) => Ok(done),
        Err((emptied, e)) => Err(TrashError::Partial(format!(
            "empty trash stopped after {emptied} item(s): {e}; run `trash list` to see what is left"
        ))),
    }
}

pub async fn dropbox_list(p: &mut dyn StorageProvider, path: &str) -> Res<Vec<RemoteEntry>> {
    let db = concrete::<DropboxProvider>(p, "Dropbox downcast failed")?;
    let mut entries = db.list_deleted(path).await.map_err(|e| e.to_string())?;
    decode_overlay_trash_names(p, &mut entries);
    Ok(entries)
}

pub async fn dropbox_restore(p: &mut dyn StorageProvider, path: &str, rev: &str) -> Res<()> {
    let db = concrete::<DropboxProvider>(p, "Dropbox downcast failed")?;
    db.restore_file(path, rev)
        .await
        .map_err(|e| e.to_string().into())
}

pub async fn dropbox_delete(p: &mut dyn StorageProvider, path: &str) -> Res<()> {
    let db = concrete::<DropboxProvider>(p, "Dropbox downcast failed")?;
    db.permanent_delete(path)
        .await
        .map_err(|e| e.to_string().into())
}

/// The Dropbox account tier ("basic" | "pro" | "business"). The GUI offers
/// Permanent Delete and Empty Trash on "business" only, because
/// `files/permanently_delete` is a Dropbox Business endpoint.
pub async fn dropbox_account_type(p: &mut dyn StorageProvider) -> Res<String> {
    let db = concrete::<DropboxProvider>(p, "Dropbox downcast failed")?;
    db.account_type().await.map_err(|e| e.to_string().into())
}

pub async fn onedrive_list(p: &mut dyn StorageProvider) -> Res<Vec<RemoteEntry>> {
    let od = concrete::<OneDriveProvider>(p, "OneDrive downcast failed")?;
    let mut entries = od.list_trash().await.map_err(|e| e.to_string())?;
    decode_overlay_trash_names(p, &mut entries);
    Ok(entries)
}

pub async fn onedrive_restore(p: &mut dyn StorageProvider, item_id: &str) -> Res<()> {
    let od = concrete::<OneDriveProvider>(p, "OneDrive downcast failed")?;
    od.restore_from_trash(item_id)
        .await
        .map_err(|e| e.to_string().into())
}

pub async fn onedrive_delete(p: &mut dyn StorageProvider, item_id: &str) -> Res<()> {
    let od = concrete::<OneDriveProvider>(p, "OneDrive downcast failed")?;
    od.permanent_delete(item_id)
        .await
        .map_err(|e| e.to_string().into())
}

pub async fn azure_list(p: &mut dyn StorageProvider) -> Res<Vec<RemoteEntry>> {
    let azure = concrete::<AzureProvider>(p, "Failed to access Azure provider")?;
    azure
        .list_deleted_blobs()
        .await
        .map_err(|e| e.to_string().into())
}

pub async fn azure_undelete(p: &mut dyn StorageProvider, path: &str) -> Res<()> {
    let azure = concrete::<AzureProvider>(p, "Failed to access Azure provider")?;
    // The deleted-blob listing names each blob from the container root, and
    // `undelete_blob` resolves a relative name against the current folder,
    // so the listed name goes in container-absolute.
    let absolute = format!("/{}", path.trim_start_matches('/'));
    azure
        .undelete_blob(&absolute)
        .await
        .map_err(|e| e.to_string().into())
}

pub async fn internxt_list(p: &mut dyn StorageProvider) -> Res<Vec<RemoteEntry>> {
    let internxt = concrete::<InternxtProvider>(p, "Failed to access Internxt provider")?;
    let mut entries = internxt.list_trash().await.map_err(|e| e.to_string())?;
    decode_overlay_trash_names(p, &mut entries);
    Ok(entries)
}

pub async fn pcloud_list(p: &mut dyn StorageProvider) -> Res<Vec<RemoteEntry>> {
    let pcloud = concrete::<PCloudProvider>(p, "Failed to access pCloud provider")?;
    let mut entries = pcloud.list_trash().await.map_err(|e| e.to_string())?;
    decode_overlay_trash_names(p, &mut entries);
    Ok(entries)
}

pub async fn pcloud_restore(p: &mut dyn StorageProvider, id: &str, is_folder: bool) -> Res<()> {
    let pcloud = concrete::<PCloudProvider>(p, "Failed to access pCloud provider")?;
    pcloud
        .restore_from_trash(id, is_folder)
        .await
        .map_err(|e| e.to_string().into())
}

pub async fn pcloud_delete(p: &mut dyn StorageProvider, id: &str, is_folder: bool) -> Res<()> {
    let pcloud = concrete::<PCloudProvider>(p, "Failed to access pCloud provider")?;
    pcloud
        .permanent_delete_from_trash(id, is_folder)
        .await
        .map_err(|e| e.to_string().into())
}

pub async fn pcloud_empty(p: &mut dyn StorageProvider) -> Res<()> {
    let pcloud = concrete::<PCloudProvider>(p, "Failed to access pCloud provider")?;
    pcloud.empty_trash().await.map_err(|e| e.to_string().into())
}

pub async fn kdrive_list(p: &mut dyn StorageProvider) -> Res<Vec<RemoteEntry>> {
    let kdrive = concrete::<KDriveProvider>(p, "Failed to access kDrive provider")?;
    let mut entries = kdrive.list_trash().await.map_err(|e| e.to_string())?;
    decode_overlay_trash_names(p, &mut entries);
    Ok(entries)
}

pub async fn kdrive_restore(p: &mut dyn StorageProvider, file_id: &str) -> Res<()> {
    let kdrive = concrete::<KDriveProvider>(p, "Failed to access kDrive provider")?;
    kdrive
        .restore_from_trash(file_id)
        .await
        .map_err(|e| e.to_string().into())
}

pub async fn kdrive_delete(p: &mut dyn StorageProvider, file_id: &str) -> Res<()> {
    let kdrive = concrete::<KDriveProvider>(p, "Failed to access kDrive provider")?;
    kdrive
        .permanently_delete_trash(file_id)
        .await
        .map_err(|e| e.to_string().into())
}

pub async fn kdrive_empty(p: &mut dyn StorageProvider) -> Res<()> {
    let kdrive = concrete::<KDriveProvider>(p, "Failed to access kDrive provider")?;
    kdrive.empty_trash().await.map_err(|e| e.to_string().into())
}

/// B2 native hide markers under `path`: a hidden file is B2's soft delete.
pub async fn b2_list(p: &mut dyn StorageProvider, path: &str) -> Res<Vec<RemoteEntry>> {
    let b2 = concrete::<B2Provider>(p, "B2 downcast failed")?;
    b2.list_hidden_files(path)
        .await
        .map_err(|e| e.to_string().into())
}

pub async fn b2_restore(p: &mut dyn StorageProvider, path: &str) -> Res<()> {
    let b2 = concrete::<B2Provider>(p, "B2 downcast failed")?;
    b2.restore_hidden_file(path)
        .await
        .map_err(|e| e.to_string().into())
}

/// Hard-delete every version of a B2 file. The number of versions purged.
pub async fn b2_delete(p: &mut dyn StorageProvider, path: &str) -> Res<u32> {
    let b2 = concrete::<B2Provider>(p, "B2 downcast failed")?;
    b2.permanent_delete_path(path)
        .await
        .map_err(|e| e.to_string().into())
}

pub async fn koofr_list(p: &mut dyn StorageProvider) -> Res<Vec<KoofrTrashEntry>> {
    crate::providers::koofr::trash_entries(p)
        .await
        .map_err(TrashError::Failed)
}

/// `files` are `(mount_id, path)` pairs, as the GUI sends them.
pub async fn koofr_restore(p: &mut dyn StorageProvider, files: Vec<(String, String)>) -> Res<()> {
    let koofr = concrete::<crate::providers::koofr::KoofrProvider>(p, "Not a Koofr connection")?;
    koofr
        .restore_from_trash(files)
        .await
        .map_err(|e| e.to_string().into())
}

pub async fn koofr_empty(p: &mut dyn StorageProvider) -> Res<()> {
    let koofr = concrete::<crate::providers::koofr::KoofrProvider>(p, "Not a Koofr connection")?;
    koofr.empty_trash().await.map_err(|e| e.to_string().into())
}

pub async fn nextcloud_list(p: &mut dyn StorageProvider) -> Res<Vec<NextcloudTrashEntry>> {
    let webdav = concrete::<WebDavProvider>(p, "Not a WebDAV connection")?;
    webdav
        .nextcloud_list_trash()
        .await
        .map_err(|e| e.to_string().into())
}

pub async fn nextcloud_restore(p: &mut dyn StorageProvider, ids: &[String]) -> Res<()> {
    let webdav = concrete::<WebDavProvider>(p, "Not a WebDAV connection")?;
    for id in ids {
        webdav
            .nextcloud_restore_trash(id)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub async fn nextcloud_delete(p: &mut dyn StorageProvider, ids: &[String]) -> Res<()> {
    let webdav = concrete::<WebDavProvider>(p, "Not a WebDAV connection")?;
    for id in ids {
        webdav
            .nextcloud_delete_trash_item(id)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub async fn nextcloud_empty(p: &mut dyn StorageProvider) -> Res<()> {
    let webdav = concrete::<WebDavProvider>(p, "Not a WebDAV connection")?;
    webdav
        .nextcloud_empty_trash()
        .await
        .map_err(|e| e.to_string().into())
}

/// The S3 soft-delete trash: every version and delete marker under `prefix`,
/// with the decrypted `display_key` filled when a crypt overlay is live.
pub async fn s3_list(
    p: &mut dyn StorageProvider,
    prefix: &str,
    include_noncurrent: bool,
) -> Res<Vec<TrashEntry>> {
    let mut entries = concrete_provider_mut(p)
        .list_object_versions(prefix, include_noncurrent)
        .await
        .map_err(|e| format!("List trash failed: {}", e))?;
    decode_overlay_trash_keys(p, &mut entries);
    Ok(entries)
}

/// One S3 trash row, by its raw `key` and `version_id`: `undelete` drops a
/// delete marker, `copy_forward` copies an older version forward, `purge`
/// removes that version or marker for good.
pub async fn s3_restore(
    p: &mut dyn StorageProvider,
    key: &str,
    version_id: &str,
    mode: &str,
) -> Res<()> {
    let s3 = concrete_provider_mut(p);
    match mode {
        "copy_forward" => s3
            .restore_version(key, version_id)
            .await
            .map_err(|e| format!("Restore from trash failed: {}", e).into()),
        "undelete" | "purge" => s3
            .delete_version(key, version_id)
            .await
            .map_err(|e| format!("Restore from trash failed: {}", e).into()),
        other => Err(TrashError::Failed(format!(
            "Unknown trash mode '{}' (expected 'undelete', 'copy_forward', or 'purge')",
            other
        ))),
    }
}

/// Empty the S3 trash under `prefix`: how many versions and markers, and
/// their bytes, went (or would go, with `dry_run`).
pub async fn s3_empty(
    p: &mut dyn StorageProvider,
    prefix: &str,
    include_noncurrent: bool,
    dry_run: bool,
) -> Res<(u64, u64)> {
    concrete_provider_mut(p)
        .empty_object_versions(prefix, include_noncurrent, dry_run)
        .await
        .map_err(|e| format!("Empty trash failed: {}", e).into())
}

// ── One entry point for the CLI and the agent ──────────────────────────────

/// A provider with a GUI trash manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TrashKind {
    Nextcloud,
    FileLu,
    Koofr,
    GoogleDrive,
    OneDrive,
    Box,
    Dropbox,
    Mega,
    Zoho,
    Jottacloud,
    OpenDrive,
    Yandex,
    PCloud,
    KDrive,
    Internxt,
    Azure,
    S3,
    B2,
}

impl TrashKind {
    pub const ALL: [TrashKind; 18] = [
        TrashKind::Nextcloud,
        TrashKind::FileLu,
        TrashKind::Koofr,
        TrashKind::GoogleDrive,
        TrashKind::OneDrive,
        TrashKind::Box,
        TrashKind::Dropbox,
        TrashKind::Mega,
        TrashKind::Zoho,
        TrashKind::Jottacloud,
        TrashKind::OpenDrive,
        TrashKind::Yandex,
        TrashKind::PCloud,
        TrashKind::KDrive,
        TrashKind::Internxt,
        TrashKind::Azure,
        TrashKind::S3,
        TrashKind::B2,
    ];

    pub fn label(self) -> &'static str {
        match self {
            TrashKind::Nextcloud => "Nextcloud",
            TrashKind::FileLu => "FileLu",
            TrashKind::Koofr => "Koofr",
            TrashKind::GoogleDrive => "Google Drive",
            TrashKind::OneDrive => "OneDrive",
            TrashKind::Box => "Box",
            TrashKind::Dropbox => "Dropbox",
            TrashKind::Mega => "MEGA",
            TrashKind::Zoho => "Zoho WorkDrive",
            TrashKind::Jottacloud => "Jottacloud",
            TrashKind::OpenDrive => "OpenDrive",
            TrashKind::Yandex => "Yandex Disk",
            TrashKind::PCloud => "pCloud",
            TrashKind::KDrive => "kDrive",
            TrashKind::Internxt => "Internxt",
            TrashKind::Azure => "Azure Blob",
            TrashKind::S3 => "S3",
            TrashKind::B2 => "Backblaze B2",
        }
    }
}

/// What a provider's trash offers besides listing: the GUI manager's buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TrashCaps {
    pub restore: bool,
    pub delete: bool,
    pub empty: bool,
}

pub const fn trash_caps(kind: TrashKind) -> TrashCaps {
    let (restore, delete, empty) = match kind {
        TrashKind::Nextcloud => (true, true, true),
        // No Empty in the GUI; the CLI had one before this module (kept).
        TrashKind::FileLu => (true, true, true),
        TrashKind::Koofr => (true, false, true),
        TrashKind::GoogleDrive => (true, true, false),
        TrashKind::OneDrive => (true, true, false),
        TrashKind::Box => (true, true, false),
        // Delete and Empty only on a Business account: checked at the call.
        TrashKind::Dropbox => (true, true, true),
        TrashKind::Mega => (true, true, false),
        TrashKind::Zoho => (true, true, false),
        TrashKind::Jottacloud => (true, true, true),
        TrashKind::OpenDrive => (true, true, true),
        TrashKind::Yandex => (true, true, true),
        TrashKind::PCloud => (true, true, true),
        TrashKind::KDrive => (true, true, true),
        TrashKind::Internxt => (false, false, false),
        TrashKind::Azure => (true, false, false),
        TrashKind::S3 => (true, true, true),
        TrashKind::B2 => (true, true, false),
    };
    TrashCaps {
        restore,
        delete,
        empty,
    }
}

/// The trash `provider` has, `None` for a backend without one the app reaches.
/// Looked up on the concrete provider under a crypt overlay, as the GUI does.
pub fn trash_kind(provider: &mut dyn StorageProvider) -> Option<TrashKind> {
    let concrete = concrete_provider_mut(provider);
    if concrete.provider_type() == ProviderType::S3 {
        return Some(TrashKind::S3);
    }
    let any = concrete.as_any_mut();
    Some(if any.is::<WebDavProvider>() {
        TrashKind::Nextcloud
    } else if any.is::<FileLuProvider>() {
        TrashKind::FileLu
    } else if any.is::<crate::providers::koofr::KoofrProvider>() {
        TrashKind::Koofr
    } else if any.is::<GoogleDriveProvider>() {
        TrashKind::GoogleDrive
    } else if any.is::<OneDriveProvider>() {
        TrashKind::OneDrive
    } else if any.is::<BoxProvider>() {
        TrashKind::Box
    } else if any.is::<DropboxProvider>() {
        TrashKind::Dropbox
    } else if any.is::<MegaNativeProvider>() || any.is::<MegaCmdProvider>() {
        TrashKind::Mega
    } else if any.is::<ZohoWorkdriveProvider>() {
        TrashKind::Zoho
    } else if any.is::<JottacloudProvider>() {
        TrashKind::Jottacloud
    } else if any.is::<OpenDriveProvider>() {
        TrashKind::OpenDrive
    } else if any.is::<YandexDiskProvider>() {
        TrashKind::Yandex
    } else if any.is::<PCloudProvider>() {
        TrashKind::PCloud
    } else if any.is::<KDriveProvider>() {
        TrashKind::KDrive
    } else if any.is::<InternxtProvider>() {
        TrashKind::Internxt
    } else if any.is::<AzureProvider>() {
        TrashKind::Azure
    } else if any.is::<B2Provider>() {
        TrashKind::B2
    } else {
        return None;
    })
}

/// Whether a provider of this type has a trash manager in the app other than
/// S3's (whose agent tool keeps its own version actions). A WebDAV server's is
/// the Nextcloud trash.
pub fn has_trash_manager(provider_type: ProviderType) -> bool {
    matches!(
        provider_type,
        ProviderType::WebDav
            | ProviderType::FileLu
            | ProviderType::Koofr
            | ProviderType::GoogleDrive
            | ProviderType::OneDrive
            | ProviderType::Box
            | ProviderType::Dropbox
            | ProviderType::Mega
            | ProviderType::ZohoWorkdrive
            | ProviderType::Jottacloud
            | ProviderType::OpenDrive
            | ProviderType::YandexDisk
            | ProviderType::PCloud
            | ProviderType::KDrive
            | ProviderType::Internxt
            | ProviderType::Azure
            | ProviderType::Backblaze
    )
}

/// One trash operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrashOp {
    /// List the trash. `path` scopes it where the GUI lists the current
    /// folder's deletions (Dropbox, B2); ignored elsewhere.
    List { path: String },
    /// Put the item back where it was.
    Restore { id: String },
    /// Delete the item for good.
    Delete { id: String },
    /// Delete everything in the trash for good, or with `dry_run` only say
    /// how much that is. `path` as for `List`.
    Empty { path: String, dry_run: bool },
}

/// The result of a [`TrashOp`].
#[derive(Debug, Clone, PartialEq)]
pub enum TrashOutcome {
    /// Each item is the row the GUI receives, with `id` (what restore and
    /// delete take) and `size` when the row has none of its own.
    Listed { kind: TrashKind, items: Vec<Value> },
    Restored {
        kind: TrashKind,
        id: String,
        detail: Option<Value>,
    },
    Deleted {
        kind: TrashKind,
        id: String,
        detail: Option<Value>,
    },
    /// `items`/`bytes`: what went (or would go), when known.
    Emptied {
        kind: TrashKind,
        dry_run: bool,
        items: Option<u64>,
        bytes: Option<u64>,
    },
}

/// The byte size of a listed item, when the row says.
pub fn item_size(item: &Value) -> Option<u64> {
    item.get("size").and_then(Value::as_u64)
}

fn offers(kind: TrashKind) -> String {
    let caps = trash_caps(kind);
    let mut ops = vec!["list"];
    if caps.restore {
        ops.push("restore");
    }
    if caps.delete {
        ops.push("delete");
    }
    if caps.empty {
        ops.push("empty");
    }
    format!("the {} trash offers {}", kind.label(), ops.join(", "))
}

fn not_offered(kind: TrashKind, op: &str) -> TrashError {
    TrashError::NotOffered(format!(
        "{op} is not available on {}: {}, as in the app's trash manager",
        kind.label(),
        offers(kind)
    ))
}

/// Escape `:` and `%` in an id part that comes before a `:` separator.
fn esc(part: &str) -> String {
    part.replace('%', "%25").replace(':', "%3A")
}

fn unesc(part: &str) -> String {
    part.replace("%3A", ":").replace("%25", "%")
}

/// Split an id `<head>:<rest>`, with `head` unescaped.
fn split_id(kind: TrashKind, id: &str) -> Res<(String, &str)> {
    id.split_once(':')
        .map(|(head, rest)| (unesc(head), rest))
        .ok_or_else(|| {
            TrashError::BadId(format!(
                "'{id}' is not a {} trash id: use an id as `trash list` shows it",
                kind.label()
            ))
        })
}

fn meta<'a>(entry: &'a RemoteEntry, key: &str) -> Option<&'a str> {
    entry
        .metadata
        .get(key)
        .map(String::as_str)
        .filter(|s| !s.is_empty())
}

/// A listed row: the GUI's row, plus the normalized fields it lacks.
fn row<T: Serialize>(native: &T, id: String, original_path: Option<&str>) -> Value {
    let mut obj = match serde_json::to_value(native) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    };
    obj.insert("id".into(), Value::String(id));
    if let Some(path) = original_path {
        obj.entry("original_path")
            .or_insert(Value::String(path.to_string()));
    }
    Value::Object(obj)
}

/// The id of a RemoteEntry row, as the GUI manager derives it.
fn entry_id(kind: TrashKind, e: &RemoteEntry) -> String {
    match kind {
        // `item.metadata?.id || item.path`
        TrashKind::GoogleDrive | TrashKind::OneDrive | TrashKind::Zoho => {
            meta(e, "id").unwrap_or(&e.path).to_string()
        }
        TrashKind::Box => {
            let id = meta(e, "id").unwrap_or(&e.path);
            let ty = meta(e, "item_type").unwrap_or(if e.is_dir { "folder" } else { "file" });
            format!("{}:{}", esc(ty), id)
        }
        TrashKind::PCloud => {
            // `item.metadata?.fileid ?? item.metadata?.folderid`, `isFolder: item.is_dir`
            let id = e
                .metadata
                .get("fileid")
                .or_else(|| e.metadata.get("folderid"))
                .cloned()
                .unwrap_or_default();
            format!("{}:{}", if e.is_dir { "d" } else { "f" }, id)
        }
        TrashKind::OpenDrive => {
            let id = meta(e, "opendrive_item_id").unwrap_or(&e.path);
            format!("{}:{}", if e.is_dir { "d" } else { "f" }, id)
        }
        // `item.metadata?.file_id ?? ''`
        TrashKind::KDrive => e.metadata.get("file_id").cloned().unwrap_or_default(),
        TrashKind::Mega => e.name.clone(),
        // Dropbox, Jottacloud, Yandex, Internxt, Azure, B2: the row's path.
        _ => e.path.clone(),
    }
}

fn entries(kind: TrashKind, list: &[RemoteEntry]) -> Vec<Value> {
    list.iter()
        .map(|e| row(e, entry_id(kind, e), None))
        .collect()
}

/// The S3 id: `m:` for a delete marker (restore drops it), `v:` for a
/// version (restore copies it forward), then the version id and the key.
fn s3_id(e: &TrashEntry) -> String {
    format!(
        "{}:{}:{}",
        if e.is_delete_marker { "m" } else { "v" },
        esc(&e.version_id),
        e.key
    )
}

async fn list(p: &mut dyn StorageProvider, kind: TrashKind, path: &str) -> Res<Vec<Value>> {
    Ok(match kind {
        TrashKind::Nextcloud => nextcloud_list(p)
            .await?
            .iter()
            .map(|e| row(e, e.id.clone(), None))
            .collect(),
        TrashKind::FileLu => filelu_list(p)
            .await?
            .iter()
            .map(|e| row(e, e.file_code.clone().unwrap_or_default(), None))
            .collect(),
        TrashKind::Koofr => koofr_list(p)
            .await?
            .iter()
            .map(|e| row(e, format!("{}:{}", esc(&e.mount_id), e.path), Some(&e.path)))
            .collect(),
        TrashKind::S3 => s3_list(p, "", false)
            .await?
            .iter()
            .map(|e| {
                let mut v = row(e, s3_id(e), None);
                if let Value::Object(map) = &mut v {
                    map.insert("name".into(), Value::String(e.display_key.clone()));
                    // The GUI offers Restore on a marker and on an older
                    // version; the current version is not in the trash.
                    map.insert(
                        "restorable".into(),
                        Value::Bool(e.is_delete_marker || !e.is_latest),
                    );
                }
                v
            })
            .collect(),
        TrashKind::GoogleDrive => entries(kind, &google_drive_list(p).await?),
        TrashKind::OneDrive => entries(kind, &onedrive_list(p).await?),
        TrashKind::Box => entries(kind, &box_list(p).await?),
        TrashKind::Dropbox => entries(kind, &dropbox_list(p, path).await?),
        TrashKind::Mega => entries(kind, &mega_list(p).await?),
        TrashKind::Zoho => entries(kind, &zoho_list(p).await?),
        TrashKind::Jottacloud => entries(kind, &jottacloud_list(p).await?),
        TrashKind::OpenDrive => entries(kind, &opendrive_list(p).await?),
        TrashKind::Yandex => entries(kind, &yandex_list(p).await?),
        TrashKind::PCloud => entries(kind, &pcloud_list(p).await?),
        TrashKind::KDrive => entries(kind, &kdrive_list(p).await?),
        TrashKind::Internxt => entries(kind, &internxt_list(p).await?),
        TrashKind::Azure => entries(kind, &azure_list(p).await?),
        TrashKind::B2 => entries(kind, &b2_list(p, path).await?),
    })
}

/// A Nextcloud trash id goes into the URL as one segment with its dots
/// encoded, and the URL parser still reads `%2E` and `%2E%2E` as `.` and
/// `..`: the request would land on the trash collection itself (a DELETE
/// there empties the whole trash) or on its parent. No listed id is all dots.
fn nextcloud_item(kind: TrashKind, id: &str) -> Res<String> {
    if id.bytes().all(|b| b == b'.') {
        return Err(TrashError::BadId(format!(
            "'{id}' is not a {} trash item: use an id as `trash list` shows it",
            kind.label()
        )));
    }
    Ok(id.to_string())
}

/// An id part these providers put into a request URL as it is: a number on
/// pCloud and kDrive. Anything else is refused before any request, so an
/// empty or crafted id can never reach, say, pCloud's `trash_clear` without
/// its item, which empties the whole trash.
fn numeric(kind: TrashKind, part: &str) -> Res<&str> {
    if !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()) {
        return Ok(part);
    }
    Err(TrashError::BadId(format!(
        "'{part}' is not a {} trash item: the item part of the id is a number",
        kind.label()
    )))
}

/// OpenDrive's and Box's item ids are letters, digits, `_`, `-` and `=`, and
/// go into the request path unencoded: nothing else gets through.
fn opaque(kind: TrashKind, part: &str) -> Res<&str> {
    if !part.is_empty()
        && part
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'='))
    {
        return Ok(part);
    }
    Err(TrashError::BadId(format!(
        "'{part}' is not a {} trash item id",
        kind.label()
    )))
}

fn dir_flag(kind: TrashKind, flag: &str) -> Res<bool> {
    match flag {
        "d" => Ok(true),
        "f" => Ok(false),
        _ => Err(TrashError::BadId(format!(
            "not a {} trash id: it starts with d: or f:",
            kind.label()
        ))),
    }
}

/// Refuse Delete and Empty on a Dropbox account that is not Business, as the
/// GUI disables them there.
async fn dropbox_purge_allowed(p: &mut dyn StorageProvider) -> Res<()> {
    let tier = dropbox_account_type(p).await?;
    if tier == "business" {
        return Ok(());
    }
    Err(TrashError::NotOffered(format!(
        "Dropbox deletes for good only on a Business account (this one is '{tier}'): its trash offers list and restore, as in the app's trash manager"
    )))
}

async fn restore(p: &mut dyn StorageProvider, kind: TrashKind, id: &str) -> Res<Option<Value>> {
    let one = [id.to_string()];
    match kind {
        TrashKind::Nextcloud => nextcloud_restore(p, &[nextcloud_item(kind, id)?]).await?,
        TrashKind::FileLu => filelu_restore(p, id).await?,
        TrashKind::Koofr => {
            let (mount, path) = split_id(kind, id)?;
            koofr_restore(p, vec![(mount, path.to_string())]).await?
        }
        TrashKind::GoogleDrive => google_drive_restore(p, &one).await?,
        TrashKind::OneDrive => onedrive_restore(p, id).await?,
        TrashKind::Box => {
            let (ty, item) = split_id(kind, id)?;
            box_restore(p, opaque(kind, item)?, &ty).await?
        }
        // The GUI sends the row's `rev`; `restore_file` does not read it.
        TrashKind::Dropbox => dropbox_restore(p, id, "").await?,
        TrashKind::Mega => mega_restore(p, &one).await?,
        TrashKind::Zoho => zoho_restore(p, &one).await?,
        TrashKind::Jottacloud => {
            let report = jottacloud_restore(p, &one).await?;
            return Ok(Some(json!({
                "files_restored": report.files_restored,
                "files_already_present": report.files_already_present,
                "dirs_restored": report.dirs_restored,
            })));
        }
        TrashKind::OpenDrive => {
            let (flag, item) = split_id(kind, id)?;
            let is_dir = dir_flag(kind, &flag)?;
            opendrive_restore(p, &[(opaque(kind, item)?.to_string(), is_dir)]).await?
        }
        TrashKind::Yandex => yandex_restore(p, &one).await?,
        TrashKind::PCloud => {
            let (flag, item) = split_id(kind, id)?;
            let is_dir = dir_flag(kind, &flag)?;
            pcloud_restore(p, numeric(kind, item)?, is_dir).await?
        }
        TrashKind::KDrive => kdrive_restore(p, numeric(kind, id)?).await?,
        TrashKind::Azure => azure_undelete(p, id).await?,
        TrashKind::B2 => b2_restore(p, id).await?,
        TrashKind::S3 => {
            let (flag, rest) = split_id(kind, id)?;
            let (version, key) = split_id(kind, rest)?;
            let mode = match flag.as_str() {
                "m" => "undelete",
                "v" => "copy_forward",
                _ => {
                    return Err(TrashError::BadId(
                        "not an S3 trash id: it starts with m: or v:".to_string(),
                    ))
                }
            };
            s3_restore(p, key, &version, mode).await?
        }
        TrashKind::Internxt => return Err(not_offered(kind, "restore")),
    }
    Ok(None)
}

async fn delete(p: &mut dyn StorageProvider, kind: TrashKind, id: &str) -> Res<Option<Value>> {
    let one = [id.to_string()];
    match kind {
        TrashKind::Nextcloud => nextcloud_delete(p, &[nextcloud_item(kind, id)?]).await?,
        TrashKind::FileLu => filelu_delete(p, id).await?,
        TrashKind::GoogleDrive => google_drive_delete(p, &one).await?,
        TrashKind::OneDrive => onedrive_delete(p, id).await?,
        TrashKind::Box => {
            let (ty, item) = split_id(kind, id)?;
            box_delete(p, opaque(kind, item)?, &ty).await?
        }
        TrashKind::Dropbox => {
            dropbox_purge_allowed(p).await?;
            dropbox_delete(p, id).await?
        }
        TrashKind::Mega => mega_delete(p, &one).await?,
        TrashKind::Zoho => zoho_delete(p, &one).await?,
        TrashKind::Jottacloud => jottacloud_delete(p, &one).await?,
        TrashKind::OpenDrive => {
            let (flag, item) = split_id(kind, id)?;
            let is_dir = dir_flag(kind, &flag)?;
            opendrive_delete(p, &[(opaque(kind, item)?.to_string(), is_dir)]).await?
        }
        TrashKind::Yandex => yandex_delete(p, &one).await?,
        TrashKind::PCloud => {
            let (flag, item) = split_id(kind, id)?;
            let is_dir = dir_flag(kind, &flag)?;
            pcloud_delete(p, numeric(kind, item)?, is_dir).await?
        }
        TrashKind::KDrive => kdrive_delete(p, numeric(kind, id)?).await?,
        TrashKind::B2 => {
            let versions = b2_delete(p, id).await?;
            return Ok(Some(json!({ "versions_purged": versions })));
        }
        TrashKind::S3 => {
            let (_flag, rest) = split_id(kind, id)?;
            let (version, key) = split_id(kind, rest)?;
            s3_restore(p, key, &version, "purge").await?
        }
        TrashKind::Koofr | TrashKind::Internxt | TrashKind::Azure => {
            return Err(not_offered(kind, "delete"))
        }
    }
    Ok(None)
}

fn totals(items: &[Value]) -> (u64, u64) {
    (
        items.len() as u64,
        items.iter().filter_map(item_size).sum::<u64>(),
    )
}

async fn empty(
    p: &mut dyn StorageProvider,
    kind: TrashKind,
    path: &str,
    dry_run: bool,
) -> Res<(Option<u64>, Option<u64>)> {
    if !trash_caps(kind).empty {
        return Err(not_offered(kind, "empty"));
    }
    if kind == TrashKind::S3 {
        let (count, bytes) = s3_empty(p, "", false, dry_run).await?;
        return Ok((Some(count), Some(bytes)));
    }
    if kind == TrashKind::Dropbox {
        dropbox_purge_allowed(p).await?;
    }
    // What is about to go, so the run is auditable and a dry run has an answer.
    let before = list(p, kind, path).await;
    if dry_run {
        let (n, bytes) = totals(&before?);
        return Ok((Some(n), Some(bytes)));
    }
    let before = before.ok();
    let (n, bytes) = match &before {
        Some(items) => (Some(totals(items).0), Some(totals(items).1)),
        None => (None, None),
    };
    match kind {
        TrashKind::Nextcloud => nextcloud_empty(p).await?,
        TrashKind::FileLu => {
            let done = filelu_empty(p).await?;
            return Ok((Some(done.deleted as u64), None));
        }
        TrashKind::Koofr => koofr_empty(p).await?,
        TrashKind::Jottacloud => {
            let (files, folders) = jottacloud_empty(p).await?;
            return Ok((Some(files + folders), bytes));
        }
        TrashKind::OpenDrive => opendrive_empty(p).await?,
        TrashKind::Yandex => yandex_empty(p).await?,
        TrashKind::PCloud => pcloud_empty(p).await?,
        TrashKind::KDrive => kdrive_empty(p).await?,
        TrashKind::Dropbox => {
            // The GUI's Empty: a permanent delete of every listed row.
            let items = before.ok_or_else(|| {
                TrashError::Failed("empty trash failed: the trash could not be listed".into())
            })?;
            for item in &items {
                let path = item.get("id").and_then(Value::as_str).unwrap_or_default();
                dropbox_delete(p, path).await?;
            }
        }
        _ => return Err(not_offered(kind, "empty")),
    }
    Ok((n, bytes))
}

/// Run `op` on the trash of `provider`. The CLI `trash` command and the agent
/// `remote_trash` tool come through here.
pub async fn run(provider: &mut dyn StorageProvider, op: TrashOp) -> Res<TrashOutcome> {
    let kind = trash_kind(provider).ok_or(TrashError::NoTrash)?;
    let caps = trash_caps(kind);
    if let TrashOp::Restore { id } | TrashOp::Delete { id } = &op {
        if id.trim().is_empty() {
            return Err(TrashError::BadId(format!(
                "an empty id is not a {} trash item: use an id as `trash list` shows it",
                kind.label()
            )));
        }
    }
    match op {
        TrashOp::List { path } => Ok(TrashOutcome::Listed {
            kind,
            items: list(provider, kind, &path).await?,
        }),
        TrashOp::Restore { id } => {
            if !caps.restore {
                return Err(not_offered(kind, "restore"));
            }
            let detail = restore(provider, kind, &id).await?;
            Ok(TrashOutcome::Restored { kind, id, detail })
        }
        TrashOp::Delete { id } => {
            if !caps.delete {
                return Err(not_offered(kind, "delete"));
            }
            let detail = delete(provider, kind, &id).await?;
            Ok(TrashOutcome::Deleted { kind, id, detail })
        }
        TrashOp::Empty { path, dry_run } => {
            let (items, bytes) = empty(provider, kind, &path, dry_run).await?;
            Ok(TrashOutcome::Emptied {
                kind,
                dry_run,
                items,
                bytes,
            })
        }
    }
}

#[cfg(test)]
#[path = "provider_trash_tests.rs"]
mod tests;
